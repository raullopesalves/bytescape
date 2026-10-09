//! Looking around a scan: totals, folders, the treemap, the overview, export, file types and the right-click actions.

use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CategoryRow {
    pub(crate) id: u16,
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) color: String,
    pub(crate) cleanup: &'static str,
    pub(crate) files: u64,
    pub(crate) size: u64,
    pub(crate) alloc: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    pub(crate) root: Row,
    pub(crate) files: u64,
    pub(crate) dirs: u64,
    pub(crate) links: u64,
    pub(crate) errors: u64,
    pub(crate) hardlink_dups: u64,
    pub(crate) scan_ms: u64,
    pub(crate) categorise_ms: u64,
    /// Every category, in id order, with its totals.
    pub(crate) categories: Vec<CategoryRow>,
    pub(crate) rules_file: Option<String>,
    pub(crate) rules_error: Option<String>,
    /// "mft" or "walk".
    pub(crate) method: &'static str,
    /// Why the MFT was tried but not used.
    pub(crate) mft_skipped: Option<String>,
    /// A whole-drive scan that would be faster as administrator.
    pub(crate) admin_would_help: bool,
    /// Size and free space of the drive holding the scanned folder.
    pub(crate) drive_total: Option<u64>,
    pub(crate) drive_free: Option<u64>,
}

#[tauri::command]
pub fn summary(state: State<AppState>) -> Result<Summary, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let tree = &s.tree;
    let st = tree.stats();
    let root_path = tree.path(tree.root());
    let space = if root_path.is_absolute() {
        drives::space(&root_path)
    } else {
        // All drives: add up the drives scanned.
        tree.child_ids(tree.root())
            .filter_map(|c| drives::space(&tree.path(c)))
            .reduce(|a, b| (a.0 + b.0, a.1 + b.1))
    };
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
        drive_total: space.map(|s| s.0),
        drive_free: space.map(|s| s.1),
    })
}

/// Direct children of a folder, in name order.
#[tauri::command]
pub fn children(state: State<AppState>, id: NodeId) -> Result<Vec<Row>, String> {
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
pub fn node_path(state: State<AppState>, id: NodeId) -> Result<String, String> {
    let guard = state.session.lock().unwrap();
    let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
    if id as usize >= tree.len() {
        return Err(format!("No node {id}."));
    }
    Ok(tree.path(id).display().to_string())
}

/// One node and its full path, for tooltips.
#[derive(Serialize)]
pub(crate) struct Details {
    pub(crate) row: Row,
    pub(crate) path: String,
}

#[tauri::command]
pub fn node_details(state: State<AppState>, id: NodeId) -> Result<Details, String> {
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
pub fn ancestors(state: State<AppState>, id: NodeId) -> Result<Vec<(NodeId, String)>, String> {
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
pub fn treemap(
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
pub(crate) struct Overview {
    /// Topmost items in "usually safe to clean" categories, minus anything
    /// risky: how many, their bytes, and the largest few.
    pub(crate) safe_count: usize,
    pub(crate) safe_bytes: u64,
    pub(crate) safe_items: Vec<Details>,
    pub(crate) largest_files: Vec<Details>,
}

/// What to show right after a scan.
#[tauri::command]
pub fn overview(state: State<AppState>) -> Result<Overview, String> {
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
pub(crate) struct ExportDone {
    pub(crate) path: String,
    pub(crate) rows: usize,
    pub(crate) millis: u64,
}

/// Saves everything below folder `id`, or the current search results, as
/// CSV or JSON to a file the user picks. `None` if they cancel.
#[tauri::command]
pub async fn export(
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
pub fn drives() -> Vec<drives::Drive> {
    drives::list()
}

/// Files of one extension.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileType {
    /// Lower case, without the dot; empty for files without one.
    pub(crate) ext: String,
    pub(crate) files: u64,
    pub(crate) size: u64,
    pub(crate) alloc: u64,
    /// The category holding most of its bytes.
    pub(crate) category: u16,
}

/// Totals per file extension, largest first.
#[tauri::command]
pub fn file_types(state: State<AppState>) -> Result<Vec<FileType>, String> {
    let guard = state.session.lock().unwrap();
    let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
    Ok(categories::file_types(tree)
        .into_iter()
        .map(|t| FileType {
            ext: t.ext,
            files: t.files,
            size: t.size,
            alloc: t.alloc,
            category: t.category,
        })
        .collect())
}

/// The node at `path`, if the scan covers it (names compared ignoring case).
pub(crate) fn find_node(tree: &ScanTree, path: &Path) -> Option<NodeId> {
    let root = tree.root();
    let roots: Vec<NodeId> = if tree.path(root).is_absolute() {
        vec![root]
    } else {
        tree.child_ids(root).collect()
    };
    let want = path.to_string_lossy().to_lowercase();
    for r in roots {
        let rp = tree.path(r).to_string_lossy().to_lowercase();
        let Some(rest) = want.strip_prefix(rp.trim_end_matches('\\')) else {
            continue;
        };
        if !(rest.is_empty() || rest.starts_with('\\')) {
            continue;
        }
        let mut id = r;
        for part in rest.split('\\').filter(|p| !p.is_empty()) {
            id = tree.child_ids(id).find(|&c| {
                let n = tree.node(c);
                !n.has_flag(flags::REMOVED) && n.name.to_lowercase() == part
            })?;
        }
        return Some(id);
    }
    None
}

#[derive(Serialize)]
pub(crate) struct Place {
    pub(crate) name: &'static str,
    pub(crate) path: String,
}

/// The user's standard folders that exist (they may have been moved, for
/// example into OneDrive), for the start screen.
#[tauri::command]
pub fn places(app: AppHandle) -> Vec<Place> {
    let p = app.path();
    [
        ("Home", p.home_dir()),
        ("Desktop", p.desktop_dir()),
        ("Documents", p.document_dir()),
        ("Downloads", p.download_dir()),
        ("Pictures", p.picture_dir()),
        ("Videos", p.video_dir()),
        ("Music", p.audio_dir()),
    ]
    .into_iter()
    .filter_map(|(name, dir)| {
        let dir = dir.ok().filter(|d| d.is_dir())?;
        Some(Place {
            name,
            path: dir.display().to_string(),
        })
    })
    .collect()
}

/// Opens the file manager with this node selected.
#[tauri::command]
pub fn reveal(state: State<AppState>, id: NodeId) -> Result<(), String> {
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

/// A right-click action on node `id`: "open", "terminal" (in its folder)
/// or "properties".
#[tauri::command]
pub fn item_action(
    app: AppHandle,
    state: State<AppState>,
    id: NodeId,
    action: String,
) -> Result<(), String> {
    let (path, is_dir) = {
        let guard = state.session.lock().unwrap();
        let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
        if id as usize >= tree.len() || tree.node(id).has_flag(flags::REMOVED) {
            return Err("That item is gone.".into());
        }
        (tree.path(id), tree.node(id).kind == NodeKind::Dir)
    };
    if !path.is_absolute() {
        return Err("Pick a drive or folder inside it.".into());
    }
    let owner = owner_window(&app);
    match action.as_str() {
        "open" => actions::open(&path, owner),
        "terminal" => {
            let dir = if is_dir {
                path.as_path()
            } else {
                path.parent().unwrap_or(&path)
            };
            actions::terminal(dir, owner)
        }
        "properties" => app
            .run_on_main_thread(move || {
                let _ = actions::properties(&path, owner);
            })
            .map_err(|e| e.to_string()),
        _ => Err(format!("Unknown action {action}.")),
    }
}
