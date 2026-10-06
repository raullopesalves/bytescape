//! "Scan with Bytescape" in Explorer's right-click menu, for folders, the
//! inside of a folder and drives, for the current user only. The installer
//! adds it and its uninstaller removes it (windows/hooks.nsh); the start
//! screen can turn it on or off. On Windows 11 it's under "Show more
//! options".

/// Under HKEY_CURRENT_USER.
const CLASSES: &str = r"Software\Classes";
const KEYS: [&str; 3] = [
    r"Directory\shell\Bytescape",
    r"Directory\Background\shell\Bytescape",
    r"Drive\shell\Bytescape",
];
const LABEL: &str = "Scan with Bytescape";

fn exe() -> Result<String, String> {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

/// Whether the menu entry is there and starts this copy of the app.
pub fn is_on() -> bool {
    exe().is_ok_and(|exe| is_on_in(CLASSES, &exe))
}

pub fn set(on: bool) -> Result<(), String> {
    if on {
        add_in(CLASSES, &exe()?)
    } else {
        remove_in(CLASSES)
    }
}

fn command(exe: &str) -> String {
    format!("\"{exe}\" \"%V\"")
}

fn is_on_in(base: &str, exe: &str) -> bool {
    KEYS.iter().all(|k| {
        reg::get(&format!(r"{base}\{k}\command"))
            .is_some_and(|c| c.eq_ignore_ascii_case(&command(exe)))
    })
}

fn add_in(base: &str, exe: &str) -> Result<(), String> {
    for k in KEYS {
        let key = format!(r"{base}\{k}");
        reg::set(&key, None, LABEL)?;
        reg::set(&key, Some("Icon"), exe)?;
        reg::set(&format!(r"{key}\command"), None, &command(exe))?;
    }
    Ok(())
}

fn remove_in(base: &str) -> Result<(), String> {
    for k in KEYS {
        reg::delete_tree(&format!(r"{base}\{k}"))?;
    }
    Ok(())
}

/// The few registry calls needed, on HKEY_CURRENT_USER.
#[cfg(windows)]
mod reg {
    use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// The key's string value (`None` name: its default value).
    pub fn get(key: &str) -> Option<String> {
        let k = wide(key);
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: strings are NUL-terminated; `buf` is valid for `len` bytes.
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                k.as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r != 0 {
            return None;
        }
        let n = (len as usize / 2).saturating_sub(1); // without the NUL
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    pub fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let k = wide(key);
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: `k` is NUL-terminated; `h` is a valid out-param.
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                k.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut h,
                std::ptr::null_mut(),
            )
        };
        if r != 0 {
            return Err(format!("Couldn't write to the registry (error {r})."));
        }
        let n = name.map(wide);
        let v = wide(value);
        // SAFETY: `h` is open; `v` is valid for its length in bytes.
        let r = unsafe {
            RegSetValueExW(
                h,
                n.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                0,
                REG_SZ,
                v.as_ptr().cast(),
                (v.len() * 2) as u32,
            )
        };
        // SAFETY: `h` was opened above.
        unsafe { RegCloseKey(h) };
        if r != 0 {
            return Err(format!("Couldn't write to the registry (error {r})."));
        }
        Ok(())
    }

    /// Deletes the key and everything under it; a missing key is fine.
    pub fn delete_tree(key: &str) -> Result<(), String> {
        let k = wide(key);
        // SAFETY: `k` is NUL-terminated.
        let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, k.as_ptr()) };
        if r != 0 && r != ERROR_FILE_NOT_FOUND {
            return Err(format!("Couldn't change the registry (error {r})."));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod reg {
    pub fn get(_key: &str) -> Option<String> {
        None
    }
    pub fn set(_key: &str, _name: Option<&str>, _value: &str) -> Result<(), String> {
        Err("Only on Windows.".into())
    }
    pub fn delete_tree(_key: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Uses its own key, never the real Explorer menu.
    #[test]
    fn add_check_and_remove() {
        let base = r"Software\BytescapeTest\Classes";
        let exe = r"C:\Program Files\Bytescape\bytescape.exe";
        reg::delete_tree(r"Software\BytescapeTest").unwrap();
        assert!(!is_on_in(base, exe));
        add_in(base, exe).unwrap();
        assert!(is_on_in(base, exe));
        assert!(!is_on_in(base, r"D:\other\bytescape.exe"));
        assert_eq!(
            reg::get(&format!(r"{base}\Drive\shell\Bytescape\command")).as_deref(),
            Some(r#""C:\Program Files\Bytescape\bytescape.exe" "%V""#)
        );
        remove_in(base).unwrap();
        assert!(!is_on_in(base, exe));
        remove_in(base).unwrap(); // twice is fine
        reg::delete_tree(r"Software\BytescapeTest").unwrap();
    }
}
