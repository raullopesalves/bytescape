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
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // One quoted argument. A trailing backslash would escape the closing
    // quote ("C:\" reads as C:"), so double it.
    let arg = if path.ends_with('\\') {
        format!("\"{path}\\\"")
    } else {
        format!("\"{path}\"")
    };
    shell_execute("runas", &exe.display().to_string(), &arg, true, owner)
}

/// Starts `file` with `params` through the shell; the verb "runas" asks
/// Windows for administrator rights first. `show` false hides its window.
#[cfg(windows)]
pub fn shell_execute(
    verb: &str,
    file: &str,
    params: &str,
    show: bool,
    owner: isize,
) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (v, f, p) = (wide(verb), wide(file), wide(params));
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let r = unsafe {
        ShellExecuteW(
            owner as _,
            v.as_ptr(),
            f.as_ptr(),
            p.as_ptr(),
            std::ptr::null(),
            if show { 1 } else { 0 }, // SW_SHOWNORMAL, SW_HIDE
        )
    } as isize;
    // Values above 32 mean it started; 5 is "access denied" (the user said no).
    match r {
        r if r > 32 => Ok(()),
        5 => Err("Not started: administrator rights were not given.".into()),
        r => Err(format!("Couldn't start {file} (code {r}).")),
    }
}

#[cfg(not(windows))]
pub fn shell_execute(
    _verb: &str,
    _file: &str,
    _params: &str,
    _show: bool,
    _owner: isize,
) -> Result<(), String> {
    Err("Only on Windows.".into())
}

#[cfg(not(windows))]
pub fn relaunch(_path: &str, _owner: isize) -> Result<(), String> {
    Err("Only on Windows.".into())
}
