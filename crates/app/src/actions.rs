//! Right-click actions that hand an item to Windows: open it, open a
//! terminal in its folder, show its Properties dialog.

use std::path::Path;

use crate::elevation::shell_execute;

/// Opens a file with its program, or a folder in Explorer.
pub fn open(path: &Path, owner: isize) -> Result<(), String> {
    shell_execute("open", &path.display().to_string(), "", true, owner)
}

/// A folder path that survives quoting: "C:\" would end in an escaped quote.
fn quotable(dir: &Path) -> String {
    let s = dir.display().to_string();
    if s.ends_with('\\') {
        format!("{s}.")
    } else {
        s
    }
}

/// Opens Windows Terminal in `dir`, or PowerShell where it isn't installed.
pub fn terminal(dir: &Path, owner: isize) -> Result<(), String> {
    let d = quotable(dir);
    shell_execute("open", "wt.exe", &format!("-d \"{d}\""), true, owner).or_else(|_| {
        let quoted = d.replace('\'', "''");
        shell_execute(
            "open",
            "powershell.exe",
            &format!("-NoExit -Command \"Set-Location -LiteralPath '{quoted}'\""),
            true,
            owner,
        )
    })
}

/// Shows Explorer's Properties dialog for `path`. Must run on the thread
/// with the window's message loop, or the dialog closes at once.
#[cfg(windows)]
pub fn properties(path: &Path, owner: isize) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{SHObjectProperties, SHOP_FILEPATH};
    let wide: Vec<u16> = path
        .display()
        .to_string()
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: `wide` is NUL-terminated; no particular page is asked for.
    let ok = unsafe {
        SHObjectProperties(
            owner as _,
            SHOP_FILEPATH as u32,
            wide.as_ptr(),
            std::ptr::null(),
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err("Couldn't show its properties.".into())
    }
}

#[cfg(not(windows))]
pub fn properties(_path: &Path, _owner: isize) -> Result<(), String> {
    Err("Only on Windows.".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn drive_roots_are_quoted_safely() {
        use std::path::Path;
        assert_eq!(super::quotable(Path::new(r"C:\")), r"C:\.");
        assert_eq!(super::quotable(Path::new(r"D:\Games")), r"D:\Games");
    }
}
