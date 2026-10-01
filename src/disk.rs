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
}

/// Label for the part of the Data volume not yet scanned (and, once the scan is
/// done, not readable).
pub const NOT_SCANNED: &str = "Not scanned yet";
pub const NOT_READABLE: &str = "Not readable";

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
    Some(DiskLayout { name, container_used, data_root, data_used, extras })
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
