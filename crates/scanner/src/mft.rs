//! Whole-volume scan from the NTFS Master File Table.
//!
//! Instead of listing folders one by one, this reads the MFT (one record per
//! file or folder) straight from the volume, in large chunks and in
//! parallel, and builds the same [`ScanTree`] as the folder walk. It needs
//! administrator rights to open the volume; callers fall back to the walk on
//! any [`MftError`].
//!
//! Matches what a folder listing shows: NTFS metadata files (`$MFT`,
//! `$Extend`, ...) are left out, 8.3 short names are skipped, each hardlink
//! name becomes its own entry (counted once by `compute_totals`), and links
//! and junctions are listed but have no children. The parsing works on byte
//! buffers and is tested without a real volume.

use std::fmt;
use std::io;

/// Why the MFT could not be used.
#[derive(Debug)]
pub enum MftError {
    NeedsAdmin,
    NotNtfs,
    /// The folder was not found in the MFT (or is not on a local volume).
    NotFound,
    Unsupported(String),
    Cancelled,
    Io(io::Error),
}

impl fmt::Display for MftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MftError::NeedsAdmin => write!(f, "reading the MFT needs administrator rights"),
            MftError::NotNtfs => write!(f, "the drive is not NTFS"),
            MftError::NotFound => write!(f, "the folder was not found in the MFT"),
            MftError::Unsupported(why) => write!(f, "MFT not usable: {why}"),
            MftError::Cancelled => write!(f, "cancelled"),
            MftError::Io(e) => write!(f, "MFT read failed: {e}"),
        }
    }
}

// ---------- record parsing ----------

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE_POINT: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;
/// File name namespace: DOS (8.3) only. Skipped, like a folder listing does.
const NAMESPACE_DOS: u8 = 2;
/// The MFT's fixups protect every 512 bytes, whatever the sector size.
const FIXUP_STRIDE: usize = 512;
/// Low 48 bits of a file reference are the record number.
const REF_MASK: u64 = 0xFFFF_FFFF_FFFF;

/// One name (hardlink) of a file or folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Name {
    pub parent: u64,
    pub name: String,
    pub namespace: u8,
    /// Reparse tag from the name's duplicated information (0 if none); the
    /// $REPARSE_POINT attribute's tag is preferred when there is one.
    pub reparse: u32,
}

/// What one MFT record says. Extension records (`base != record`) carry
/// attributes that belong to their base record.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub record: u64,
    pub base: u64,
    pub is_dir: bool,
    pub names: Vec<Name>,
    /// File attributes and last write time (FILETIME) from $STANDARD_INFORMATION.
    pub si: Option<(u32, u64)>,
    /// Unnamed $DATA: logical size and bytes allocated on disk.
    pub data: Option<(u64, u64)>,
    /// Bytes allocated to the "WofCompressedData" stream of a file that
    /// Windows compressed with WOF (CompactOS).
    pub wof: Option<u64>,
    /// Tag of a resident $REPARSE_POINT attribute.
    pub reparse: Option<u32>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Little-endian UTF-16 bytes to a string.
fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Undoes the update sequence ("fixups"): the last two bytes of every
/// 512-byte stride were swapped out on write. False if the record is torn.
pub(crate) fn apply_fixups(rec: &mut [u8]) -> bool {
    let (Some(usa_ofs), Some(usa_count)) = (u16_at(rec, 4), u16_at(rec, 6)) else {
        return false;
    };
    let (usa_ofs, usa_count) = (usa_ofs as usize, usa_count as usize);
    let strides = rec.len() / FIXUP_STRIDE;
    if usa_count != strides + 1 || usa_ofs + 2 * usa_count > rec.len() {
        return false;
    }
    let check = [rec[usa_ofs], rec[usa_ofs + 1]];
    for i in 0..strides {
        let end = (i + 1) * FIXUP_STRIDE - 2;
        if rec[end..end + 2] != check {
            return false;
        }
        let src = usa_ofs + 2 * (i + 1);
        rec[end] = rec[src];
        rec[end + 1] = rec[src + 1];
    }
    true
}

/// Parses record number `number`. `None` for unused, torn or non-FILE records.
pub(crate) fn parse_record(rec: &mut [u8], number: u64) -> Option<Parsed> {
    if rec.get(0..4)? != b"FILE" || !apply_fixups(rec) {
        return None;
    }
    let flags = u16_at(rec, 0x16)?;
    if flags & 0x01 == 0 {
        return None; // not in use
    }
    let used = (u32_at(rec, 0x18)? as usize).min(rec.len());
    let base = u64_at(rec, 0x20)? & REF_MASK;
    let mut p = Parsed {
        record: number,
        base: if base == 0 { number } else { base },
        is_dir: flags & 0x02 != 0,
        ..Parsed::default()
    };

    let mut off = u16_at(rec, 0x14)? as usize;
    while off + 16 <= used {
        let kind = u32_at(rec, off)?;
        if kind == ATTR_END {
            break;
        }
        let len = u32_at(rec, off + 4)? as usize;
        if len < 16 || off + len > used {
            break; // damaged; keep what we have
        }
        let a = &rec[off..off + len];
        let non_resident = a[8] != 0;
        let name_len = a[9] as usize;
        let attr_flags = u16_at(a, 0x0C)?;
        let value = || -> Option<&[u8]> {
            let vlen = u32_at(a, 0x10)? as usize;
            let vofs = u16_at(a, 0x14)? as usize;
            a.get(vofs..vofs + vlen)
        };
        match kind {
            ATTR_STANDARD_INFORMATION if !non_resident => {
                if let Some(v) = value() {
                    p.si = Some((u32_at(v, 0x20).unwrap_or(0), u64_at(v, 0x08)?));
                }
            }
            ATTR_FILE_NAME if !non_resident => {
                if let Some(v) = value() {
                    let chars = *v.get(0x40)? as usize;
                    p.names.push(Name {
                        parent: u64_at(v, 0)? & REF_MASK,
                        name: utf16(v.get(0x42..0x42 + 2 * chars)?),
                        namespace: *v.get(0x41)?,
                        reparse: u32_at(v, 0x3C)?,
                    });
                }
            }
            ATTR_DATA if name_len == 0 => {
                if !non_resident {
                    // Stored inside the record: no clusters of its own, so
                    // nothing on disk (as Explorer shows it).
                    p.data = Some((value().map_or(0, |v| v.len() as u64), 0));
                } else if u64_at(a, 0x10)? == 0 {
                    // Only the first piece (starting VCN 0) has the sizes.
                    let size = u64_at(a, 0x30)?;
                    let packed = attr_flags & (0x0001 | 0x8000) != 0; // compressed or sparse
                    let alloc = if packed && len >= 0x48 {
                        u64_at(a, 0x40)?
                    } else {
                        u64_at(a, 0x28)?
                    };
                    p.data = Some((size, alloc));
                }
            }
            ATTR_DATA if non_resident && u64_at(a, 0x10)? == 0 => {
                let name_ofs = u16_at(a, 0x0A)? as usize;
                if utf16(a.get(name_ofs..name_ofs + 2 * name_len)?) == "WofCompressedData" {
                    p.wof = Some(u64_at(a, 0x28)?);
                }
            }
            ATTR_REPARSE_POINT if !non_resident => {
                p.reparse = value().and_then(|v| u32_at(v, 0));
            }
            _ => {}
        }
        off += len;
    }
    Some(p)
}

/// A run of clusters: `lcn` is `None` for a sparse run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    pub lcn: Option<i64>,
    pub clusters: u64,
}

/// Decodes a non-resident attribute's mapping pairs.
pub(crate) fn parse_runs(b: &[u8]) -> Option<Vec<Run>> {
    let mut out = Vec::new();
    let (mut i, mut lcn) = (0usize, 0i64);
    while i < b.len() && b[i] != 0 {
        let (len_size, ofs_size) = ((b[i] & 0x0F) as usize, (b[i] >> 4) as usize);
        i += 1;
        if len_size == 0 || len_size > 8 || ofs_size > 8 || i + len_size + ofs_size > b.len() {
            return None;
        }
        let mut clusters = 0u64;
        for (k, &byte) in b[i..i + len_size].iter().enumerate() {
            clusters |= (byte as u64) << (8 * k);
        }
        i += len_size;
        if ofs_size == 0 {
            out.push(Run {
                lcn: None,
                clusters,
            });
            continue;
        }
        let mut delta = 0i64;
        for (k, &byte) in b[i..i + ofs_size].iter().enumerate() {
            delta |= (byte as i64) << (8 * k);
        }
        // Sign-extend from the field's width.
        let shift = 64 - 8 * ofs_size as u32;
        delta = (delta << shift) >> shift;
        i += ofs_size;
        lcn += delta;
        out.push(Run {
            lcn: Some(lcn),
            clusters,
        });
    }
    Some(out)
}

/// The unnamed $DATA runs of record 0 (the MFT's own location on disk).
pub(crate) fn mft_runs(rec0: &mut [u8]) -> Option<Vec<Run>> {
    if rec0.get(0..4)? != b"FILE" || !apply_fixups(rec0) {
        return None;
    }
    let used = (u32_at(rec0, 0x18)? as usize).min(rec0.len());
    let mut off = u16_at(rec0, 0x14)? as usize;
    while off + 16 <= used {
        let kind = u32_at(rec0, off)?;
        let len = u32_at(rec0, off + 4)? as usize;
        if kind == ATTR_END || len < 16 || off + len > used {
            return None;
        }
        let a = &rec0[off..off + len];
        if kind == ATTR_DATA && a[8] != 0 && a[9] == 0 {
            let runs_ofs = u16_at(a, 0x20)? as usize;
            return parse_runs(a.get(runs_ofs..)?);
        }
        off += len;
    }
    None
}

// ---------- building the tree ----------

#[cfg(windows)]
pub(crate) use windows::scan;

#[cfg(not(windows))]
pub(crate) fn scan(
    _root: &std::path::Path,
    _opts: &crate::ScanOptions,
) -> Result<crate::ScanTree, MftError> {
    Err(MftError::Unsupported("only on Windows".into()))
}

/// Whether `path` is the root of a volume (where the MFT scan pays off).
#[cfg(windows)]
pub(crate) fn is_volume_root(path: &std::path::Path) -> bool {
    windows::volume_path(path).is_some_and(|v| same_path(&v, &path.to_string_lossy()))
}

#[cfg(not(windows))]
pub(crate) fn is_volume_root(_path: &std::path::Path) -> bool {
    false
}

#[cfg(windows)]
fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.strip_prefix(r"\\?\").unwrap_or(s).replace('/', "\\");
        s.trim_end_matches('\\').to_lowercase()
    };
    norm(a) == norm(b)
}

/// Per-record facts once extension records are merged in.
#[derive(Default, Clone, Copy)]
struct Rec {
    in_use: bool,
    is_dir: bool,
    attrs: u32,
    reparse: u32,
    modified: u64,
    size: u64,
    alloc: u64,
}

/// One name of a record, under its parent folder.
struct Link {
    parent: u32,
    child: u32,
    name: Box<str>,
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::fsinfo::FileKey;
    use crate::tree::{flags, Node, NodeId, NodeKind, ScanStats, ScanTree};
    use crate::ScanOptions;
    use rayon::prelude::*;
    use std::collections::VecDeque;
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{FileExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Component, Path};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;
    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    /// Bytes read per request; a multiple of any record and cluster size.
    const CHUNK: u64 = 4 << 20;
    const ROOT_RECORD: u64 = 5;
    /// Records below this are NTFS's own files ($MFT, $Bitmap, ...).
    const FIRST_USER_RECORD: u64 = 16;
    const ATTR_COMPRESSED: u32 = 0x800;
    const ATTR_SPARSE: u32 = 0x200;
    /// Online-only cloud file: recall on data access, or offline. (On disk,
    /// 0x40000 means "has extended attributes", not "recall on open".)
    const ATTR_CLOUD: u32 = 0x0040_0000 | 0x0000_1000;
    const TAG_NAME_SURROGATE: u32 = 0x2000_0000;
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain([0]).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    /// The volume root containing `path`, like `C:\`.
    pub(crate) fn volume_path(path: &Path) -> Option<String> {
        let p = path.to_string_lossy();
        let p = p.strip_prefix(r"\\?\").unwrap_or(&p);
        let name = wide(p);
        let mut out = vec![0u16; 1024];
        // SAFETY: both buffers are valid for the lengths passed.
        let ok = unsafe { GetVolumePathNameW(name.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
        (ok != 0).then(|| from_wide(&out))
    }

    pub(crate) fn scan(root: &Path, opts: &ScanOptions) -> Result<ScanTree, MftError> {
        let vol_root = volume_path(root).ok_or(MftError::NotFound)?;
        // "\\?\Volume{guid}\" names the volume; without the slash it opens the device.
        let mut guid = vec![0u16; 64];
        let w = wide(&vol_root);
        // SAFETY: `w` is NUL-terminated; `guid` is valid for its length.
        let ok = unsafe {
            GetVolumeNameForVolumeMountPointW(w.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
        };
        if ok == 0 {
            return Err(MftError::Unsupported("not a local volume".into()));
        }
        let device = from_wide(&guid).trim_end_matches('\\').to_string();
        let vol = OpenOptions::new()
            .read(true)
            .share_mode(3) // read | write
            .open(&device)
            .map_err(|e| match e.raw_os_error() {
                Some(5) => MftError::NeedsAdmin,
                _ => MftError::Io(e),
            })?;

        // SAFETY: zeroed is valid for this plain C struct.
        let mut nv: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
        let mut got = 0u32;
        // SAFETY: the handle is open; `nv` is valid for its size.
        let ok = unsafe {
            DeviceIoControl(
                vol.as_raw_handle(),
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null(),
                0,
                (&mut nv as *mut NTFS_VOLUME_DATA_BUFFER).cast(),
                std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(MftError::NotNtfs);
        }
        let cluster = nv.BytesPerCluster as u64;
        let rec_size = nv.BytesPerFileRecordSegment as u64;
        let valid = nv.MftValidDataLength as u64;
        if rec_size == 0
            || cluster == 0
            || !CHUNK.is_multiple_of(rec_size)
            || !CHUNK.is_multiple_of(cluster)
        {
            return Err(MftError::Unsupported(
                "unusual record or cluster size".into(),
            ));
        }

        // Record 0 describes the MFT itself: where its pieces are on disk.
        let mut rec0 = vec![0u8; rec_size as usize];
        read_at(&vol, &mut rec0, nv.MftStartLcn as u64 * cluster).map_err(MftError::Io)?;
        let runs =
            mft_runs(&mut rec0).ok_or(MftError::Unsupported("unreadable MFT record 0".into()))?;
        // (disk offset, first record, bytes holding records, bytes to read).
        // Reads are whole clusters, which keeps them sector-aligned.
        let mut chunks = Vec::new();
        let mut mft_pos = 0u64;
        for run in &runs {
            let lcn = run.lcn.ok_or(MftError::Unsupported("sparse MFT".into()))?;
            let (mut disk, mut left) = (lcn as u64 * cluster, run.clusters * cluster);
            while left > 0 && mft_pos < valid {
                let n = left.min(CHUNK).min(valid - mft_pos);
                let read = n.div_ceil(cluster) * cluster;
                chunks.push((disk, mft_pos / rec_size, n, read.min(left)));
                disk += n;
                left -= n;
                mft_pos += n;
            }
        }
        if mft_pos < valid {
            // The rest of the MFT's map is in an attribute list (very fragmented MFT).
            return Err(MftError::Unsupported("MFT too fragmented".into()));
        }

        // Chunks are read and parsed in parallel and merged as they finish,
        // so only the chunks in flight are held in parsed form.
        let total = (valid / rec_size) as usize;
        let merged = Mutex::new((vec![Rec::default(); total], Vec::<Link>::new()));
        chunks.par_iter().try_for_each(|&(disk, first, n, read)| {
            if opts
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                return Err(MftError::Cancelled);
            }
            let mut buf = vec![0u8; read as usize];
            read_at(&vol, &mut buf, disk).map_err(MftError::Io)?;
            let parsed: Vec<Parsed> = buf[..n as usize]
                .chunks_exact_mut(rec_size as usize)
                .enumerate()
                .filter_map(|(i, rec)| parse_record(rec, first + i as u64))
                .collect();
            drop(buf);
            if let Some(p) = &opts.progress {
                let (mut files, mut dirs, mut bytes) = (0, 0, 0);
                for r in parsed.iter().filter(|r| r.base == r.record) {
                    if r.is_dir {
                        dirs += 1;
                    } else {
                        files += 1;
                    }
                    bytes += r.data.map_or(0, |d| d.0);
                }
                p.files.fetch_add(files, Ordering::Relaxed);
                p.dirs.fetch_add(dirs, Ordering::Relaxed);
                p.bytes.fetch_add(bytes, Ordering::Relaxed);
            }
            let mut guard = merged.lock().unwrap();
            let (recs, links) = &mut *guard;
            for p in parsed {
                merge(recs, links, p);
            }
            Ok(())
        })?;
        let (recs, links) = merged.into_inner().unwrap();
        build(recs, links, root, &vol_root, nv.VolumeSerialNumber as u64)
    }

    /// Adds one parsed record to its base record and the link list.
    fn merge(recs: &mut [Rec], links: &mut Vec<Link>, p: Parsed) {
        let base = p.base;
        let total = recs.len() as u64;
        let Some(r) = recs.get_mut(base as usize) else {
            return;
        };
        if base == p.record {
            r.in_use = true;
            r.is_dir = p.is_dir;
        }
        if let Some((attrs, modified)) = p.si {
            r.attrs = attrs;
            r.modified = modified;
        }
        if let Some((size, alloc)) = p.data {
            r.size = size;
            r.alloc += alloc;
        }
        if let Some(w) = p.wof {
            r.alloc += w;
        }
        if let Some(tag) = p.reparse {
            r.reparse = tag;
        }
        // NTFS's own files (below record 16, except the root) are not listed.
        if base < FIRST_USER_RECORD && base != ROOT_RECORD {
            return;
        }
        for name in p.names {
            if name.namespace == NAMESPACE_DOS || name.parent == base || name.parent >= total {
                continue;
            }
            if r.reparse == 0 && name.reparse != 0 {
                r.reparse = name.reparse;
            }
            links.push(Link {
                parent: name.parent as u32,
                child: base as u32,
                name: name.name.into_boxed_str(),
            });
        }
    }

    fn read_at(f: &File, buf: &mut [u8], mut offset: u64) -> io::Result<()> {
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], offset)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
            offset += n as u64;
        }
        Ok(())
    }

    /// Turns the merged records into a flat, breadth-first arena rooted at `root`.
    fn build(
        recs: Vec<Rec>,
        mut links: Vec<Link>,
        root: &Path,
        vol_root: &str,
        serial: u64,
    ) -> Result<ScanTree, MftError> {
        let n = recs.len();
        // Names per record, so hardlinked files can be counted once.
        let mut name_count = vec![0u8; n];
        for l in &links {
            let c = &mut name_count[l.child as usize];
            *c = c.saturating_add(1);
        }
        // Group the links by parent: each folder's entries are one slice.
        links.sort_unstable_by_key(|l| l.parent);
        let mut start = vec![0u32; n + 1];
        for l in &links {
            start[l.parent as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let kids = |r: u32| start[r as usize] as usize..start[r as usize + 1] as usize;

        // Find the scanned folder by name from the volume root.
        let full = root.to_string_lossy();
        let full = full.strip_prefix(r"\\?\").unwrap_or(&full);
        let rel = full.get(vol_root.len().min(full.len())..).unwrap_or("");
        let mut cur = ROOT_RECORD as u32;
        for comp in Path::new(rel).components() {
            let Component::Normal(part) = comp else {
                continue;
            };
            let want = part.to_string_lossy().to_lowercase();
            cur = links[kids(cur)]
                .iter()
                .find(|l| recs[l.child as usize].is_dir && l.name.to_lowercase() == want)
                .map(|l| l.child)
                .ok_or(MftError::NotFound)?;
        }

        let modified = |ft: u64| {
            if ft == 0 {
                0
            } else {
                ft as i64 / 10_000_000 - EPOCH_DIFF_SECS
            }
        };
        let mut nodes: Vec<Node> = Vec::new();
        let mut link_keys: Vec<(NodeId, FileKey)> = Vec::new();
        nodes.push(Node {
            name: root.to_string_lossy().into(),
            parent: 0,
            first_child: 0,
            child_count: 0,
            kind: NodeKind::Dir,
            flags: 0,
            category: 0,
            size: 0,
            alloc: 0,
            modified: modified(recs[cur as usize].modified),
            files: 0,
        });
        let mut seen = vec![false; n];
        seen[cur as usize] = true;
        let mut queue: VecDeque<(u32, NodeId)> = VecDeque::from([(cur, 0)]);
        while let Some((rec, id)) = queue.pop_front() {
            let range = kids(rec);
            if range.is_empty() {
                continue;
            }
            let entries = &mut links[range];
            entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
            let first = u32::try_from(nodes.len())
                .map_err(|_| MftError::Unsupported("too many entries".into()))?;
            nodes[id as usize].first_child = first;
            nodes[id as usize].child_count = entries.len() as u32;
            for l in entries.iter_mut() {
                let child = l.child;
                let r = &recs[child as usize];
                let kind = if r.reparse & TAG_NAME_SURROGATE != 0 {
                    NodeKind::Symlink
                } else if r.is_dir {
                    NodeKind::Dir
                } else {
                    NodeKind::File
                };
                let cid = nodes.len() as NodeId;
                let mut node = Node {
                    // Moved, not copied: each name ends up in exactly one node.
                    name: std::mem::take(&mut l.name),
                    parent: id,
                    first_child: 0,
                    child_count: 0,
                    kind,
                    flags: 0,
                    category: 0,
                    size: 0,
                    alloc: 0,
                    modified: modified(r.modified),
                    files: 0,
                };
                match kind {
                    NodeKind::File => {
                        node.size = r.size;
                        if r.attrs & ATTR_CLOUD != 0 {
                            node.flags |= flags::CLOUD_PLACEHOLDER;
                        } else {
                            node.alloc = r.alloc;
                        }
                        if r.attrs & ATTR_COMPRESSED != 0 {
                            node.flags |= flags::COMPRESSED;
                        }
                        if r.attrs & ATTR_SPARSE != 0 {
                            node.flags |= flags::SPARSE;
                        }
                        if name_count[child as usize] > 1 {
                            link_keys.push((cid, FileKey::new(serial, child.into())));
                        }
                    }
                    NodeKind::Dir if !seen[child as usize] => {
                        seen[child as usize] = true;
                        queue.push_back((child, cid));
                    }
                    _ => {}
                }
                nodes.push(node);
            }
        }
        drop(links);
        drop(recs);

        let mut tree = ScanTree {
            nodes,
            errors: Vec::new(),
            stats: ScanStats::default(),
        };
        tree.compute_totals(link_keys);
        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a 1024-byte FILE record with the given attributes, applying
    /// fixups the way NTFS writes them.
    fn record(flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&0x30u16.to_le_bytes()); // update sequence offset
        r[6..8].copy_from_slice(&3u16.to_le_bytes()); // 2 strides + 1
        r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes()); // first attribute
        r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        r[0x20..0x28].copy_from_slice(&base.to_le_bytes());
        let mut off = 0x38;
        for a in attrs {
            r[off..off + a.len()].copy_from_slice(a);
            off += a.len();
        }
        r[off..off + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        r[0x18..0x1C].copy_from_slice(&((off + 8) as u32).to_le_bytes());
        r[0x1C..0x20].copy_from_slice(&1024u32.to_le_bytes());
        // Fixups: save each stride's last two bytes and stamp the sequence number.
        let usn = [0xAB, 0xCD];
        r[0x30..0x32].copy_from_slice(&usn);
        for i in 0..2 {
            let end = (i + 1) * 512 - 2;
            let saved = [r[end], r[end + 1]];
            r[0x32 + 2 * i..0x34 + 2 * i].copy_from_slice(&saved);
            r[end..end + 2].copy_from_slice(&usn);
        }
        r
    }

    fn resident(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
        let name16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let vofs = (0x18 + name16.len() + 7) & !7;
        let len = (vofs + value.len() + 7) & !7;
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[9] = name.encode_utf16().count() as u8;
        a[0x0A..0x0C].copy_from_slice(&0x18u16.to_le_bytes());
        a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[0x14..0x16].copy_from_slice(&(vofs as u16).to_le_bytes());
        a[0x18..0x18 + name16.len()].copy_from_slice(&name16);
        a[vofs..vofs + value.len()].copy_from_slice(value);
        a
    }

    fn non_resident(
        name: &str,
        flags: u16,
        start_vcn: u64,
        alloc: u64,
        size: u64,
        compressed: u64,
        runs: &[u8],
    ) -> Vec<u8> {
        let name16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let runs_ofs = (0x48 + name16.len() + 7) & !7;
        let len = (runs_ofs + runs.len() + 7) & !7;
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 1;
        a[9] = name.encode_utf16().count() as u8;
        a[0x0A..0x0C].copy_from_slice(&0x48u16.to_le_bytes());
        a[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        a[0x10..0x18].copy_from_slice(&start_vcn.to_le_bytes());
        a[0x20..0x22].copy_from_slice(&(runs_ofs as u16).to_le_bytes());
        a[0x28..0x30].copy_from_slice(&alloc.to_le_bytes());
        a[0x30..0x38].copy_from_slice(&size.to_le_bytes());
        a[0x38..0x40].copy_from_slice(&size.to_le_bytes());
        a[0x40..0x48].copy_from_slice(&compressed.to_le_bytes());
        a[0x48..0x48 + name16.len()].copy_from_slice(&name16);
        a[runs_ofs..runs_ofs + runs.len()].copy_from_slice(runs);
        a
    }

    fn file_name(parent: u64, name: &str, namespace: u8, reparse: u32) -> Vec<u8> {
        let mut v = vec![0u8; 0x42];
        v[0..8].copy_from_slice(&(parent | (7u64 << 48)).to_le_bytes()); // sequence bits ignored
        v[0x3C..0x40].copy_from_slice(&reparse.to_le_bytes());
        v[0x40] = name.encode_utf16().count() as u8;
        v[0x41] = namespace;
        v.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
        resident(ATTR_FILE_NAME, "", &v)
    }

    fn std_info(attrs: u32, modified: u64) -> Vec<u8> {
        let mut v = vec![0u8; 0x48];
        v[0x08..0x10].copy_from_slice(&modified.to_le_bytes());
        v[0x20..0x24].copy_from_slice(&attrs.to_le_bytes());
        resident(ATTR_STANDARD_INFORMATION, "", &v)
    }

    #[test]
    fn parses_a_file_record() {
        let mut r = record(
            0x01,
            0,
            &[
                std_info(0x20, 133_000_000_000_000_000),
                file_name(5, "REPORT~1.PDF", NAMESPACE_DOS, 0),
                file_name(5, "Report final.pdf", 1, 0),
                non_resident("", 0, 0, 8192, 5000, 0, &[0x11, 0x02, 0x40, 0x00]),
            ],
        );
        let p = parse_record(&mut r, 42).unwrap();
        assert_eq!((p.record, p.base, p.is_dir), (42, 42, false));
        assert_eq!(p.si, Some((0x20, 133_000_000_000_000_000)));
        assert_eq!(p.data, Some((5000, 8192)));
        assert_eq!(p.names.len(), 2);
        assert_eq!(p.names[1].name, "Report final.pdf");
        assert_eq!(p.names[1].parent, 5, "sequence bits are masked off");
    }

    #[test]
    fn resident_compressed_wof_and_extension_records() {
        let mut small = record(
            0x01,
            0,
            &[
                file_name(5, "tiny.txt", 3, 0),
                resident(ATTR_DATA, "", b"hello"),
            ],
        );
        assert_eq!(parse_record(&mut small, 30).unwrap().data, Some((5, 0)));

        let mut packed = record(
            0x01,
            0,
            &[non_resident("", 0x0001, 0, 65536, 60000, 12288, &[])],
        );
        assert_eq!(
            parse_record(&mut packed, 31).unwrap().data,
            Some((60000, 12288))
        );

        let mut wof = record(
            0x01,
            0,
            &[
                non_resident("", 0x8000, 0, 0, 90000, 0, &[]),
                non_resident("WofCompressedData", 0, 0, 40960, 39000, 0, &[]),
            ],
        );
        let p = parse_record(&mut wof, 32).unwrap();
        assert_eq!((p.data, p.wof), (Some((90000, 0)), Some(40960)));

        // A later piece of the data (VCN > 0) has no sizes; the base record is 77.
        let mut ext = record(
            0x01,
            77 | (3 << 48),
            &[
                non_resident("", 0, 128, 1, 1, 0, &[]),
                file_name(9, "link2", 1, 0),
            ],
        );
        let p = parse_record(&mut ext, 500).unwrap();
        assert_eq!((p.record, p.base, p.data), (500, 77, None));
        assert_eq!(p.names[0].name, "link2");

        let mut dir = record(0x03, 0, &[file_name(5, "Junction", 1, 0xA000_0003)]);
        let p = parse_record(&mut dir, 33).unwrap();
        assert!(p.is_dir);
        assert_eq!((p.names[0].reparse, p.reparse), (0xA000_0003, None));

        // The tag can also be only in the $REPARSE_POINT attribute.
        let mut tag = 0xA000_000Cu32.to_le_bytes().to_vec(); // symlink
        tag.extend([0u8; 12]);
        let mut link = record(
            0x03,
            0,
            &[
                file_name(5, "Link", 1, 0),
                resident(ATTR_REPARSE_POINT, "", &tag),
            ],
        );
        assert_eq!(
            parse_record(&mut link, 34).unwrap().reparse,
            Some(0xA000_000C)
        );
    }

    #[test]
    fn unused_and_torn_records_are_skipped() {
        let mut unused = record(0x00, 0, &[file_name(5, "gone", 1, 0)]);
        assert!(parse_record(&mut unused, 1).is_none());
        let mut torn = record(0x01, 0, &[file_name(5, "x", 1, 0)]);
        torn[1022] ^= 0xFF; // second stride written by a different update
        assert!(parse_record(&mut torn, 1).is_none());
        let mut junk = vec![0u8; 1024];
        assert!(parse_record(&mut junk, 1).is_none());
    }

    #[test]
    fn data_runs() {
        // 0x21: 1-byte length, 2-byte offset. Second run goes back (negative delta);
        // the third is sparse.
        let runs = [0x21, 0x10, 0x00, 0x10, 0x11, 0x08, 0xF0, 0x01, 0x05, 0x00];
        assert_eq!(
            parse_runs(&runs).unwrap(),
            [
                Run {
                    lcn: Some(0x1000),
                    clusters: 0x10
                },
                Run {
                    lcn: Some(0x1000 - 0x10),
                    clusters: 0x08
                },
                Run {
                    lcn: None,
                    clusters: 0x05
                },
            ]
        );
        assert!(parse_runs(&[0x31, 0x01]).is_none(), "truncated");

        let mut rec0 = record(
            0x01,
            0,
            &[non_resident(
                "",
                0,
                0,
                4096,
                4096,
                0,
                &[0x31, 0x04, 0x00, 0x00, 0x0C, 0x00],
            )],
        );
        assert_eq!(
            mft_runs(&mut rec0).unwrap(),
            [Run {
                lcn: Some(0xC0000),
                clusters: 4
            }]
        );
    }
}
