//! The startup disk's APFS layout. Every volume in the container reports its exact
//! usage, so the chart can account for every byte on the disk from the first frame,
//! and only the Data volume (your files and apps) needs scanning.

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub struct DiskLayout {
    /// The disk's name in Finder, e.g. "Macintosh HD".
    pub name: String,
    /// Bytes in use across the whole APFS container (what Finder reports as used).
    pub container_used: u64,
    /// Where the Data volume is mounted; this is what gets scanned.
    pub data_root: PathBuf,
    pub data_used: u64,
    /// The other volumes, by exact usage: the sealed macOS volume, Preboot, swap…
    pub extras: Vec<(String, u64)>,
    /// The Data volume's APFS snapshots, oldest first.
    pub snapshots: Vec<Snapshot>,
    /// The Data volume's device (e.g. "disk3s5"), for deleting snapshots.
    pub data_device: String,
}

/// Label for the part of the Data volume not yet scanned (and, once the scan is
/// done, not readable).
pub const NOT_SCANNED: &str = "Not scanned yet";
pub const NOT_READABLE: &str = "Not readable";
/// The same remainder when the Data volume has snapshots: APFS doesn't say how much of it
/// they hold, only that it's in there.
pub const SNAPSHOTS_AND_UNREADABLE: &str = "Snapshots and unreadable";

/// Whether a slice named `name` is the Data volume's remainder rather than a volume.
pub fn is_remainder(name: &str) -> bool {
    [NOT_SCANNED, NOT_READABLE, SNAPSHOTS_AND_UNREADABLE].contains(&name)
}

/// The label for what the Data volume holds beyond what was read.
pub fn remainder_label(snapshots: &[Snapshot]) -> &'static str {
    if snapshots.is_empty() { NOT_READABLE } else { SNAPSHOTS_AND_UNREADABLE }
}

/// An APFS snapshot: a frozen copy of a volume. Its blocks stay allocated until it goes,
/// so files deleted (or changed) since it was taken don't free their space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub name: String,
    /// Seconds since 1970.
    pub created: i64,
}

impl Snapshot {
    /// A local Time Machine snapshot, which macOS keeps for 24 hours and which is safe to
    /// delete (the backups themselves are elsewhere).
    pub fn is_time_machine(&self) -> bool {
        self.name.starts_with("com.apple.TimeMachine.")
    }

    /// What made it, in words.
    pub fn kind(&self) -> &'static str {
        if self.is_time_machine() {
            "Time Machine"
        } else if self.name.starts_with("com.apple.os.update") {
            "macOS update"
        } else if self.name.starts_with("com.bombich.ccc") {
            "Carbon Copy Cloner"
        } else {
            "Other app"
        }
    }
}

/// The snapshots of the volume mounted at `mount`, oldest first. Listing them needs no
/// special rights.
pub fn snapshots(mount: &Path) -> Vec<Snapshot> {
    unsafe extern "C" {
        fn fs_snapshot_list(dirfd: libc::c_int, alist: *mut libc::attrlist, buf: *mut libc::c_void, size: libc::size_t, flags: u32) -> libc::c_int;
    }
    let Ok(c_path) = CString::new(mount.as_os_str().as_bytes()) else { return Vec::new() };
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if fd < 0 {
        return Vec::new();
    }
    let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
    attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    // The call refuses (EINVAL) without the returned-attributes bitmap.
    attrs.commonattr = libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_NAME | libc::ATTR_CMN_CRTIME;
    let mut buf = vec![0u8; 64 * 1024];
    let mut found = Vec::new();
    loop {
        let count = unsafe { fs_snapshot_list(fd, &mut attrs, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if count <= 0 {
            break;
        }
        let mut at = 0;
        for _ in 0..count {
            let Some(length) = read_u32(&buf, at) else { break };
            if let Some(snapshot) = parse_snapshot(&buf[at..(at + length as usize).min(buf.len())]) {
                found.push(snapshot);
            }
            at += length as usize;
        }
    }
    unsafe { libc::close(fd) };
    found.sort_by_key(|s: &Snapshot| s.created);
    found
}

fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(buf.get(at..at + 4)?.try_into().ok()?))
}

/// One `fs_snapshot_list` record: length, returned attributes, name reference, creation time.
fn parse_snapshot(record: &[u8]) -> Option<Snapshot> {
    let name_ref = 4 + std::mem::size_of::<libc::attribute_set_t>();
    let offset = i32::from_ne_bytes(record.get(name_ref..name_ref + 4)?.try_into().ok()?);
    let length = read_u32(record, name_ref + 4)? as usize;
    let start = usize::try_from(name_ref as i64 + offset as i64).ok()?;
    let name = record.get(start..start + length.saturating_sub(1))?;
    let time = name_ref + std::mem::size_of::<libc::attrreference_t>();
    let seconds = i64::from_ne_bytes(record.get(time..time + 8)?.try_into().ok()?);
    Some(Snapshot { name: String::from_utf8_lossy(name).into_owned(), created: seconds })
}

/// Space on the volume holding `path` that macOS frees by itself when it needs room: local
/// Time Machine snapshots, iCloud files it can download again, caches marked purgeable.
/// It's part of the used space (it is in the chart already), the difference between what's
/// free now and what's available for "important" use, as Finder shows it.
pub fn purgeable(path: &Path) -> Option<u64> {
    use std::ffi::c_void;
    type CFTypeRef = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFURLVolumeAvailableCapacityKey: CFTypeRef;
        static kCFURLVolumeAvailableCapacityForImportantUsageKey: CFTypeRef;
        fn CFURLCreateFromFileSystemRepresentation(allocator: CFTypeRef, buffer: *const u8, length: isize, is_directory: u8) -> CFTypeRef;
        fn CFURLCopyResourcePropertyForKey(url: CFTypeRef, key: CFTypeRef, value: *mut CFTypeRef, error: *mut CFTypeRef) -> u8;
        fn CFNumberGetValue(number: CFTypeRef, number_type: isize, value: *mut c_void) -> u8;
        fn CFRelease(cf: CFTypeRef);
    }
    const K_CF_NUMBER_SINT64_TYPE: isize = 4;
    let bytes = path.as_os_str().as_bytes();
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 1);
        if url.is_null() {
            return None;
        }
        let read = |key: CFTypeRef| -> Option<i64> {
            let mut value: CFTypeRef = std::ptr::null();
            if CFURLCopyResourcePropertyForKey(url, key, &mut value, std::ptr::null_mut()) == 0 || value.is_null() {
                return None;
            }
            let mut out: i64 = 0;
            let ok = CFNumberGetValue(value, K_CF_NUMBER_SINT64_TYPE, (&mut out as *mut i64).cast());
            CFRelease(value);
            (ok != 0).then_some(out)
        };
        let free = read(kCFURLVolumeAvailableCapacityKey);
        let important = read(kCFURLVolumeAvailableCapacityForImportantUsageKey);
        CFRelease(url);
        Some(important?.saturating_sub(free?).max(0) as u64)
    }
}

pub(crate) fn volume_used(mount: &Path) -> Option<u64> {
    let c_path = CString::new(mount.as_os_str().as_bytes()).ok()?;
    let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
    attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attrs.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED;
    #[repr(C, packed)]
    struct Reply {
        length: u32,
        used: libc::off_t,
    }
    let mut reply = Reply { length: 0, used: 0 };
    let result = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            &mut attrs as *mut _ as *mut libc::c_void,
            &mut reply as *mut _ as *mut libc::c_void,
            std::mem::size_of::<Reply>(),
            0,
        )
    };
    (result == 0).then_some(reply.used as u64)
}

/// "/dev/disk3s1s1" → "disk3": the APFS container a volume belongs to.
pub fn container_of(device: &str) -> Option<&str> {
    let name = device.strip_prefix("/dev/")?;
    let digits = name.strip_prefix("disk")?;
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    Some(&name[..4 + end])
}

pub fn cstr(chars: &[libc::c_char]) -> String {
    unsafe { CStr::from_ptr(chars.as_ptr()) }.to_string_lossy().into_owned()
}

/// The startup disk's layout, or `None` when it isn't a modern APFS system/data pair.
pub fn startup_layout(name: String) -> Option<DiskLayout> {
    let mut root: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c"/".as_ptr(), &mut root) } != 0 {
        return None;
    }
    let root_device = cstr(&root.f_mntfromname);
    let container = container_of(&root_device)?.to_string();
    let container_used = (root.f_blocks - root.f_bfree) * root.f_bsize as u64;

    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 {
        return None;
    }
    let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };

    let mut data = None;
    let mut data_device = String::new();
    let mut extras = Vec::new();
    for mount in mounts {
        if cstr(&mount.f_fstypename) != "apfs" || container_of(&cstr(&mount.f_mntfromname)) != Some(container.as_str()) {
            continue;
        }
        let mount_point = PathBuf::from(cstr(&mount.f_mntonname));
        let Some(used) = volume_used(&mount_point) else { continue };
        let label = match mount_point.to_str() {
            Some("/") => "macOS".to_string(),
            Some("/System/Volumes/Data") => {
                data = Some((mount_point, used));
                data_device = cstr(&mount.f_mntfromname).trim_start_matches("/dev/").to_string();
                continue;
            }
            Some("/System/Volumes/VM") => "Swap (VM)".to_string(),
            Some("/System/Volumes/Preboot") => "Preboot".to_string(),
            Some("/System/Volumes/Update") => "Software updates".to_string(),
            _ => mount_point.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        extras.push((label, used));
    }
    let (data_root, data_used) = data?;
    let accounted: u64 = data_used + extras.iter().map(|e| e.1).sum::<u64>();
    if container_used > accounted {
        // Unmounted volumes (usually Recovery) and container metadata.
        extras.push(("Other volumes".to_string(), container_used - accounted));
    }
    let snapshots = snapshots(&data_root);
    Some(DiskLayout { name, container_used, data_root, data_used, extras, snapshots, data_device })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_container() {
        assert_eq!(container_of("/dev/disk3s1s1"), Some("disk3"));
        assert_eq!(container_of("/dev/disk12s5"), Some("disk12"));
        assert_eq!(container_of("map auto_home"), None);
    }

    /// A record as `fs_snapshot_list` packs it: length, returned attributes, a reference
    /// to the name (relative to the reference itself), the creation time, then the name.
    #[test]
    fn parses_snapshot_record() {
        let name = b"com.apple.TimeMachine.2026-10-04-101010.local\0";
        let set = std::mem::size_of::<libc::attribute_set_t>();
        let reference = std::mem::size_of::<libc::attrreference_t>();
        let name_at = 4 + set + reference + 16;
        let mut record = vec![0u8; name_at + name.len()];
        let length = record.len() as u32;
        record[..4].copy_from_slice(&length.to_ne_bytes());
        record[4 + set..8 + set].copy_from_slice(&((name_at - 4 - set) as i32).to_ne_bytes());
        record[8 + set..12 + set].copy_from_slice(&(name.len() as u32).to_ne_bytes());
        record[4 + set + reference..12 + set + reference].copy_from_slice(&1_791_113_131i64.to_ne_bytes());
        record[name_at..].copy_from_slice(name);
        let snapshot = parse_snapshot(&record).unwrap();
        assert_eq!(snapshot.name, "com.apple.TimeMachine.2026-10-04-101010.local");
        assert_eq!(snapshot.created, 1_791_113_131);
        assert!(snapshot.is_time_machine());
        assert_eq!(snapshot.kind(), "Time Machine");
        // Truncated records are skipped, not misread.
        assert!(parse_snapshot(&record[..name_at + 4]).is_none());
    }

    /// Whatever snapshots the startup disk has come back named and dated; purgeable space
    /// is known for it.
    #[test]
    fn lists_real_snapshots() {
        for snapshot in snapshots(Path::new("/")) {
            assert!(snapshot.name.starts_with("com."), "{snapshot:?}");
            assert!(snapshot.created > 1_500_000_000, "{snapshot:?}");
        }
        assert!(purgeable(Path::new("/")).is_some());
    }

    #[test]
    fn remainder_names() {
        assert_eq!(remainder_label(&[]), NOT_READABLE);
        assert_eq!(remainder_label(&[Snapshot { name: "x".into(), created: 0 }]), SNAPSHOTS_AND_UNREADABLE);
        assert!(is_remainder(NOT_SCANNED) && is_remainder(NOT_READABLE) && is_remainder(SNAPSHOTS_AND_UNREADABLE));
        assert!(!is_remainder("macOS"));
    }

    /// The layout must account for exactly what the container reports as used.
    #[test]
    fn layout_adds_up() {
        let Some(layout) = startup_layout("Macintosh HD".into()) else { return };
        let total = layout.data_used + layout.extras.iter().map(|e| e.1).sum::<u64>();
        assert_eq!(total, layout.container_used);
        assert!(layout.extras.iter().any(|e| e.0 == "macOS"));
    }
}

/// The APFS container of the volume mounted at `path` (e.g. "disk3"), if any.
pub fn container_at(path: &Path) -> Option<String> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    container_of(&cstr(&stat.f_mntfromname)).map(str::to_string)
}
