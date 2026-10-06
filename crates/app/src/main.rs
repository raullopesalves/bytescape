//! Bytescape desktop app: scan a folder or drive, then browse the result.
//!
//! The scan runs on a background thread and reports progress through the
//! `scan-progress` event, then `scan-done` or `scan-error`. The finished tree
//! stays here; the UI asks for one folder's children at a time, and for
//! treemap layouts as packed binary rectangles.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod basket;
mod drives;
mod duplicates;
mod elevation;
mod export;
mod scan_history;
mod search;
mod treemap;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use basket::{Basket, Level, Method};
use categories::{Cleanup, Rules};
use scanner::{flags, NodeId, NodeKind, Progress, ScanOptions, ScanTree};
use serde::Serialize;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

/// Name of the user rules file in the app's config folder.
const USER_RULES: &str = "categories.toml";

/// A finished scan the UI is browsing.
struct Session {
    tree: ScanTree,
    rules: Rules,
    /// Colour category per node for the treemap (see `treemap::dominant_categories`).
    dominant: Vec<u16>,
    categorise: Duration,
    rules_file: Option<String>,
    rules_error: Option<String>,
    /// The active search, if any.
    search: Option<SearchState>,
    /// Bumped by every search, so the UI can't page through a stale one.
    generation: u64,
    basket: Basket,
    /// This scan's snapshot in the history, once saved.
    history_id: Option<i64>,
    /// The last duplicate search of this scan.
    dupes: Option<duplicates::Found>,
}

struct SearchState {
    generation: u64,
    results: search::Results,
}

#[derive(Default)]
struct AppState {
    session: Mutex<Option<Session>>,
    /// The running scan's cancel flag; `None` when no scan is running.
    scanning: Mutex<Option<Arc<AtomicBool>>>,
    /// Scan history; `None` if the database couldn't be opened.
    history: Mutex<Option<history::History>>,
}

/// One node as the UI sees it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    id: NodeId,
    name: String,
    kind: &'static str,
    size: u64,
    alloc: u64,
    files: u64,
    modified: i64,
    category: u16,
    flags: u8,
    children: u32,
}

impl Row {
    fn new(tree: &ScanTree, id: NodeId) -> Row {
        let n = tree.node(id);
        Row {
            id,
            name: n.name.to_string(),
            kind: match n.kind {
                NodeKind::File => "file",
                NodeKind::Dir => "dir",
                NodeKind::Symlink => "link",
                NodeKind::Error => "error",
            },
            size: n.size,
            alloc: n.alloc,
            files: n.files,
            modified: n.modified,
            category: n.category,
            flags: n.flags,
            // Deleted entries don't count.
            children: tree
                .child_ids(id)
                .filter(|&c| !tree.node(c).has_flag(flags::REMOVED))
                .count() as u32,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressMsg {
    files: u64,
    dirs: u64,
    bytes: u64,
    elapsed_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CategoryRow {
    id: u16,
    key: String,
    name: String,
    color: String,
    cleanup: &'static str,
    files: u64,
    size: u64,
    alloc: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    root: Row,
    files: u64,
    dirs: u64,
    links: u64,
    errors: u64,
    hardlink_dups: u64,
    scan_ms: u64,
    categorise_ms: u64,
    /// Every category, in id order, with its totals.
    categories: Vec<CategoryRow>,
    rules_file: Option<String>,
    rules_error: Option<String>,
    /// "mft" or "walk".
    method: &'static str,
    /// Why the MFT was tried but not used.
    mft_skipped: Option<String>,
    /// A whole-drive scan that would be faster as administrator.
    admin_would_help: bool,
}

/// The folder given on the command line (`bytescape <path>`), if any.
#[tauri::command]
fn initial_path() -> Option<String> {
    std::env::args().nth(1)
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> Option<String> {
    let picked = app.dialog().file().blocking_pick_folder()?;
    picked.into_path().ok().map(|p| p.display().to_string())
}

#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>, path: String) -> Result<(), String> {
    let mut scanning = state.scanning.lock().unwrap();
    if scanning.is_some() {
        return Err("A scan is already running.".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *scanning = Some(cancel.clone());
    // Free the previous tree before building the next one.
    *state.session.lock().unwrap() = None;
    thread::spawn(move || run_scan(app, PathBuf::from(path), cancel));
    Ok(())
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    if let Some(cancel) = &*state.scanning.lock().unwrap() {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn summary(state: State<AppState>) -> Result<Summary, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let tree = &s.tree;
    let st = tree.stats();
    let categories = s
        .rules
        .totals(tree)
        .into_iter()
        .filter_map(|t| {
            let c = s.rules.category(t.category)?;
            Some(CategoryRow {
                id: t.category,
                key: c.id.clone(),
                name: c.name.clone(),
                color: c.color.clone(),
                cleanup: match c.cleanup {
                    Cleanup::Never => "never",
                    Cleanup::Review => "review",
                    Cleanup::Safe => "safe",
                },
                files: t.files,
                size: t.size,
                alloc: t.alloc,
            })
        })
        .collect();
    Ok(Summary {
        root: Row::new(tree, tree.root()),
        files: st.files,
        dirs: st.dirs,
        links: st.symlinks,
        errors: st.errors,
        hardlink_dups: st.hardlink_dups,
        scan_ms: st.elapsed.as_millis() as u64,
        categorise_ms: s.categorise.as_millis() as u64,
        categories,
        rules_file: s.rules_file.as_deref().map(with_appdata),
        rules_error: s.rules_error.clone(),
        method: match st.method {
            scanner::ScanMethod::Mft => "mft",
            _ => "walk",
        },
        mft_skipped: st.mft_skipped.clone(),
        admin_would_help: st.mft_needs_admin,
    })
}

/// Shows a path under the user's roaming app data as `%APPDATA%\...`:
/// shorter, and it doesn't show the user name.
fn with_appdata(path: &str) -> String {
    match std::env::var("APPDATA") {
        Ok(base) if !base.is_empty() && path.to_lowercase().starts_with(&base.to_lowercase()) => {
            format!("%APPDATA%{}", &path[base.len()..])
        }
        _ => path.to_string(),
    }
}

/// Starts the app again as administrator to scan `path` (Windows asks the
/// user first), then closes this one.
#[tauri::command]
fn relaunch_elevated(app: AppHandle, path: String) -> Result<(), String> {
    elevation::relaunch(&path, owner_window(&app))?;
    app.exit(0);
    Ok(())
}

/// Direct children of a folder, in name order.
#[tauri::command]
fn children(state: State<AppState>, id: NodeId) -> Result<Vec<Row>, String> {
    let guard = state.session.lock().unwrap();
    let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
    if id as usize >= tree.len() {
        return Err(format!("No node {id}."));
    }
    Ok(tree
        .child_ids(id)
        .filter(|&c| !tree.node(c).has_flag(flags::REMOVED))
        .map(|c| Row::new(tree, c))
        .collect())
}

#[tauri::command]
fn node_path(state: State<AppState>, id: NodeId) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
    if id as usize >= tree.len() {
        return Err(format!("No node {id}."));
    }
    Ok(tree.path(id).display().to_string())
}

/// One node and its full path, for tooltips.
#[derive(Serialize)]
struct Details {
    row: Row,
    path: String,
}

#[tauri::command]
fn node_details(state: State<AppState>, id: NodeId) -> Result<Details, String> {
    let guard = state.session.lock().unwrap();
    let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
    if id as usize >= tree.len() {
        return Err(format!("No node {id}."));
    }
    Ok(Details {
        row: Row::new(tree, id),
        path: tree.path(id).display().to_string(),
    })
}

/// The node and its ancestors, root first, as `(id, name)`.
#[tauri::command]
fn ancestors(state: State<AppState>, id: NodeId) -> Result<Vec<(NodeId, String)>, String> {
    let guard = state.session.lock().unwrap();
    let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
    if id as usize >= tree.len() {
        return Err(format!("No node {id}."));
    }
    let mut out = Vec::new();
    let mut cur = id;
    loop {
        out.push((cur, tree.node(cur).name.to_string()));
        if cur == tree.root() {
            break;
        }
        cur = tree.node(cur).parent;
    }
    out.reverse();
    Ok(out)
}

/// Treemap of the subtree under `id` for a `width` x `height` pixel canvas,
/// packed as described in `treemap::encode`.
#[tauri::command]
fn treemap(
    state: State<AppState>,
    id: NodeId,
    width: f64,
    height: f64,
    min_area: f64,
    pad: f64,
) -> Result<Response, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    if id as usize >= s.tree.len() {
        return Err(format!("No node {id}."));
    }
    let opts = treemap::Options {
        min_area: min_area.max(0.5),
        pad: pad.max(0.0),
        max_rects: 300_000,
    };
    let mut rects = treemap::layout(&s.tree, &s.dominant, id, width, height, &opts);
    if let Some(st) = &s.search {
        // Dim what doesn't match: files that aren't matches, and blocks
        // with no match inside.
        for r in &mut rects {
            let mark = st.results.marks[r.id as usize];
            let lit = match r.kind {
                treemap::FILE => mark & search::SELF != 0,
                _ => mark != 0,
            };
            if !lit {
                r.kind |= treemap::DIMMED;
            }
        }
    }
    Ok(Response::new(treemap::encode(&rects)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchSummary {
    generation: u64,
    count: usize,
    bytes: u64,
    millis: u64,
}

/// Runs a search and keeps its results for `search_page` and the treemap.
/// An empty filter clears the search.
#[tauri::command]
fn search(state: State<AppState>, filter: search::Filter) -> Result<SearchSummary, String> {
    let mut guard = state.session.lock().unwrap();
    let s = guard.as_mut().ok_or("No scan yet.")?;
    s.generation += 1;
    let generation = s.generation;
    if filter.is_empty() {
        s.search = None;
        return Ok(SearchSummary {
            generation,
            count: 0,
            bytes: 0,
            millis: 0,
        });
    }
    let t = Instant::now();
    let results = search::run(&s.tree, &filter);
    let summary = SearchSummary {
        generation,
        count: results.ids.len(),
        bytes: results.file_bytes,
        millis: t.elapsed().as_millis() as u64,
    };
    s.search = Some(SearchState {
        generation,
        results,
    });
    Ok(summary)
}

#[tauri::command]
fn clear_search(state: State<AppState>) {
    if let Some(s) = state.session.lock().unwrap().as_mut() {
        s.generation += 1;
        s.search = None;
    }
}

/// Matches `offset .. offset + limit` of search `generation`, largest first.
#[tauri::command]
fn search_page(
    state: State<AppState>,
    generation: u64,
    offset: usize,
    limit: usize,
) -> Result<Vec<Details>, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let st = s
        .search
        .as_ref()
        .filter(|st| st.generation == generation)
        .ok_or("That search is out of date.")?;
    let ids = st.results.ids.iter().skip(offset).take(limit.min(1000));
    Ok(ids
        .map(|&id| Details {
            row: Row::new(&s.tree, id),
            path: s.tree.path(id).display().to_string(),
        })
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BasketItem {
    id: NodeId,
    name: String,
    path: String,
    kind: &'static str,
    size: u64,
    level: Level,
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BasketView {
    items: Vec<BasketItem>,
    /// Items that can be deleted, and their bytes (repeated hardlinks count 0).
    count: usize,
    bytes: u64,
    blocked: usize,
    danger: usize,
}

fn basket_view_of(s: &Session) -> BasketView {
    let mut v = BasketView {
        items: Vec::new(),
        count: 0,
        bytes: 0,
        blocked: 0,
        danger: 0,
    };
    for &id in s.basket.items() {
        let a = basket::assess(&s.tree, &s.rules, &s.dominant, id);
        let row = Row::new(&s.tree, id);
        let size = effective_size(&s.tree, id);
        match a.level {
            Level::Blocked => v.blocked += 1,
            level => {
                v.count += 1;
                v.bytes += size;
                if level == Level::Danger {
                    v.danger += 1;
                }
            }
        }
        v.items.push(BasketItem {
            id,
            name: row.name,
            path: s.tree.path(id).display().to_string(),
            kind: row.kind,
            size: row.size,
            level: a.level,
            reason: a.reason,
        });
    }
    v
}

/// Bytes deleting `id` gives back (0 for a repeated hardlink).
fn effective_size(tree: &ScanTree, id: NodeId) -> u64 {
    let n = tree.node(id);
    if n.has_flag(flags::HARDLINK_DUP) {
        0
    } else {
        n.size
    }
}

fn with_basket(
    state: &State<AppState>,
    f: impl FnOnce(&mut Session),
) -> Result<BasketView, String> {
    let mut guard = state.session.lock().unwrap();
    let s = guard.as_mut().ok_or("No scan yet.")?;
    f(s);
    Ok(basket_view_of(s))
}

#[tauri::command]
fn basket_view(state: State<AppState>) -> Result<BasketView, String> {
    with_basket(&state, |_| {})
}

#[tauri::command]
fn basket_add(state: State<AppState>, ids: Vec<NodeId>) -> Result<BasketView, String> {
    with_basket(&state, |s| {
        for id in ids {
            s.basket.add(&s.tree, id);
        }
    })
}

#[tauri::command]
fn basket_remove(state: State<AppState>, ids: Vec<NodeId>) -> Result<BasketView, String> {
    with_basket(&state, |s| {
        for id in ids {
            s.basket.remove(id);
        }
    })
}

#[tauri::command]
fn basket_clear(state: State<AppState>) -> Result<BasketView, String> {
    with_basket(&state, |s| s.basket.clear())
}

/// Adds every suggested cleanup item (see `overview`) to the basket.
#[tauri::command]
fn basket_add_suggested(state: State<AppState>) -> Result<BasketView, String> {
    with_basket(&state, |s| {
        let (ids, _, _) = basket::cleanup_candidates(&s.tree, &s.rules, &s.dominant, usize::MAX);
        for id in ids {
            s.basket.add(&s.tree, id);
        }
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    /// Topmost items in "usually safe to clean" categories, minus anything
    /// risky: how many, their bytes, and the largest few.
    safe_count: usize,
    safe_bytes: u64,
    safe_items: Vec<Details>,
    largest_files: Vec<Details>,
}

/// What to show right after a scan.
#[tauri::command]
fn overview(state: State<AppState>) -> Result<Overview, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let details = |id: NodeId| Details {
        row: Row::new(&s.tree, id),
        path: s.tree.path(id).display().to_string(),
    };
    let (ids, safe_count, safe_bytes) =
        basket::cleanup_candidates(&s.tree, &s.rules, &s.dominant, 8);
    Ok(Overview {
        safe_count,
        safe_bytes,
        safe_items: ids.into_iter().map(details).collect(),
        largest_files: s.tree.largest_files(10).into_iter().map(details).collect(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportDone {
    path: String,
    rows: usize,
    millis: u64,
}

/// Saves everything below folder `id`, or the current search results, as
/// CSV or JSON to a file the user picks. `None` if they cancel.
#[tauri::command]
async fn export(
    app: AppHandle,
    state: State<'_, AppState>,
    format: String,
    search: bool,
    id: NodeId,
) -> Result<Option<ExportDone>, String> {
    let (format, ext, filter) = match format.as_str() {
        "csv" => (export::Format::Csv, "csv", "CSV"),
        "json" => (export::Format::Json, "json", "JSON"),
        f => return Err(format!("Unknown format {f:?}.")),
    };
    let stem = if search {
        "search results".to_string()
    } else {
        let guard = state.session.lock().unwrap();
        let s = guard.as_ref().ok_or("No scan yet.")?;
        if id as usize >= s.tree.len() {
            return Err(format!("No node {id}."));
        }
        let name: String = s
            .tree
            .node(id)
            .name
            .chars()
            .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
            .collect();
        name.trim_matches('_').to_string()
    };
    let Some(file) = app
        .dialog()
        .file()
        .add_filter(filter, &[ext])
        .set_file_name(format!("{stem}.{ext}"))
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let path = file.into_path().map_err(|e| e.to_string())?;

    let t = Instant::now();
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let mut out = BufWriter::new(File::create(&path).map_err(|e| e.to_string())?);
    let rows = if search {
        let st = s.search.as_ref().ok_or("There is no search to export.")?;
        let rows = st
            .results
            .ids
            .iter()
            .map(|&i| (i, s.tree.path(i).display().to_string()));
        export::write(&mut out, &s.tree, &s.rules, rows, format)
    } else {
        export::write(
            &mut out,
            &s.tree,
            &s.rules,
            export::subtree(&s.tree, id),
            format,
        )
    }
    .and_then(|n| out.flush().map(|_| n))
    .map_err(|e| e.to_string())?;
    Ok(Some(ExportDone {
        path: path.display().to_string(),
        rows,
        millis: t.elapsed().as_millis() as u64,
    }))
}

#[tauri::command]
fn drives() -> Vec<drives::Drive> {
    drives::list()
}

struct DeleteJob {
    id: NodeId,
    name: String,
    path: PathBuf,
    was_dir: bool,
    size: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteProgress {
    done: usize,
    total: usize,
    name: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFailure {
    path: String,
    error: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteDone {
    deleted: usize,
    bytes: u64,
    recycled: bool,
    failed: Vec<DeleteFailure>,
    cancelled: bool,
}

/// Deletes everything in the basket that isn't blocked, on a background
/// thread: `delete-progress` per item, then `delete-done`. Danger items need
/// `confirmed_danger`. `cancel_scan` stops after the current item.
#[tauri::command]
fn delete_basket(
    app: AppHandle,
    state: State<AppState>,
    permanent: bool,
    confirmed_danger: bool,
) -> Result<(), String> {
    let mut busy = state.scanning.lock().unwrap();
    if busy.is_some() {
        return Err("Busy with a scan or another delete.".into());
    }
    let jobs =
        {
            let guard = state.session.lock().unwrap();
            let s = guard.as_ref().ok_or("No scan yet.")?;
            let mut jobs = Vec::new();
            for &id in s.basket.items() {
                match basket::assess(&s.tree, &s.rules, &s.dominant, id).level {
                    Level::Blocked => continue,
                    Level::Danger if !confirmed_danger => return Err(
                        "Some items are system, program or OneDrive files; confirm those first."
                            .into(),
                    ),
                    _ => {}
                }
                let n = s.tree.node(id);
                jobs.push(DeleteJob {
                    id,
                    name: n.name.to_string(),
                    path: s.tree.path(id),
                    was_dir: n.kind == NodeKind::Dir,
                    size: effective_size(&s.tree, id),
                });
            }
            jobs
        };
    if jobs.is_empty() {
        return Err("Nothing in the basket can be deleted.".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *busy = Some(cancel.clone());
    drop(busy);
    let method = if permanent {
        Method::Permanent
    } else {
        Method::RecycleBin
    };
    let owner = owner_window(&app);
    thread::spawn(move || run_delete(app, jobs, method, owner, cancel));
    Ok(())
}

/// The main window's handle, to own any dialog Windows shows while deleting.
#[cfg(windows)]
fn owner_window(app: &AppHandle) -> isize {
    app.get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map_or(0, |h| h.0 as isize)
}

#[cfg(not(windows))]
fn owner_window(_app: &AppHandle) -> isize {
    0
}

fn run_delete(
    app: AppHandle,
    jobs: Vec<DeleteJob>,
    method: Method,
    owner: isize,
    cancel: Arc<AtomicBool>,
) {
    let state = app.state::<AppState>();
    let total = jobs.len();
    let mut done = DeleteDone {
        deleted: 0,
        bytes: 0,
        recycled: method == Method::RecycleBin,
        failed: Vec::new(),
        cancelled: false,
    };
    for (k, job) in jobs.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            done.cancelled = true;
            break;
        }
        let _ = app.emit(
            "delete-progress",
            DeleteProgress {
                done: k,
                total,
                name: job.name.clone(),
            },
        );
        match basket::delete(&job.path, job.was_dir, method, owner) {
            Ok(()) => {
                done.deleted += 1;
                done.bytes += job.size;
                if let Some(s) = state.session.lock().unwrap().as_mut() {
                    s.tree.remove(job.id);
                    s.basket.remove(job.id);
                }
            }
            Err(error) => done.failed.push(DeleteFailure {
                path: job.path.display().to_string(),
                error,
            }),
        }
    }
    if let Some(s) = state.session.lock().unwrap().as_mut() {
        s.dominant = treemap::dominant_categories(&s.tree, s.rules.categories().len());
        // Old search results may point at deleted items.
        s.generation += 1;
        s.search = None;
    }
    *state.scanning.lock().unwrap() = None;
    let _ = app.emit("delete-done", done);
}

/// Opens the file manager with this node selected.
#[tauri::command]
fn reveal(state: State<AppState>, id: NodeId) -> Result<(), String> {
    let path = node_path(state, id)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer parses its own command line; quote the path as one argument.
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{path}\""))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let dir = std::path::Path::new(&path)
            .parent()
            .map_or(path.clone(), |p| p.display().to_string());
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener)
            .arg(dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_scan(app: AppHandle, path: PathBuf, cancel: Arc<AtomicBool>) {
    let progress = Arc::new(Progress::default());
    let opts = ScanOptions {
        progress: Some(progress.clone()),
        cancel: Some(cancel),
        ..ScanOptions::default()
    };

    let start = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let ticker = {
        let (app, done) = (app.clone(), done.clone());
        thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let (files, dirs, bytes) = progress.snapshot();
                let msg = ProgressMsg {
                    files,
                    dirs,
                    bytes,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                };
                let _ = app.emit("scan-progress", msg);
                thread::sleep(Duration::from_millis(100));
            }
        })
    };

    let result = scanner::scan(&path, &opts);
    done.store(true, Ordering::Relaxed);
    let _ = ticker.join();

    let state = app.state::<AppState>();
    let outcome = result.map(|mut tree| {
        let (rules, rules_file, rules_error) = load_rules(&app);
        let t = Instant::now();
        rules.apply(&mut tree);
        let dominant = treemap::dominant_categories(&tree, rules.categories().len());
        Session {
            tree,
            rules,
            dominant,
            categorise: t.elapsed(),
            rules_file,
            rules_error,
            search: None,
            generation: 0,
            basket: Basket::default(),
            history_id: None,
            dupes: None,
        }
    });
    let event = match outcome {
        Ok(session) => {
            *state.session.lock().unwrap() = Some(session);
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    };
    *state.scanning.lock().unwrap() = None;
    match event {
        Ok(()) => {
            let _ = app.emit("scan-done", ());
            // After the results are on screen.
            scan_history::save_current(&app);
        }
        Err(e) => {
            let _ = app.emit("scan-error", e);
        }
    }
}

/// The built-in rules plus the user's rules file, if there is one. A broken
/// user file falls back to the built-in rules and reports why.
fn load_rules(app: &AppHandle) -> (Rules, Option<String>, Option<String>) {
    let Ok(dir) = app.path().app_config_dir() else {
        return (Rules::defaults(), None, None);
    };
    let file = dir.join(USER_RULES);
    let name = file.display().to_string();
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Rules::defaults(), Some(name), None)
        }
        Err(e) => {
            return (
                Rules::defaults(),
                Some(name.clone()),
                Some(format!("{name}: {e}")),
            )
        }
    };
    match Rules::with_user_rules(&name, &text) {
        Ok(rules) => (rules, Some(name), None),
        Err(e) => (Rules::defaults(), Some(name), Some(e.to_string())),
    }
}

/// The app's identifier, as in tauri.conf.json: it names its data folders.
const IDENTIFIER: &str = "io.github.raullopesalves.bytescape";
/// The identifier before the rename from "diskmap" to Bytescape.
const OLD_IDENTIFIER: &str = "dev.diskmap.desktop";

/// Moves `<base>\dev.diskmap.desktop` to `<base>\<IDENTIFIER>` in each base
/// folder, unless the new folder already exists. In %APPDATA% that carries
/// the scan history, category rules and window position; in %LOCALAPPDATA%,
/// the WebView2 data with the saved settings. If a move fails (for example
/// an old window is still open), the old data simply stays where it is.
fn move_old_data(bases: &[PathBuf]) {
    for base in bases {
        let (old, new) = (base.join(OLD_IDENTIFIER), base.join(IDENTIFIER));
        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
}

fn main() {
    // Before Tauri or WebView2 open anything in these folders.
    let bases: Vec<PathBuf> = ["APPDATA", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .collect();
    move_old_data(&bases);

    let elevated = elevation::is_elevated();
    if elevated {
        // WebView2 can't share its data folder between elevated and normal
        // processes, so the elevated app gets its own.
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let dir = PathBuf::from(local)
                .join(IDENTIFIER)
                .join("EBWebView-admin");
            std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir);
        }
    }
    tauri::Builder::default()
        .setup(move |app| {
            *app.state::<AppState>().history.lock().unwrap() = scan_history::open(app.handle());
            if elevated {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_title("Bytescape (administrator)");
                }
            }
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        // Restores the window's size and position from the last run.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            initial_path,
            pick_folder,
            start_scan,
            cancel_scan,
            summary,
            children,
            node_path,
            node_details,
            ancestors,
            treemap,
            search,
            search_page,
            clear_search,
            basket_view,
            basket_add,
            basket_remove,
            basket_clear,
            delete_basket,
            basket_add_suggested,
            overview,
            export,
            drives,
            relaunch_elevated,
            scan_history::history_list,
            scan_history::history_compare,
            scan_history::history_clear,
            duplicates::dupes_start,
            duplicates::dupes_view,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Bytescape");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_matches_the_tauri_config() {
        let conf = include_str!("../tauri.conf.json");
        assert!(conf.contains(&format!("\"identifier\": \"{IDENTIFIER}\"")));
    }

    #[test]
    fn old_data_moves_once() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("roaming"), dir.path().join("local"));
        std::fs::create_dir_all(a.join(OLD_IDENTIFIER)).unwrap();
        std::fs::write(a.join(OLD_IDENTIFIER).join("history.sqlite"), b"h").unwrap();
        // Here the new folder exists already: leave both alone.
        std::fs::create_dir_all(b.join(OLD_IDENTIFIER)).unwrap();
        std::fs::create_dir_all(b.join(IDENTIFIER)).unwrap();

        move_old_data(&[a.clone(), b.clone(), dir.path().join("missing")]);
        assert_eq!(
            std::fs::read(a.join(IDENTIFIER).join("history.sqlite")).unwrap(),
            b"h"
        );
        assert!(!a.join(OLD_IDENTIFIER).exists());
        assert!(b.join(OLD_IDENTIFIER).exists() && b.join(IDENTIFIER).exists());
    }
}
