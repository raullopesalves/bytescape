//! The drives to offer on the start screen.

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    /// Root path, for example `C:\`.
    pub path: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

/// Fixed, removable and network drives that are ready (have media).
#[cfg(windows)]
pub fn list() -> Vec<Drive> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
    };
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;

    let mut buf = vec![0u16; 512];
    // SAFETY: `buf` is valid for its length.
    let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
    if n == 0 || n > buf.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // "C:\\\0D:\\\0\0": NUL-separated roots.
    for root in buf[..n].split(|&c| c == 0).filter(|r| !r.is_empty()) {
        let mut wide = root.to_vec();
        wide.push(0);
        // SAFETY: `wide` is NUL-terminated.
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if !matches!(kind, DRIVE_REMOVABLE | DRIVE_FIXED | DRIVE_REMOTE) {
            continue;
        }
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: `wide` is NUL-terminated; the out-params are valid or null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
        };
        if ok == 0 {
            continue; // no media
        }
        let mut name = vec![0u16; 261];
        // SAFETY: `name` is valid for its length; other outputs are null.
        let ok = unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        let label = if ok != 0 {
            let len = name.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf16_lossy(&name[..len])
        } else {
            String::new()
        };
        out.push(Drive {
            path: String::from_utf16_lossy(root),
            label,
            total,
            free,
        });
    }
    out
}

#[cfg(not(windows))]
pub fn list() -> Vec<Drive> {
    Vec::new()
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn the_system_drive_is_listed() {
        let drives = super::list();
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let d = drives
            .iter()
            .find(|d| d.path.eq_ignore_ascii_case(&format!("{sys}\\")))
            .expect("system drive listed");
        assert!(d.total > 0 && d.free <= d.total);
    }
}
