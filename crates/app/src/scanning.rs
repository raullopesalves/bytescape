//! Scanning: whole scans, rescanning one folder, All drives, progress.

use super::*;

/// Name of the user rules file in the app's config folder.
pub(crate) const USER_RULES: &str = "categories.toml";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProgressMsg {
    pub(crate) files: u64,
    pub(crate) dirs: u64,
    pub(crate) bytes: u64,
    pub(crate) elapsed_ms: u64,
    /// What's being scanned in a scan of several drives ("C:\ (1 of 2)").
    pub(crate) stage: Option<String>,
}

/// Set while a scan runs, for the progress messages.
pub(crate) type Stage = Arc<Mutex<Option<String>>>;

/// What to scan for "All drives": every fixed and removable drive, under
/// one root.
pub(crate) const ALL_DRIVES: &str = "::all-drives";

/// The folder given on the command line (`bytescape <path>`), if any.
#[tauri::command]
pub fn initial_path() -> Option<String> {
    std::env::args().nth(1).map(|a| clean_arg(&a))
}

/// Explorer passes a drive as "C:\", whose backslash escapes the closing
/// quote, so it arrives as `C:"`.
pub(crate) fn clean_arg(arg: &str) -> String {
    let a = arg.trim_end_matches('"');
    if a.len() == 2 && a.ends_with(':') {
        format!("{a}\\")
    } else {
        a.to_string()
    }
}

#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Option<String> {
    let picked = app.dialog().file().blocking_pick_folder()?;
    picked.into_path().ok().map(|p| p.display().to_string())
}

#[tauri::command]
pub fn start_scan(app: AppHandle, state: State<AppState>, path: String) -> Result<(), String> {
    let mut scanning = state.scanning.lock().unwrap();
    if scanning.is_some() {
        return Err("A scan is already running.".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *scanning = Some(cancel.clone());
    // Free the previous tree before building the next one.
    *state.session.lock().unwrap() = None;
    applog::write(&format!("scan {path}"));
    thread::spawn(move || run_scan(app, PathBuf::from(path), cancel));
    Ok(())
}

#[tauri::command]
pub fn cancel_scan(state: State<AppState>) {
    if let Some(cancel) = &*state.scanning.lock().unwrap() {
        cancel.store(true, Ordering::Relaxed);
    }
}

/// Starts the app again as administrator to scan `path` (Windows asks the
/// user first), then closes this one.
#[tauri::command]
pub fn relaunch_elevated(app: AppHandle, path: String) -> Result<(), String> {
    elevation::relaunch(&path, owner_window(&app))?;
    app.exit(0);
    Ok(())
}

pub(crate) fn run_scan(app: AppHandle, path: PathBuf, cancel: Arc<AtomicBool>) {
    let progress = Arc::new(Progress::default());
    let opts = ScanOptions {
        progress: Some(progress.clone()),
        cancel: Some(cancel),
        exclude: app.state::<AppState>().exclude.lock().unwrap().clone(),
        ..ScanOptions::default()
    };

    let done = Arc::new(AtomicBool::new(false));
    let stage = Stage::default();
    let ticker = progress_ticker(&app, "scan-progress", progress, stage.clone(), done.clone());

    let result = if path.as_os_str() == ALL_DRIVES {
        scan_all_drives(&opts, &stage)
    } else {
        scanner::scan(&path, &opts)
    };
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
            let st = session.tree.stats();
            applog::write(&format!(
                "scanned {}: {} files, {} folders, {} bytes, {} errors, {} ms, {}{}",
                path.display(),
                st.files,
                st.dirs,
                session.tree.node(session.tree.root()).size,
                st.errors,
                st.elapsed.as_millis(),
                if st.method == scanner::ScanMethod::Mft {
                    "mft"
                } else {
                    "walk"
                },
                st.mft_skipped
                    .as_ref()
                    .map_or(String::new(), |w| format!(" (mft skipped: {w})")),
            ));
            *state.session.lock().unwrap() = Some(session);
            Ok(())
        }
        Err(e) => {
            applog::write(&format!("scan of {} stopped: {e}", path.display()));
            Err(e.to_string())
        }
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

/// Folders to leave out of the next scans (Settings).
#[tauri::command]
pub fn set_exclude(state: State<AppState>, exclude: Vec<String>) {
    *state.exclude.lock().unwrap() = exclude;
}

/// Sends `event` with the scan's progress every 100 ms until `done` is set.
pub(crate) fn progress_ticker(
    app: &AppHandle,
    event: &'static str,
    progress: Arc<Progress>,
    stage: Stage,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let app = app.clone();
    let start = Instant::now();
    thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            let (files, dirs, bytes) = progress.snapshot();
            let msg = ProgressMsg {
                files,
                dirs,
                bytes,
                elapsed_ms: start.elapsed().as_millis() as u64,
                stage: stage.lock().unwrap().clone(),
            };
            let _ = app.emit(event, msg);
            thread::sleep(Duration::from_millis(100));
        }
    })
}

/// Scans folder `id` again and puts the result in place of its old
/// contents. Runs in the background like a scan (and `cancel_scan` stops
/// it); sends `rescan-progress`, then `rescan-done` or `rescan-error`.
#[tauri::command]
pub fn rescan_folder(app: AppHandle, state: State<AppState>, id: NodeId) -> Result<String, String> {
    let path = {
        let guard = state.session.lock().unwrap();
        let tree = &guard.as_ref().ok_or("No scan yet.")?.tree;
        if id as usize >= tree.len() {
            return Err(format!("No node {id}."));
        }
        let n = tree.node(id);
        if n.kind != NodeKind::Dir || n.has_flag(flags::REMOVED) {
            return Err("Only folders can be rescanned.".into());
        }
        tree.path(id)
    };
    if !path.is_absolute() {
        return Err("Rescan everything instead.".into());
    }
    let mut scanning = state.scanning.lock().unwrap();
    if scanning.is_some() {
        return Err("A scan is already running.".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *scanning = Some(cancel.clone());
    let shown = path.display().to_string();
    applog::write(&format!("rescan {shown}"));
    thread::spawn(move || run_rescan(app, id, path, cancel));
    Ok(shown)
}

pub(crate) fn run_rescan(app: AppHandle, id: NodeId, path: PathBuf, cancel: Arc<AtomicBool>) {
    let progress = Arc::new(Progress::default());
    let opts = ScanOptions {
        progress: Some(progress.clone()),
        cancel: Some(cancel),
        exclude: app.state::<AppState>().exclude.lock().unwrap().clone(),
        ..ScanOptions::default()
    };
    let done = Arc::new(AtomicBool::new(false));
    let ticker = progress_ticker(
        &app,
        "rescan-progress",
        progress,
        Stage::default(),
        done.clone(),
    );
    let result = scanner::scan(&path, &opts);
    done.store(true, Ordering::Relaxed);
    let _ = ticker.join();

    let state = app.state::<AppState>();
    let outcome = result.map_err(|e| e.to_string()).and_then(|mut fresh| {
        let mut guard = state.session.lock().unwrap();
        let s = guard.as_mut().ok_or("The scan was closed.")?;
        s.rules.apply(&mut fresh);
        if !s.tree.replace_contents(id, fresh) {
            return Err("That folder can't be rescanned any more.".to_string());
        }
        s.dominant = treemap::dominant_categories(&s.tree, s.rules.categories().len());
        s.search = None;
        s.generation += 1;
        s.dupes = None;
        // What was in the basket from the old contents is gone.
        let gone: Vec<NodeId> = s
            .basket
            .items()
            .iter()
            .copied()
            .filter(|&i| s.tree.node(i).has_flag(flags::REMOVED))
            .collect();
        s.basket.remove_all(&gone);
        Ok(())
    });
    *state.scanning.lock().unwrap() = None;
    let _ = match outcome {
        Ok(()) => app.emit("rescan-done", id),
        Err(e) => {
            applog::write(&format!("rescan stopped: {e}"));
            app.emit("rescan-error", e)
        }
    };
}

/// Every fixed and removable drive, one after the other, under one root.
/// Drives that can't be read (no media) are left out.
pub(crate) fn scan_all_drives(
    opts: &ScanOptions,
    stage: &Stage,
) -> Result<ScanTree, scanner::ScanError> {
    let drives = scanner::local_drives();
    scanner::scan_drives(&drives, opts, |k, d| {
        *stage.lock().unwrap() = Some(format!("{} ({} of {})", d.display(), k + 1, drives.len()));
    })
}

/// The built-in rules plus the user's rules file, if there is one. A broken
/// user file falls back to the built-in rules and reports why.
pub(crate) fn load_rules(app: &AppHandle) -> (Rules, Option<String>, Option<String>) {
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
