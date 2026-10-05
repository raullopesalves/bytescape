//! diskmap desktop app: scan a folder or drive, then browse the result.
//!
//! The scan runs on a background thread and reports progress through the
//! `scan-progress` event, then `scan-done` or `scan-error`. The finished tree
//! stays here; the UI asks for one folder's children at a time.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use categories::{Cleanup, Rules};
use scanner::{NodeId, NodeKind, Progress, ScanOptions, ScanTree};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

/// Name of the user rules file in the app's config folder.
const USER_RULES: &str = "categories.toml";

/// A finished scan the UI is browsing.
struct Session {
    tree: ScanTree,
    rules: Rules,
    categorise: Duration,
    rules_file: Option<String>,
    rules_error: Option<String>,
}

#[derive(Default)]
struct AppState {
    session: Mutex<Option<Session>>,
    /// The running scan's cancel flag; `None` when no scan is running.
    scanning: Mutex<Option<Arc<AtomicBool>>>,
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
            children: n.child_count,
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
}

/// The folder given on the command line (`diskmap <path>`), if any.
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
        rules_file: s.rules_file.clone(),
        rules_error: s.rules_error.clone(),
    })
}

/// Direct children of a folder, in name order.
#[tauri::command]
fn children(state: State<AppState>, id: NodeId) -> Result<Vec<Row>, String> {
    let guard = state.session.lock().unwrap();
    let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
    if id as usize >= tree.len() {
        return Err(format!("No node {id}."));
    }
    Ok(tree.child_ids(id).map(|c| Row::new(tree, c)).collect())
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
        Session {
            tree,
            rules,
            categorise: t.elapsed(),
            rules_file,
            rules_error,
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
    let _ = match event {
        Ok(()) => app.emit("scan-done", ()),
        Err(e) => app.emit("scan-error", e),
    };
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            initial_path,
            pick_folder,
            start_scan,
            cancel_scan,
            summary,
            children,
            node_path,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("failed to start diskmap");
}
