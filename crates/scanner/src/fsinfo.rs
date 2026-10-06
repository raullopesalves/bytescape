//! Platform-specific folder listing: sizes on disk, hardlink identity, cloud
//! placeholders. Everything outside this module is portable.
//!
//! On Windows, folders are listed with `GetFileInformationByHandleEx`
//! (`FileIdExtdDirectoryInfo`), which returns every entry's file ID and
//! allocated size in one call per folder. Hardlinks are then found by
//! matching file IDs, without opening any file. (`FileIdBothDirectoryInfo`
//! also returns 8.3 short names; looking those up made it ~3x slower.) If
//! that call is not supported (some network or non-NTFS drives), the
//! portable listing is used.

use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::tree::NodeKind;

/// Identifies one physical file across all its hardlinks: (volume, file id).
/// The 128-bit id is kept as two `u64` halves: a `u128` would make the key
/// 16-byte aligned and 32 bytes instead of 24, and there is one per file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileKey(pub u64, pub [u64; 2]);

impl FileKey {
    pub fn new(volume: u64, id: u128) -> Self {
        FileKey(volume, [(id >> 64) as u64, id as u64])
    }
}

/// Per-scan facts about the volume being scanned.
#[derive(Clone, Copy, Debug)]
pub struct VolumeInfo {
    /// Allocation unit in bytes, used to round sizes up to size on disk.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub cluster: u64,
    /// Volume serial number, the first half of every `FileKey`. A scan stays
    /// on one volume: mount points are reparse points and are never followed.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub serial: Option<u64>,
}

/// One entry of a folder listing.
#[derive(Debug)]
pub struct Entry {
    pub name: Box<str>,
    pub kind: NodeKind,
    pub size: u64,
    pub alloc: u64,
    pub modified: i64,
    /// Physical file identity, for hardlink matching. Files only.
    pub key: Option<FileKey>,
    pub flags: u8,
    pub error: Option<Box<str>>,
    /// Full path; filled in for folders only (empty otherwise).
    pub path: PathBuf,
}

impl Entry {
    fn new(name: Box<str>, kind: NodeKind) -> Self {
        Entry {
            name,
            kind,
            size: 0,
            alloc: 0,
            modified: 0,
            key: None,
            flags: 0,
            error: None,
            path: PathBuf::new(),
        }
    }
}

/// A folder's entries, plus an error if listing stopped part-way.
#[derive(Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub error: Option<Box<str>>,
}

/// Lists one folder. `Err` means the folder could not be opened at all.
pub fn list_dir(path: &Path, vol: &VolumeInfo, hardlinks: bool, fast: bool) -> io::Result<Listing> {
    imp::list_dir(path, vol, hardlinks, fast)
}

pub fn volume_info(root: &Path) -> VolumeInfo {
    imp::volume_info(root)
}

pub fn modified_secs(md: &Metadata) -> i64 {
    md.modified()
        .ok()
        .map(|t| match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        })
        .unwrap_or(0)
}

/// Portable listing with `std::fs::read_dir`. `extra` fills in per-file
/// details (size on disk, hardlink key, flags) for the platform.
fn list_dir_std(path: &Path, extra: impl Fn(&Path, &Metadata, &mut Entry)) -> io::Result<Listing> {
    let mut listing = Listing::default();
    for entry in fs::read_dir(path)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                listing.error = Some(e.to_string().into());
                break;
            }
        };
        let name: Box<str> = entry.file_name().to_string_lossy().into();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                let mut r = Entry::new(name, NodeKind::Error);
                r.error = Some(e.to_string().into());
                listing.entries.push(r);
                continue;
            }
        };
        // `metadata` on a DirEntry does not follow symlinks.
        let md = entry.metadata();
        let kind = if ft.is_symlink() {
            NodeKind::Symlink
        } else if ft.is_dir() {
            NodeKind::Dir
        } else {
            NodeKind::File
        };
        let mut r = Entry::new(name, kind);
        match md {
            Ok(md) => {
                r.modified = modified_secs(&md);
                match kind {
                    NodeKind::Dir => r.path = entry.path(),
                    NodeKind::File => {
                        r.size = md.len();
                        extra(&entry.path(), &md, &mut r);
                    }
                    _ => {}
                }
            }
            Err(e) if kind == NodeKind::File => {
                r.kind = NodeKind::Error;
                r.error = Some(e.to_string().into());
            }
            Err(_) => {
                if kind == NodeKind::Dir {
                    r.path = entry.path();
                }
            }
        }
        listing.entries.push(r);
    }
    Ok(listing)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use crate::tree::flags;
    use std::os::unix::fs::MetadataExt;

    pub fn volume_info(_root: &Path) -> VolumeInfo {
        VolumeInfo {
            cluster: 4096,
            serial: None,
        }
    }

    pub fn list_dir(
        path: &Path,
        _vol: &VolumeInfo,
        hardlinks: bool,
        _fast: bool,
    ) -> io::Result<Listing> {
        list_dir_std(path, |_, md, e| {
            e.alloc = md.blocks() * 512;
            if hardlinks && md.nlink() > 1 {
                e.key = Some(FileKey::new(md.dev(), md.ino().into()));
            }
            // Far fewer blocks than its length means a sparse file.
            if md.len() > 0 && e.alloc + 4096 < md.len() {
                e.flags |= flags::SPARSE;
            }
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub fn volume_info(_root: &Path) -> VolumeInfo {
        VolumeInfo {
            cluster: 4096,
            serial: None,
        }
    }

    pub fn list_dir(
        path: &Path,
        _vol: &VolumeInfo,
        _hardlinks: bool,
        _fast: bool,
    ) -> io::Result<Listing> {
        list_dir_std(path, |_, md, e| e.alloc = md.len())
    }
}

#[cfg(windows)]
pub(crate) mod imp {
    use super::*;
    use crate::tree::flags;
    use std::cell::RefCell;
    use std::ffi::{OsStr, OsString};
    use std::fs::OpenOptions;
    use std::mem::offset_of;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdExtdDirectoryInfo, GetCompressedFileSizeW, GetDiskFreeSpaceW,
        GetFileInformationByHandle, GetFileInformationByHandleEx, GetVolumeInformationW,
        GetVolumePathNameW, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO,
    };

    const ATTR_DIRECTORY: u32 = 0x0000_0010;
    const ATTR_SPARSE: u32 = 0x0000_0200;
    const ATTR_REPARSE_POINT: u32 = 0x0000_0400;
    const ATTR_COMPRESSED: u32 = 0x0000_0800;
    const ATTR_OFFLINE: u32 = 0x0000_1000;
    const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    /// Reparse tags with this bit (symlinks, junctions, mount points) point elsewhere.
    const TAG_NAME_SURROGATE: u32 = 0x2000_0000;
    const INVALID_FILE_SIZE: u32 = 0xFFFF_FFFF;
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_NO_MORE_FILES: i32 = 18;
    /// Seconds between 1601-01-01 (Windows epoch) and 1970-01-01.
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;

    /// NUL-terminated UTF-16 path with the `\\?\` prefix so long paths work.
    fn wide(path: &Path) -> Vec<u16> {
        let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
        let starts = |p: &str| raw.starts_with(&p.encode_utf16().collect::<Vec<_>>());
        let mut out: Vec<u16> = if starts(r"\\?\") || !path.is_absolute() {
            raw
        } else if starts(r"\\") {
            // \\server\share -> \\?\UNC\server\share
            let mut v: Vec<u16> = OsStr::new(r"\\?\UNC\").encode_wide().collect();
            v.extend_from_slice(&raw[2..]);
            v
        } else {
            let mut v: Vec<u16> = OsStr::new(r"\\?\").encode_wide().collect();
            v.extend_from_slice(&raw);
            v
        };
        out.push(0);
        out
    }

    pub fn volume_info(root: &Path) -> VolumeInfo {
        let mut cluster = 4096u64;
        let mut serial = None;
        let name = wide(root);
        let mut vol = vec![0u16; 1024];
        // SAFETY: both buffers are valid for the lengths passed.
        let ok = unsafe { GetVolumePathNameW(name.as_ptr(), vol.as_mut_ptr(), vol.len() as u32) };
        if ok != 0 {
            let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
            // SAFETY: `vol` is NUL-terminated by the call above; out-params are valid.
            let ok = unsafe {
                GetDiskFreeSpaceW(vol.as_ptr(), &mut spc, &mut bps, &mut free, &mut total)
            };
            if ok != 0 && spc > 0 && bps > 0 {
                cluster = spc as u64 * bps as u64;
            }
            let mut sn = 0u32;
            // SAFETY: `vol` is NUL-terminated; `sn` is a valid out-param and
            // the other outputs are null with zero length, which is allowed.
            let ok = unsafe {
                GetVolumeInformationW(
                    vol.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    &mut sn,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if ok != 0 {
                serial = Some(sn as u64);
            }
        }
        VolumeInfo { cluster, serial }
    }

    pub fn list_dir(
        path: &Path,
        vol: &VolumeInfo,
        hardlinks: bool,
        fast: bool,
    ) -> io::Result<Listing> {
        if !fast {
            return list_dir_slow(path, vol, hardlinks);
        }
        match list_dir_fast(path, vol, hardlinks)? {
            Some(listing) => Ok(listing),
            None => list_dir_slow(path, vol, hardlinks),
        }
    }

    /// One call per folder: names, attributes, sizes and file IDs together.
    /// `Ok(None)` means this volume does not support it; use the slow path.
    pub(crate) fn list_dir_fast(
        path: &Path,
        vol: &VolumeInfo,
        hardlinks: bool,
    ) -> io::Result<Option<Listing>> {
        let dir = OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | SYNCHRONIZE)
            .share_mode(7) // read | write | delete
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let handle = dir.as_raw_handle();

        thread_local! {
            // 64 KB, 8-byte aligned as the entries require. One per thread,
            // reused for every folder that thread lists.
            static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0u64; 8 * 1024]);
        }
        BUF.with_borrow_mut(|buf| read_entries(path, handle, buf, vol, hardlinks))
    }

    /// Reads every entry of the open folder `handle` through `buf`.
    fn read_entries(
        path: &Path,
        handle: HANDLE,
        buf: &mut [u64],
        vol: &VolumeInfo,
        hardlinks: bool,
    ) -> io::Result<Option<Listing>> {
        let mut listing = Listing::default();
        let buf_bytes = (buf.len() * 8) as u32;
        let mut first_call = true;

        loop {
            // SAFETY: `buf` is valid and writable for `buf_bytes` bytes.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FileIdExtdDirectoryInfo,
                    buf.as_mut_ptr().cast(),
                    buf_bytes,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                    break;
                }
                if first_call {
                    // Not supported here (or failed before returning anything).
                    return Ok(None);
                }
                listing.error = Some(err.to_string().into());
                break;
            }
            first_call = false;

            let base = buf.as_ptr() as *const u8;
            let mut offset = 0usize;
            loop {
                // SAFETY: the OS wrote a chain of entries into `buf`; `offset`
                // follows its NextEntryOffset links, which stay inside the buffer.
                let info = unsafe {
                    std::ptr::read_unaligned(base.add(offset) as *const FILE_ID_EXTD_DIR_INFO)
                };
                let name_len = info.FileNameLength as usize / 2;
                // SAFETY: the name follows the fixed part of the entry, `name_len` units long.
                let name16 = unsafe {
                    std::slice::from_raw_parts(
                        base.add(offset + offset_of!(FILE_ID_EXTD_DIR_INFO, FileName))
                            as *const u16,
                        name_len,
                    )
                };
                if name16 != [b'.' as u16] && name16 != [b'.' as u16, b'.' as u16] {
                    listing
                        .entries
                        .push(entry_from_info(path, &info, name16, vol, hardlinks));
                }
                if info.NextEntryOffset == 0 {
                    break;
                }
                offset += info.NextEntryOffset as usize;
            }
        }
        Ok(Some(listing))
    }

    fn entry_from_info(
        dir: &Path,
        info: &FILE_ID_EXTD_DIR_INFO,
        name16: &[u16],
        vol: &VolumeInfo,
        hardlinks: bool,
    ) -> Entry {
        let attrs = info.FileAttributes;
        let is_reparse = attrs & ATTR_REPARSE_POINT != 0;
        let surrogate = is_reparse && info.ReparsePointTag & TAG_NAME_SURROGATE != 0;
        let kind = if surrogate {
            NodeKind::Symlink
        } else if attrs & ATTR_DIRECTORY != 0 {
            NodeKind::Dir
        } else {
            NodeKind::File
        };

        let name: Box<str> = String::from_utf16_lossy(name16).into();
        let mut e = Entry::new(name, kind);
        e.modified = info.LastWriteTime / 10_000_000 - EPOCH_DIFF_SECS;

        match kind {
            NodeKind::Dir => {
                e.path = dir.join(OsString::from_wide(name16));
            }
            NodeKind::File => {
                e.size = info.EndOfFile.max(0) as u64;
                let (alloc, fl) = file_alloc(attrs, e.size, info.AllocationSize, vol, || {
                    dir.join(OsString::from_wide(name16))
                });
                e.alloc = alloc;
                e.flags = fl;
                // 128-bit on ReFS; NTFS fills only the low 64 bits.
                let id = u128::from_le_bytes(info.FileId.Identifier);
                if hardlinks && id != 0 && fl & flags::CLOUD_PLACEHOLDER == 0 {
                    if let Some(serial) = vol.serial {
                        e.key = Some(FileKey::new(serial, id));
                    }
                }
            }
            _ => {}
        }
        e
    }

    /// Size on disk and flags for a file, from its attributes.
    fn file_alloc(
        attrs: u32,
        len: u64,
        reported: i64,
        vol: &VolumeInfo,
        path: impl FnOnce() -> PathBuf,
    ) -> (u64, u8) {
        let mut fl = 0u8;
        if attrs & (ATTR_RECALL_ON_DATA_ACCESS | ATTR_RECALL_ON_OPEN | ATTR_OFFLINE) != 0 {
            // Online-only: no local data. Never open it, so nothing is downloaded.
            return (0, flags::CLOUD_PLACEHOLDER);
        }
        if attrs & (ATTR_COMPRESSED | ATTR_SPARSE) != 0 {
            if attrs & ATTR_COMPRESSED != 0 {
                fl |= flags::COMPRESSED;
            }
            if attrs & ATTR_SPARSE != 0 {
                fl |= flags::SPARSE;
            }
            if let Some(n) = compressed_size(&path()) {
                return (n, fl);
            }
        }
        let alloc = if reported >= 0 {
            let reported = reported as u64;
            // Clusters are allocated whole. Anything else is a tiny file kept
            // inside its MFT record (the listing gives its length rounded to
            // 8): it uses no clusters, and Explorer shows 0 for it too.
            if vol.cluster > 0 && !reported.is_multiple_of(vol.cluster) {
                0
            } else {
                reported
            }
        } else {
            round_up(len, vol.cluster)
        };
        (alloc, fl)
    }

    fn compressed_size(path: &Path) -> Option<u64> {
        let name = wide(path);
        let mut high = 0u32;
        // SAFETY: `name` is NUL-terminated; `high` is a valid out-param.
        let low = unsafe { GetCompressedFileSizeW(name.as_ptr(), &mut high) };
        if low == INVALID_FILE_SIZE && io::Error::last_os_error().raw_os_error() != Some(0) {
            return None;
        }
        Some(((high as u64) << 32) | low as u64)
    }

    /// Fallback: std listing, opening each file to read its link count.
    pub(crate) fn list_dir_slow(
        path: &Path,
        vol: &VolumeInfo,
        hardlinks: bool,
    ) -> io::Result<Listing> {
        list_dir_std(path, |p, md, e| {
            let attrs = md.file_attributes();
            let (alloc, fl) = file_alloc(attrs, md.len(), -1, vol, || p.to_path_buf());
            e.alloc = alloc;
            e.flags = fl;
            if hardlinks && fl & flags::CLOUD_PLACEHOLDER == 0 {
                if let Some((key, links)) = link_info(p) {
                    if links > 1 {
                        e.key = Some(key);
                    }
                }
            }
        })
    }

    fn link_info(path: &Path) -> Option<(FileKey, u32)> {
        // Access mode 0 reads attributes only; works on files in use.
        let file = OpenOptions::new()
            .access_mode(0)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .ok()?;
        // SAFETY: zeroed is a valid bit pattern for this plain C struct.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open for the lifetime of `file`; `info` is valid.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Some((
            FileKey::new(info.dwVolumeSerialNumber as u64, index.into()),
            info.nNumberOfLinks,
        ))
    }

    fn round_up(len: u64, cluster: u64) -> u64 {
        if cluster == 0 {
            return len;
        }
        len.div_ceil(cluster) * cluster
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::imp::{list_dir_fast, list_dir_slow};
    use super::*;
    use std::process::Command;

    fn sorted(mut l: Listing) -> Vec<Entry> {
        l.entries.sort_by(|a, b| a.name.cmp(&b.name));
        l.entries
    }

    #[test]
    fn fast_listing_matches_std_listing() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::write(r.join("a.bin"), vec![1u8; 5000]).unwrap();
        fs::write(r.join("empty.txt"), b"").unwrap();
        fs::write(r.join("ünïcode name.txt"), b"hello").unwrap();
        fs::create_dir(r.join("sub")).unwrap();
        fs::hard_link(r.join("a.bin"), r.join("link.bin")).unwrap();

        let vol = volume_info(r);
        let fast = sorted(
            list_dir_fast(r, &vol, true)
                .unwrap()
                .expect("fast listing supported"),
        );
        let slow = sorted(list_dir_slow(r, &vol, true).unwrap());

        let names = |v: &[Entry]| v.iter().map(|e| e.name.to_string()).collect::<Vec<_>>();
        assert_eq!(names(&fast), names(&slow));
        for (f, s) in fast.iter().zip(&slow) {
            assert_eq!(f.kind, s.kind, "{}", f.name);
            assert_eq!(f.size, s.size, "{}", f.name);
            assert!((f.modified - s.modified).abs() <= 1, "{}", f.name);
        }
        let sub = fast.iter().find(|e| &*e.name == "sub").unwrap();
        assert_eq!(sub.kind, NodeKind::Dir);
        assert_eq!(sub.path, r.join("sub"));

        // Both links share one key; other files have different keys.
        let key = |n: &str| fast.iter().find(|e| &*e.name == n).unwrap().key;
        assert!(key("a.bin").is_some());
        assert_eq!(key("a.bin"), key("link.bin"));
        assert_ne!(key("a.bin"), key("empty.txt"));
        // The slow path agrees on which files are linked.
        let slow_key = |n: &str| slow.iter().find(|e| &*e.name == n).unwrap().key;
        assert_eq!(slow_key("a.bin"), slow_key("link.bin"));
    }

    #[test]
    fn files_inside_their_mft_record_use_no_clusters() {
        // Explorer shows 0 bytes on disk for the two small ones, and whole
        // clusters for the others (checked by hand on NTFS, 4 KB clusters).
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for (name, len) in [("100", 100), ("600", 600), ("1000", 1000), ("5000", 5000)] {
            fs::write(r.join(name), vec![0u8; len]).unwrap();
        }
        let vol = volume_info(r);
        let fast = list_dir_fast(r, &vol, false)
            .unwrap()
            .expect("fast listing supported");
        let alloc = |n: &str| fast.entries.iter().find(|e| &*e.name == n).unwrap().alloc;
        assert_eq!(alloc("100"), 0);
        assert_eq!(alloc("600"), 0);
        for n in ["1000", "5000"] {
            assert!(
                alloc(n) > 0 && alloc(n) % vol.cluster == 0,
                "{n}: {}",
                alloc(n)
            );
        }
    }

    #[test]
    fn junctions_are_listed_as_links() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir(r.join("target")).unwrap();
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(r.join("junction"))
            .arg(r.join("target"))
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink failed: {status:?}");

        let vol = volume_info(r);
        let fast = sorted(list_dir_fast(r, &vol, true).unwrap().unwrap());
        let j = fast.iter().find(|e| &*e.name == "junction").unwrap();
        assert_eq!(j.kind, NodeKind::Symlink);
        let t = fast.iter().find(|e| &*e.name == "target").unwrap();
        assert_eq!(t.kind, NodeKind::Dir);
    }
}
