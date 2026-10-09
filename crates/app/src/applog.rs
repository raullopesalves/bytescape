//! A small log file, so a problem on someone's PC can be looked into: what
//! was scanned (path, counts, time, method), deletes (and what failed),
//! Windows cleanups, update problems, errors in the window and crashes. It
//! stays on the PC, in the app's local data folder; About opens it. About
//! 1 MB is kept, plus the one before as `bytescape.old.log`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1 << 20;

static LOG: Mutex<Option<(File, PathBuf)>> = Mutex::new(None);

/// Opens (or starts) the log in `dir` and sends crashes to it.
pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("bytescape.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, dir.join("bytescape.old.log"));
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        *LOG.lock().unwrap() = Some((f, path));
    }
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write(&format!("crash: {info}"));
        default(info);
    }));
}

/// Adds one line, with the time (UTC). Line breaks in `msg` become " | ".
pub fn write(msg: &str) {
    // A poisoned lock (a crash while writing) just skips the line.
    if let Ok(mut log) = LOG.lock() {
        if let Some((f, _)) = log.as_mut() {
            let _ = writeln!(f, "{} {}", now(), msg.replace(['\r', '\n'], " | "));
        }
    }
}

pub fn path() -> Option<PathBuf> {
    LOG.lock().ok()?.as_ref().map(|(_, p)| p.clone())
}

/// `YYYY-MM-DD HH:MM:SSZ`.
fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    stamp(secs)
}

fn stamp(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// Days since 1970-01-01 to a calendar date (proleptic Gregorian), after
/// Howard Hinnant's well-known public-domain `civil_from_days`.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamps() {
        assert_eq!(super::stamp(0), "1970-01-01 00:00:00Z");
        assert_eq!(super::stamp(951_782_400), "2000-02-29 00:00:00Z");
        assert_eq!(super::stamp(1_791_536_645), "2026-10-09 09:04:05Z");
    }
}
