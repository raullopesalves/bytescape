//! Scan history in the app: a snapshot is saved after each scan, and the
//! History view lists past scans of the same folder and what changed since.

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use history::{CategoryRow, History};
use scanner::{NodeId, ScanMethod};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{AppState, Session};

/// The history database in the app's data folder; `None` if it can't be opened.
pub fn open(app: &AppHandle) -> Option<History> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    History::open(&dir.join("history.sqlite")).ok()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn category_rows(s: &Session) -> Vec<CategoryRow> {
    s.rules
        .totals(&s.tree)
        .into_iter()
        .filter(|t| t.files > 0)
        .filter_map(|t| {
            let c = s.rules.category(t.category)?;
            Some(CategoryRow {
                key: c.id.clone(),
                name: c.name.clone(),
                files: t.files,
                size: t.size,
            })
        })
        .collect()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    id: i64,
    folders: usize,
    millis: u64,
}

/// Saves a snapshot of the scan the UI is showing. Runs after `scan-done`,
/// so the results are on screen first; emits `history-saved`.
pub fn save_current(app: &AppHandle) {
    let state = app.state::<AppState>();
    let t = Instant::now();
    let snap = {
        let guard = state.session.lock().unwrap();
        let Some(s) = guard.as_ref() else { return };
        let method = match s.tree.stats().method {
            ScanMethod::Mft => "mft",
            _ => "walk",
        };
        history::snapshot(&s.tree, now(), method, category_rows(s))
    };
    let id = match state.history.lock().unwrap().as_mut() {
        Some(h) => match h.save(&snap) {
            Ok(id) => id,
            Err(_) => return,
        },
        None => return,
    };
    if let Some(s) = state.session.lock().unwrap().as_mut() {
        // Unless another scan replaced this one meanwhile.
        if *s.tree.node(s.tree.root()).name == snap.info.root {
            s.history_id = Some(id);
        }
    }
    let _ = app.emit(
        "history-saved",
        Saved {
            id,
            folders: snap.folders.len(),
            millis: t.elapsed().as_millis() as u64,
        },
    );
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastScan {
    id: i64,
    taken: i64,
    method: String,
    size: u64,
    files: u64,
    /// This is the scan being shown.
    current: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryList {
    available: bool,
    scans: Vec<PastScan>,
    /// Space the whole history takes, and how many scans it holds.
    bytes: u64,
    total_scans: u64,
}

/// Past scans of the folder being shown, newest first.
#[tauri::command]
pub fn history_list(state: State<AppState>) -> Result<HistoryList, String> {
    let (root, current) = {
        let guard = state.session.lock().unwrap();
        let s = guard.as_ref().ok_or("No scan yet.")?;
        (s.tree.node(s.tree.root()).name.to_string(), s.history_id)
    };
    let guard = state.history.lock().unwrap();
    let Some(h) = guard.as_ref() else {
        return Ok(HistoryList {
            available: false,
            scans: Vec::new(),
            bytes: 0,
            total_scans: 0,
        });
    };
    let scans = h
        .list(&root)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|i| PastScan {
            current: Some(i.id) == current,
            id: i.id,
            taken: i.taken,
            method: i.method,
            size: i.size,
            files: i.files,
        })
        .collect();
    let (bytes, total_scans) = h.usage().map_err(|e| e.to_string())?;
    Ok(HistoryList {
        available: true,
        scans,
        bytes,
        total_scans,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRow {
    path: String,
    name: String,
    /// The folder now, to show it in the tree; `None` if it's gone.
    node: Option<NodeId>,
    old: u64,
    new: u64,
    /// Not in the old snapshot: new, or smaller than `threshold` then.
    was_missing: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryChange {
    key: String,
    name: String,
    old: u64,
    new: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    taken: i64,
    old_total: u64,
    new_total: u64,
    threshold: u64,
    changes: Vec<ChangeRow>,
    categories: Vec<CategoryChange>,
    millis: u64,
}

/// What changed between past scan `id` and the scan being shown.
#[tauri::command]
pub fn history_compare(state: State<AppState>, id: i64) -> Result<Comparison, String> {
    let snap = {
        let guard = state.history.lock().unwrap();
        let h = guard.as_ref().ok_or("History is not available.")?;
        h.load(id)
            .map_err(|e| e.to_string())?
            .ok_or("That scan is no longer in the history.")?
    };
    let t = Instant::now();
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let root = s.tree.node(s.tree.root());
    let base = root.name.trim_end_matches(['\\', '/']).to_string();
    let changes = history::compare(&snap, &s.tree, 50)
        .into_iter()
        .map(|c| ChangeRow {
            name: c.path.rsplit('\\').next().unwrap_or(&c.path).to_string(),
            path: format!("{base}\\{}", c.path),
            node: c.node,
            old: c.old,
            new: c.new,
            was_missing: c.was_missing,
        })
        .collect();

    // Categories then and now, by key; largest change first.
    let mut cats: HashMap<String, CategoryChange> = HashMap::new();
    for c in &snap.categories {
        cats.insert(
            c.key.clone(),
            CategoryChange {
                key: c.key.clone(),
                name: c.name.clone(),
                old: c.size,
                new: 0,
            },
        );
    }
    for c in category_rows(s) {
        cats.entry(c.key.clone())
            .or_insert(CategoryChange {
                key: c.key,
                name: c.name,
                old: 0,
                new: 0,
            })
            .new = c.size;
    }
    let mut categories: Vec<CategoryChange> =
        cats.into_values().filter(|c| c.old != c.new).collect();
    categories.sort_by_key(|c| std::cmp::Reverse((c.new as i64 - c.old as i64).unsigned_abs()));

    Ok(Comparison {
        taken: snap.info.taken,
        old_total: snap.info.size,
        new_total: root.size,
        threshold: snap.info.threshold,
        changes,
        categories,
        millis: t.elapsed().as_millis() as u64,
    })
}

/// Deletes all scan history.
#[tauri::command]
pub fn history_clear(state: State<AppState>) -> Result<HistoryList, String> {
    if let Some(h) = state.history.lock().unwrap().as_mut() {
        h.clear().map_err(|e| e.to_string())?;
    }
    if let Some(s) = state.session.lock().unwrap().as_mut() {
        s.history_id = None;
    }
    history_list(state)
}
