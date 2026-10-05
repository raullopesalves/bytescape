//! Running as administrator: needed to read a drive's MFT directly.

/// Whether this process runs with administrator rights (elevated).
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle from GetCurrentProcess needs no closing;
    // `token` is a valid out-param.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut info = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `info` is valid for its size; `token` is open.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut info as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    // SAFETY: `token` was opened above.
    unsafe { CloseHandle(token) };
    ok != 0 && info.TokenIsElevated != 0
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

/// Starts this program again as administrator (Windows asks the user first),
/// to scan `path`. Errors if the user says no or it can't start.
#[cfg(windows)]
pub fn relaunch(path: &str, owner: isize) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    // One quoted argument. A trailing backslash would escape the closing
    // quote ("C:\" reads as C:"), so double it.
    let arg = if path.ends_with('\\') {
        format!("\"{path}\\\"")
    } else {
        format!("\"{path}\"")
    };
    let (verb, file, params) = (
        wide("runas".as_ref()),
        wide(exe.as_os_str()),
        wide(arg.as_ref()),
    );
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let r = unsafe {
        ShellExecuteW(
            owner as _,
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    } as isize;
    // Values above 32 mean it started; 5 is "access denied" (the user said no).
    match r {
        r if r > 32 => Ok(()),
        5 => Err("Not started: administrator rights were not given.".into()),
        r => Err(format!("Couldn't start as administrator (code {r}).")),
    }
}

#[cfg(not(windows))]
pub fn relaunch(_path: &str, _owner: isize) -> Result<(), String> {
    Err("Only on Windows.".into())
}
