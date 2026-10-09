//! The cleanup basket and deleting what's in it.

use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BasketItem {
    pub(crate) id: NodeId,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: &'static str,
    pub(crate) size: u64,
    pub(crate) level: Level,
    pub(crate) reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BasketView {
    pub(crate) items: Vec<BasketItem>,
    /// Items that can be deleted, and their bytes (repeated hardlinks count 0).
    pub(crate) count: usize,
    pub(crate) bytes: u64,
    pub(crate) blocked: usize,
    pub(crate) danger: usize,
}

pub(crate) fn basket_view_of(s: &Session) -> BasketView {
    let mut v = BasketView {
        items: Vec::new(),
        count: 0,
        bytes: 0,
        blocked: 0,
        danger: 0,
    };
    for &id in s.basket.items() {
        let a = basket::assess(&s.tree, &s.rules, &s.dominant, id);
        let n = s.tree.node(id);
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
            name: n.name.to_string(),
            path: s.tree.path(id).display().to_string(),
            kind: kind_name(n.kind),
            size: n.size,
            level: a.level,
            reason: a.reason,
        });
    }
    v
}

/// Bytes deleting `id` gives back (0 for a repeated hardlink).
pub(crate) fn effective_size(tree: &ScanTree, id: NodeId) -> u64 {
    let n = tree.node(id);
    if n.has_flag(flags::HARDLINK_DUP) {
        0
    } else {
        n.size
    }
}

pub(crate) fn with_basket(
    state: &State<AppState>,
    f: impl FnOnce(&mut Session),
) -> Result<BasketView, String> {
    let mut guard = state.session.lock().unwrap();
    let s = guard.as_mut().ok_or("No scan yet.")?;
    f(s);
    Ok(basket_view_of(s))
}

#[tauri::command]
pub fn basket_view(state: State<AppState>) -> Result<BasketView, String> {
    with_basket(&state, |_| {})
}

#[tauri::command]
pub fn basket_add(state: State<AppState>, ids: Vec<NodeId>) -> Result<BasketView, String> {
    with_basket(&state, |s| s.basket.add_all(&s.tree, &ids))
}

#[tauri::command]
pub fn basket_remove(state: State<AppState>, ids: Vec<NodeId>) -> Result<BasketView, String> {
    with_basket(&state, |s| s.basket.remove_all(&ids))
}

#[tauri::command]
pub fn basket_clear(state: State<AppState>) -> Result<BasketView, String> {
    with_basket(&state, |s| s.basket.clear())
}

/// Adds every suggested cleanup item (see `overview`) to the basket.
#[tauri::command]
pub fn basket_add_suggested(state: State<AppState>) -> Result<BasketView, String> {
    with_basket(&state, |s| {
        let (ids, _, _) = basket::cleanup_candidates(&s.tree, &s.rules, &s.dominant, usize::MAX);
        s.basket.add_all(&s.tree, &ids);
    })
}

pub(crate) struct DeleteJob {
    pub(crate) id: NodeId,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) was_dir: bool,
    pub(crate) size: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteProgress {
    pub(crate) done: usize,
    pub(crate) total: usize,
    pub(crate) name: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteFailure {
    pub(crate) path: String,
    pub(crate) error: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteDone {
    pub(crate) deleted: usize,
    pub(crate) bytes: u64,
    pub(crate) recycled: bool,
    pub(crate) failed: Vec<DeleteFailure>,
    pub(crate) cancelled: bool,
}

/// Deletes everything in the basket that isn't blocked, on a background
/// thread: `delete-progress` per item, then `delete-done`. Danger items need
/// `confirmed_danger`. `cancel_scan` stops after the current item.
#[tauri::command]
pub fn delete_basket(
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

pub(crate) fn run_delete(
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
    applog::write(&format!(
        "deleted {} items, {} bytes, {}{}; {} failed{}",
        done.deleted,
        done.bytes,
        if done.recycled {
            "to the Recycle Bin"
        } else {
            "permanently"
        },
        if done.cancelled { ", cancelled" } else { "" },
        done.failed.len(),
        done.failed
            .iter()
            .take(20)
            .map(|f| format!(" | {}: {}", f.path, f.error))
            .collect::<String>(),
    ));
    let _ = app.emit("delete-done", done);
}
