//! The duplicate finder in the app: runs in the background (`dupes-progress`,
//! then `dupes-done`) and keeps its result with the scan.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use scanner::{flags, NodeId};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;

/// A finished duplicate search.
pub struct Found {
    pub report: dupes::Report,
    pub millis: u64,
    pub min_size: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DupesProgress {
    stage: u8,
    files_done: u64,
    files_total: u64,
    bytes_read: u64,
    elapsed_ms: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DupesDone {
    cancelled: bool,
}

/// Starts looking for duplicate files of at least `min_size` bytes.
#[tauri::command]
pub fn dupes_start(app: AppHandle, state: State<AppState>, min_size: u64) -> Result<(), String> {
    let mut busy = state.scanning.lock().unwrap();
    if busy.is_some() {
        return Err("Busy with a scan, a delete or another search.".into());
    }
    let (candidates, root) = {
        let guard = state.session.lock().unwrap();
        let s = guard.as_ref().ok_or("No scan yet.")?;
        (
            dupes::candidates(&s.tree, min_size),
            s.tree.node(s.tree.root()).name.to_string(),
        )
    };
    let cancel = Arc::new(AtomicBool::new(false));
    *busy = Some(cancel.clone());
    drop(busy);
    thread::spawn(move || run(app, candidates, root, min_size, cancel));
    Ok(())
}

fn run(
    app: AppHandle,
    candidates: dupes::Candidates,
    root: String,
    min_size: u64,
    cancel: Arc<AtomicBool>,
) {
    let progress = Arc::new(dupes::Progress::default());
    let start = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let ticker = {
        let (app, done, progress) = (app.clone(), done.clone(), progress.clone());
        thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let _ = app.emit(
                    "dupes-progress",
                    DupesProgress {
                        stage: progress.stage.load(Ordering::Relaxed),
                        files_done: progress.files_done.load(Ordering::Relaxed),
                        files_total: progress.files_total.load(Ordering::Relaxed),
                        bytes_read: progress.bytes_read.load(Ordering::Relaxed),
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    },
                );
                thread::sleep(Duration::from_millis(250));
            }
        })
    };
    let opts = dupes::Options {
        progress: Some(progress),
        cancel: Some(cancel),
        ..dupes::Options::default()
    };
    let result = dupes::find(candidates, &opts);
    done.store(true, Ordering::Relaxed);
    let _ = ticker.join();

    let state = app.state::<AppState>();
    let cancelled = result.is_err();
    if let Ok(report) = result {
        if let Some(s) = state.session.lock().unwrap().as_mut() {
            // Unless another scan replaced this one meanwhile.
            if *s.tree.node(s.tree.root()).name == root {
                s.dupes = Some(Found {
                    report,
                    millis: start.elapsed().as_millis() as u64,
                    min_size,
                });
            }
        }
    }
    *state.scanning.lock().unwrap() = None;
    let _ = app.emit("dupes-done", DupesDone { cancelled });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupFile {
    id: NodeId,
    name: String,
    path: String,
    modified: i64,
    category: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    size: u64,
    wasted: u64,
    files: Vec<DupFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupesView {
    /// False until a search has finished for this scan.
    done: bool,
    /// The `limit` groups wasting most space.
    groups: Vec<DupGroup>,
    total_groups: usize,
    total_wasted: u64,
    candidates: u64,
    candidate_bytes: u64,
    bytes_read: u64,
    changed: u64,
    unreadable: u64,
    millis: u64,
    min_size: u64,
}

/// The duplicates found for this scan. Files deleted since are left out.
#[tauri::command]
pub fn dupes_view(state: State<AppState>, limit: usize) -> Result<DupesView, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let Some(found) = &s.dupes else {
        return Ok(DupesView {
            done: false,
            groups: Vec::new(),
            total_groups: 0,
            total_wasted: 0,
            candidates: 0,
            candidate_bytes: 0,
            bytes_read: 0,
            changed: 0,
            unreadable: 0,
            millis: 0,
            min_size: 0,
        });
    };
    let tree = &s.tree;
    let mut groups: Vec<dupes::Group> = found
        .report
        .groups
        .iter()
        .map(|g| dupes::Group {
            size: g.size,
            files: g
                .files
                .iter()
                .copied()
                .filter(|&id| !tree.node(id).has_flag(flags::REMOVED))
                .collect(),
        })
        .filter(|g| g.files.len() > 1)
        .collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.wasted()));
    let r = &found.report;
    Ok(DupesView {
        done: true,
        total_groups: groups.len(),
        total_wasted: groups.iter().map(|g| g.wasted()).sum(),
        groups: groups
            .into_iter()
            .take(limit)
            .map(|g| DupGroup {
                size: g.size,
                wasted: g.wasted(),
                files: g
                    .files
                    .iter()
                    .map(|&id| {
                        let n = tree.node(id);
                        DupFile {
                            id,
                            name: n.name.to_string(),
                            path: tree.path(id).display().to_string(),
                            modified: n.modified,
                            category: s.rules.category(n.category).map(|c| c.name.clone()),
                        }
                    })
                    .collect(),
            })
            .collect(),
        candidates: r.candidates,
        candidate_bytes: r.candidate_bytes,
        bytes_read: r.bytes_read,
        changed: r.changed,
        unreadable: r.unreadable,
        millis: found.millis,
        min_size: found.min_size,
    })
}
