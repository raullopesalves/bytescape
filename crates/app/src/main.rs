//! Bytescape desktop app: scan a folder or drive, then browse the result.
//!
//! The scan runs on a background thread and reports progress through the
//! `scan-progress` event, then `scan-done` or `scan-error`. The finished tree
//! stays here; the UI asks for one folder's children at a time, and for
//! treemap layouts as packed binary rectangles.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod applog;
mod basket;
mod browse;
mod deleting;
mod drives;
mod duplicates;
mod elevation;
mod export;
mod results;
mod scan_history;
mod scanning;
mod search;
mod settings;
mod shell_menu;
mod treemap;
mod updates;
mod wincleanup;

use browse::*;
use scanning::*;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
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
    /// Folders to leave out of scans (Settings), for `ScanOptions::exclude`.
    exclude: Mutex<Vec<String>>,
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
    /// Why it probably shouldn't be deleted or changed by hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    risk: Option<Risk>,
    /// In search results grouped by folder: the node matches itself.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    hit: bool,
}

#[derive(Serialize)]
struct Risk {
    level: basket::Level,
    reason: String,
}

impl Row {
    fn new(tree: &ScanTree, id: NodeId) -> Row {
        let n = tree.node(id);
        Row {
            id,
            name: n.name.to_string(),
            kind: kind_name(n.kind),
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
            risk: (id != tree.root())
                .then(|| basket::caution(tree, id))
                .flatten()
                .map(|a| Risk {
                    level: a.level,
                    reason: a.reason.unwrap_or_default(),
                }),
            hit: false,
        }
    }

    /// The row as search results see it: size, on disk and files count only
    /// the matches in it, and only children holding matches count.
    fn matched(tree: &ScanTree, results: &search::Results, id: NodeId) -> Row {
        let mut r = Row::new(tree, id);
        let t = results.below.get(&id).copied().unwrap_or_default();
        (r.size, r.alloc, r.files) = (t.size, t.alloc, t.files);
        r.children = matched_children(tree, results, id).count() as u32;
        r.hit = results.marks[id as usize] & search::SELF != 0;
        r
    }
}

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "file",
        NodeKind::Dir => "dir",
        NodeKind::Symlink => "link",
        NodeKind::Error => "error",
    }
}

fn matched_children<'a>(
    tree: &'a ScanTree,
    results: &'a search::Results,
    id: NodeId,
) -> impl Iterator<Item = NodeId> + 'a {
    tree.child_ids(id)
        .filter(|&c| results.marks[c as usize] != 0 && !tree.node(c).has_flag(flags::REMOVED))
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

/// An error from the window (a script error), for the log.
#[tauri::command]
fn log_ui(msg: String) {
    applog::write(&format!(
        "window: {}",
        msg.chars().take(2000).collect::<String>()
    ));
}

/// Shows the log file in Explorer.
#[tauri::command]
fn open_log(app: AppHandle) -> Result<(), String> {
    let path = applog::path().ok_or("There's no log yet.")?;
    elevation::shell_execute(
        "open",
        "explorer.exe",
        &format!("/select,\"{}\"", path.display()),
        true,
        owner_window(&app),
    )
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
            if let Ok(dir) = app.path().app_local_data_dir() {
                applog::init(&dir);
            }
            applog::write(&format!(
                "Bytescape {} started{}",
                app.package_info().version,
                if elevated { " as administrator" } else { "" }
            ));
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::default())
        .manage(updates::Pending::default())
        .invoke_handler(tauri::generate_handler![
            scanning::initial_path,
            scanning::pick_folder,
            scanning::start_scan,
            scanning::cancel_scan,
            browse::summary,
            browse::children,
            browse::node_path,
            browse::node_details,
            browse::ancestors,
            browse::treemap,
            results::search,
            results::search_page,
            results::search_ids,
            results::search_children,
            results::search_matches_in,
            browse::file_types,
            scanning::rescan_folder,
            browse::item_action,
            scanning::set_exclude,
            settings::rules_info,
            settings::open_rules,
            settings::reload_rules,
            settings::open_link,
            settings::cleanup_status,
            settings::run_cleanup,
            settings::shell_menu_state,
            settings::set_shell_menu,
            settings::cli_path,
            log_ui,
            open_log,
            updates::check_update,
            updates::install_update,
            updates::app_version,
            results::clear_search,
            deleting::basket_view,
            deleting::basket_add,
            deleting::basket_remove,
            deleting::basket_clear,
            deleting::delete_basket,
            deleting::basket_add_suggested,
            browse::overview,
            browse::export,
            browse::drives,
            browse::places,
            scanning::relaunch_elevated,
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
    #[test]
    fn the_rules_template_is_valid_and_adds_nothing() {
        let rules = categories::Rules::with_user_rules("template", super::settings::RULES_TEMPLATE)
            .unwrap();
        assert_eq!(
            rules.categories().len(),
            categories::Rules::defaults().categories().len()
        );
    }

    #[test]
    fn explorer_drive_arguments() {
        assert_eq!(super::clean_arg(r#"C:""#), r"C:\");
        assert_eq!(super::clean_arg(r"D:\Games"), r"D:\Games");
        assert_eq!(super::clean_arg(r"\\server\share"), r"\\server\share");
    }

    #[test]
    fn finding_paths() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("Sub")).unwrap();
        std::fs::write(r.join("Sub/b.mp4"), vec![0u8; 2000]).unwrap();
        let tree = scanner::scan(r, &scanner::ScanOptions::default()).unwrap();
        let sub = super::find_node(&tree, &r.join("sub").join("B.mp4")).unwrap();
        assert_eq!(&*tree.node(sub).name, "b.mp4");
        assert_eq!(super::find_node(&tree, r), Some(tree.root()));
        assert_eq!(super::find_node(&tree, &r.join("nope")), None);
    }

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
