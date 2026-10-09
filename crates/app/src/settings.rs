//! Settings: category rules, the Explorer menu, Windows' own cleanups, links.

use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CleanupStatus {
    #[serde(flatten)]
    pub(crate) windows: wincleanup::Status,
    /// Sizes from the scan, when it covers these folders.
    pub(crate) windows_old_size: Option<u64>,
    pub(crate) winsxs_size: Option<u64>,
    pub(crate) elevated: bool,
}

#[tauri::command]
pub fn cleanup_status(state: State<AppState>) -> CleanupStatus {
    let windows = wincleanup::status();
    let guard = state.session.lock().unwrap();
    let size_of = |rel: &str| {
        let s = guard.as_ref()?;
        let p = PathBuf::from(format!(r"{}\{rel}", windows.system_drive));
        find_node(&s.tree, &p).map(|id| s.tree.node(id).size)
    };
    CleanupStatus {
        windows_old_size: size_of("Windows.old"),
        winsxs_size: size_of(r"Windows\WinSxS"),
        windows,
        elevated: elevation::is_elevated(),
    }
}

/// Runs one of Windows' cleanups; Windows asks the user first.
#[tauri::command]
pub fn run_cleanup(app: AppHandle, what: String) -> Result<bool, String> {
    let owner = owner_window(&app);
    match what.as_str() {
        "recycle" => Ok(wincleanup::empty_recycle_bin(owner)),
        "hibernation" => wincleanup::hibernation_off(owner).map(|()| true),
        "components" => wincleanup::component_cleanup(owner).map(|()| true),
        "windows-old" => wincleanup::open_storage_settings(owner).map(|()| true),
        _ => Err(format!("Unknown cleanup {what}.")),
    }
}

#[tauri::command]
pub fn shell_menu_state() -> bool {
    shell_menu::is_on()
}

#[tauri::command]
pub fn set_shell_menu(on: bool) -> Result<bool, String> {
    shell_menu::set(on)?;
    Ok(shell_menu::is_on())
}

/// Where the user's category rules go, and whether the file exists yet.
#[tauri::command]
pub fn rules_info(app: AppHandle) -> Result<(String, bool), String> {
    let file = rules_path(&app)?;
    Ok((with_appdata(&file.display().to_string()), file.exists()))
}

pub(crate) fn rules_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join(USER_RULES))
}

pub(crate) const RULES_TEMPLATE: &str = r#"# Your own category rules. They add to the built-in ones, and a category
# with the id of a built-in one replaces it. Save, then use "Reload rules"
# in Bytescape's settings.
#
# The format, with every option, is described at the top of the built-in
# rules: https://github.com/raullopesalves/bytescape/blob/main/crates/categories/rules/default.toml
#
# An example (remove the # to use it):
#
# [[category]]
# id = "my-projects"
# name = "My projects"
# color = "teal"
# paths = ["D:/Projects/**"]
"#;

/// Opens the user's rules file in its editor, creating it with some help
/// text first if it doesn't exist.
#[tauri::command]
pub fn open_rules(app: AppHandle) -> Result<(), String> {
    let file = rules_path(&app)?;
    if !file.exists() {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&file, RULES_TEMPLATE).map_err(|e| e.to_string())?;
    }
    let owner = owner_window(&app);
    let path = file.display().to_string();
    // .toml often has no program; Notepad always works.
    elevation::shell_execute("open", &path, "", true, owner).or_else(|_| {
        elevation::shell_execute("open", "notepad.exe", &format!("\"{path}\""), true, owner)
    })
}

/// Reads the rules file again and re-categorises the current scan.
#[tauri::command]
pub fn reload_rules(app: AppHandle, state: State<AppState>) -> Result<Option<String>, String> {
    let (rules, rules_file, rules_error) = load_rules(&app);
    let mut guard = state.session.lock().unwrap();
    let s = guard.as_mut().ok_or("No scan yet.")?;
    rules.apply(&mut s.tree);
    s.dominant = treemap::dominant_categories(&s.tree, rules.categories().len());
    s.rules = rules;
    s.rules_file = rules_file;
    s.rules_error = rules_error.clone();
    s.search = None;
    s.generation += 1;
    Ok(rules_error)
}

/// Opens a page of the project's site in the browser (About).
#[tauri::command]
pub fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    const SITE: &str = "https://github.com/raullopesalves/bytescape";
    if url != SITE && !url.starts_with(&format!("{SITE}/")) {
        return Err("Only the project's own pages open from here.".into());
    }
    elevation::shell_execute("open", &url, "", true, owner_window(&app))
}
