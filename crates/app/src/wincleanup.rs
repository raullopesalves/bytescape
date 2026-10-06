//! Windows' own cleanups, which often free the most space: emptying the
//! Recycle Bin, turning hibernation off (removes hiberfil.sys), cleaning up
//! the component store (WinSxS) and removing a previous Windows
//! installation (Windows.old). Bytescape deletes none of these files
//! itself: each one goes through Windows, which asks the user first.

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Bytes and items in the Recycle Bins of all drives.
    pub recycle_bytes: Option<u64>,
    pub recycle_items: Option<u64>,
    /// Size of the hibernation file, if hibernation is on.
    pub hiberfil: Option<u64>,
    /// Whether a previous Windows installation is there.
    pub windows_old: bool,
    /// Usually `C:`.
    pub system_drive: String,
}

pub fn system_drive() -> String {
    std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into())
}

pub fn status() -> Status {
    let sys = system_drive();
    let (recycle_bytes, recycle_items) = match recycle_bin() {
        Some((b, n)) => (Some(b), Some(n)),
        None => (None, None),
    };
    Status {
        recycle_bytes,
        recycle_items,
        hiberfil: file_size_at_root(&sys, "hiberfil.sys"),
        windows_old: std::path::Path::new(&format!(r"{sys}\Windows.old")).is_dir(),
        system_drive: sys,
    }
}

/// Size of a file in a drive's root folder, read from the folder listing:
/// the hibernation file is always in use and can't be opened.
fn file_size_at_root(drive: &str, name: &str) -> Option<u64> {
    std::fs::read_dir(format!(r"{drive}\"))
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .and_then(|e| e.metadata().ok())
        .map(|m| m.len())
}

#[cfg(windows)]
fn recycle_bin() -> Option<(u64, u64)> {
    use windows_sys::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let mut info = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        i64Size: 0,
        i64NumItems: 0,
    };
    // SAFETY: a null root means all drives; `info` is valid with cbSize set.
    let hr = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    (hr == 0).then_some((info.i64Size.max(0) as u64, info.i64NumItems.max(0) as u64))
}

#[cfg(not(windows))]
fn recycle_bin() -> Option<(u64, u64)> {
    None
}

/// Empties the Recycle Bins of all drives. Windows shows its own
/// confirmation; returns whether it was emptied.
#[cfg(windows)]
pub fn empty_recycle_bin(owner: isize) -> bool {
    use windows_sys::Win32::UI::Shell::SHEmptyRecycleBinW;
    // SAFETY: a null root means all drives; flags 0 keeps Windows' dialogs.
    unsafe { SHEmptyRecycleBinW(owner as _, std::ptr::null(), 0) == 0 }
}

#[cfg(not(windows))]
pub fn empty_recycle_bin(_owner: isize) -> bool {
    false
}

/// Turns hibernation off, which makes Windows delete hiberfil.sys. Needs
/// administrator rights, so Windows asks first. Fast startup stops working
/// too; `powercfg /hibernate on` brings both back.
pub fn hibernation_off(owner: isize) -> Result<(), String> {
    crate::elevation::shell_execute("runas", "powercfg.exe", "/hibernate off", false, owner)
}

/// Runs DISM's component store cleanup in a console window (it takes a few
/// minutes and shows its progress). Needs administrator rights.
pub fn component_cleanup(owner: isize) -> Result<(), String> {
    crate::elevation::shell_execute(
        "runas",
        "cmd.exe",
        "/c title Bytescape: cleaning up Windows components & \
         dism.exe /Online /Cleanup-Image /StartComponentCleanup & echo. & pause",
        true,
        owner,
    )
}

/// Opens Windows' storage settings, where "Temporary files" offers to
/// remove a previous Windows installation the supported way.
pub fn open_storage_settings(owner: isize) -> Result<(), String> {
    crate::elevation::shell_execute("open", "ms-settings:storagesense", "", true, owner)
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn status_reads_without_changing_anything() {
        let s = super::status();
        assert!(s.system_drive.ends_with(':'));
        // The Recycle Bin can always be asked about.
        assert!(s.recycle_bytes.is_some());
        // The page file is in the system drive's root on most PCs and, like
        // the hibernation file, can't be opened; the listing still sizes it.
        if let Some(n) = super::file_size_at_root(&s.system_drive, "pagefile.sys") {
            assert!(n > 0);
        }
    }
}
