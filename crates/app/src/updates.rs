//! Updates. The app reads `latest.json` from the latest GitHub release
//! (endpoint and public key in tauri.conf.json, `plugins.updater`). When
//! asked, it downloads the installer named there, checks its signature
//! against the public key, and runs it: the app closes, the installer runs
//! with a small progress window, and the new version starts.
//!
//! Releases are signed by the release workflow with the private key kept in
//! the repository's secrets (see docs/HANDOFF.md, "Updates").

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// The update found by the last check, kept for `install_update`.
#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    version: String,
    current: String,
    notes: Option<String>,
    /// Release date, `YYYY-MM-DD`.
    date: Option<String>,
}

#[derive(Clone, Serialize)]
struct Progress {
    done: u64,
    total: Option<u64>,
}

/// Asks for the latest release; `None` when this version is the newest.
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    pending: State<'_, Pending>,
) -> Result<Option<Available>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| {
            crate::applog::write(&format!("update check failed: {e}"));
            e.to_string()
        })?;
    let info = update.as_ref().map(|u| Available {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.date().to_string()),
    });
    *pending.0.lock().unwrap() = update;
    Ok(info)
}

/// Downloads and installs the update found by `check_update`, sending
/// `update-progress` events. On Windows the app exits when the installer
/// starts, so this only returns on failure.
#[tauri::command]
pub async fn install_update(app: AppHandle, pending: State<'_, Pending>) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .unwrap()
        .take()
        .ok_or("No update to install; check for updates again.")?;
    crate::applog::write(&format!("installing update {}", update.version));
    let mut done = 0u64;
    update
        .download_and_install(
            |chunk, total| {
                done += chunk as u64;
                let _ = app.emit("update-progress", Progress { done, total });
            },
            || {},
        )
        .await
        .map_err(|e| {
            crate::applog::write(&format!("update failed: {e}"));
            e.to_string()
        })
}

#[tauri::command]
pub fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}
