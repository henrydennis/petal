//! Listing a directory together with the metadata the scanner needs.
//!
//! On macOS this uses `getattrlistbulk`, which returns names and attributes for
//! many entries per syscall instead of one `lstat` (and full path lookup) each.

use std::io;
use std::path::Path;

pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    /// Allocated bytes on disk.
    pub size: u64,
    /// Cloud-only (e.g. evicted iCloud Drive) item whose contents aren't on this Mac.
    /// Listing such a directory would make macOS download it, possibly blocking for minutes.
    pub dataless: bool,
    /// Set for files that share disk blocks with other files through APFS cloning.
    pub sharing: Option<Sharing>,
}

/// `Sharing::private` for partial clones until it is looked up (see `private_size`).
pub const PRIVATE_UNKNOWN: u64 = u64::MAX;

/// Bytes of a file shared with no other file (what deleting just it frees).
#[cfg(target_os = "macos")]
pub fn private_size(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
    attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attrs.forkattr = libc::ATTR_CMNEXT_PRIVATESIZE;
    #[repr(C, packed)]
    struct Reply {
        length: u32,
        private: libc::off_t,
    }
    let mut reply = Reply { length: 0, private: 0 };
    let result = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            &mut attrs as *mut _ as *mut libc::c_void,
            &mut reply as *mut _ as *mut libc::c_void,
            std::mem::size_of::<Reply>(),
            libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
        )
    };
    (result == 0).then_some(reply.private as u64)
}

#[cfg(not(target_os = "macos"))]
pub fn private_size(_path: &Path) -> Option<u64> {
    None
}

/// Clone sharing for a single file (for items that are files rather than folders).
#[cfg(target_os = "macos")]
pub fn file_sharing(path: &Path) -> Option<Sharing> {
    let dir = Dir::open(path.parent()?).ok()?;
    let name = path.file_name()?.to_str()?;
    let listing = dir.list(path.parent()?, true).ok()?;
    listing.entries.into_iter().find(|e| e.name == name)?.sharing
}

#[cfg(not(target_os = "macos"))]
pub fn file_sharing(_path: &Path) -> Option<Sharing> {
    None
}

/// How a cloned file shares its blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sharing {
    /// Pure clones (identical, unmodified copies) share one data stream and this id.
    pub clone_id: u64,
    /// How many files share that data stream.
    pub refs: u32,
    /// Bytes shared with no other file: what deleting just this file frees.
    pub private: u64,
    /// Every block is shared (a pure clone).
    pub all: bool,
}

pub struct Listing {
    pub entries: Vec<Entry>,
    /// Entries whose metadata couldn't be read.
    pub errors: u64,
}

#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

#[cfg(target_os = "macos")]
fn is_dataless(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Make this process's filesystem calls fail on cloud-only items instead of asking the
/// file provider (iCloud Drive, Dropbox, …) to download them, which can block a
/// scanning thread in the kernel indefinitely. Disk-usage tools like `du` do the same.
#[cfg(target_os = "macos")]
pub fn disable_cloud_downloads() {
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;
    const IOPOL_SCOPE_PROCESS: libc::c_int = 0;
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;
    unsafe extern "C" {
        fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
    }
    unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
            IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn disable_cloud_downloads() {}

/// Portable implementation: `read_dir` plus an `lstat` per entry.
#[allow(dead_code)]
pub fn list_std(path: &Path) -> io::Result<Listing> {
    use std::os::unix::fs::MetadataExt;
    let mut listing = Listing { entries: Vec::new(), errors: 0 };
    for entry in std::fs::read_dir(path)?.filter_map(Result::ok) {
        match std::fs::symlink_metadata(entry.path()) {
            Ok(meta) => listing.entries.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: meta.is_dir(),
                dev: meta.dev(),
                ino: meta.ino(),
                nlink: meta.nlink() as u64,
                size: meta.blocks() * 512,
                dataless: is_dataless(&meta),
                sharing: None,
            }),
            Err(_) => listing.errors += 1,
        }
    }
    Ok(listing)
}

/// An open directory. Children are opened relative to it with `openat`, so the
/// kernel resolves one path component instead of the whole path from the root.
pub struct Dir(libc::c_int);

impl Drop for Dir {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

const OPEN_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;

impl Dir {
    pub fn open(path: &Path) -> io::Result<Dir> {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        let fd = unsafe { libc::open(c_path.as_ptr(), OPEN_FLAGS & !libc::O_NOFOLLOW) };
        if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(Dir(fd)) }
    }

    pub fn open_at(&self, name: &str) -> io::Result<Dir> {
        let c_name = std::ffi::CString::new(name)?;
        let fd = unsafe { libc::openat(self.0, c_name.as_ptr(), OPEN_FLAGS) };
        if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(Dir(fd)) }
    }

    /// `path` is this directory's path, used for mount points and the portable fallback.
    /// With `sharing`, also report which files share blocks through APFS cloning. That
    /// costs ~12% more on a big volume, so the main scan leaves it off.
    #[cfg(target_os = "macos")]
    pub fn list(&self, path: &Path, sharing: bool) -> io::Result<Listing> {
        macos::list(self.0, path, sharing)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn list(&self, path: &Path, _sharing: bool) -> io::Result<Listing> {
        list_std(path)
    }
}

/// A directory's own allocated size, as its parent's listing would report it.
#[cfg(target_os = "macos")]
pub fn dir_alloc(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
    attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attrs.dirattr = libc::ATTR_DIR_ALLOCSIZE;
    #[repr(C, packed)]
    struct Reply {
        length: u32,
        alloc: libc::off_t,
    }
    let mut reply = Reply { length: 0, alloc: 0 };
    let result = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            &mut attrs as *mut _ as *mut libc::c_void,
            &mut reply as *mut _ as *mut libc::c_void,
            std::mem::size_of::<Reply>(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    (result == 0).then_some(reply.alloc as u64)
}

#[cfg(not(target_os = "macos"))]
pub fn dir_alloc(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| m.blocks() * 512)
}

/// Let the scanner keep an fd open per directory level on every worker thread.
pub fn raise_fd_limit() {
    unsafe {
        let mut limit: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            let wanted = limit.rlim_max.min(10_240);
            if limit.rlim_cur < wanted {
                limit.rlim_cur = wanted;
                libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::{Entry, Listing, PRIVATE_UNKNOWN, Sharing};

    // Not (yet) in the libc crate.
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMNEXT_CLONE_REFCNT: u32 = 0x0000_1000;
    const VDIR: u32 = 2;
    const BUFFER_SIZE: usize = 32 * 1024;

    thread_local! {
        static BUFFER: RefCell<Vec<u8>> = RefCell::new(vec![0; BUFFER_SIZE]);
    }

    fn read<T: Copy>(buf: &[u8], at: &mut usize) -> T {
        let value = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(*at) as *const T) };
        *at += std::mem::size_of::<T>();
        value
    }

    pub fn list(fd: libc::c_int, path: &Path, sharing: bool) -> io::Result<Listing> {
        let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
        attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        attrs.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_DEVID
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FLAGS
            | libc::ATTR_CMN_FILEID;
        attrs.dirattr = libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_ALLOCSIZE;
        attrs.fileattr = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE;
        // The per-file flags say whether a file shares blocks; clone details are then
        // fetched only for pure clones (asking for them in bulk doubles the scan time).
        if sharing {
            attrs.forkattr = libc::ATTR_CMNEXT_EXT_FLAGS;
        }
        let options = if sharing { libc::FSOPT_ATTR_CMN_EXTENDED as u64 } else { 0 };

        let mut listing = Listing { entries: Vec::new(), errors: 0 };
        BUFFER.with_borrow_mut(|buf| {
            loop {
                let count = unsafe {
                    libc::getattrlistbulk(
                        fd,
                        &mut attrs as *mut _ as *mut libc::c_void,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                        options,
                    )
                };
                if count < 0 {
                    return Err(io::Error::last_os_error());
                }
                if count == 0 {
                    return Ok(());
                }
                let mut offset = 0usize;
                for _ in 0..count {
                    let start = offset;
                    let mut at = offset;
                    let length: u32 = read(buf, &mut at);
                    offset = start + length as usize;
                    if let Some(mut entry) = parse_entry(buf, at, path, &mut listing.errors) {
                        if entry.sharing.is_some_and(|s| s.all) {
                            entry.sharing = sharing_of(fd, &entry.name);
                        }
                        listing.entries.push(entry);
                    }
                }
            }
        })?;
        Ok(listing)
    }

    /// Fields come back packed in bit order, except that ATTR_CMN_RETURNED_ATTRS and
    /// ATTR_CMN_ERROR lead; only attributes flagged in the returned set are present.
    fn parse_entry(buf: &[u8], mut at: usize, parent: &Path, errors: &mut u64) -> Option<Entry> {
        let returned: libc::attribute_set_t = read(buf, &mut at);
        if returned.commonattr & ATTR_CMN_ERROR != 0 {
            let error: u32 = read(buf, &mut at);
            if error != 0 {
                *errors += 1;
                return None;
            }
        }
        let mut name = String::new();
        if returned.commonattr & libc::ATTR_CMN_NAME != 0 {
            let reference_at = at;
            let reference: libc::attrreference_t = read(buf, &mut at);
            let name_start = (reference_at as isize + reference.attr_dataoffset as isize) as usize;
            // The length includes the trailing NUL.
            let bytes = &buf[name_start..name_start + reference.attr_length.saturating_sub(1) as usize];
            name = String::from_utf8_lossy(bytes).into_owned();
        }
        let mut dev = 0u64;
        if returned.commonattr & libc::ATTR_CMN_DEVID != 0 {
            dev = read::<libc::dev_t>(buf, &mut at) as u64;
        }
        let mut obj_type = 0u32;
        if returned.commonattr & libc::ATTR_CMN_OBJTYPE != 0 {
            obj_type = read(buf, &mut at);
        }
        let mut flags = 0u32;
        if returned.commonattr & libc::ATTR_CMN_FLAGS != 0 {
            flags = read(buf, &mut at);
        }
        let mut ino = 0u64;
        if returned.commonattr & libc::ATTR_CMN_FILEID != 0 {
            ino = read(buf, &mut at);
        }

        let is_dir = obj_type == VDIR;
        let mut nlink = 1u64;
        let mut size = 0u64;
        if is_dir {
            let mut mount_point = false;
            if returned.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
                let status: u32 = read(buf, &mut at);
                mount_point = status & libc::DIR_MNTSTATUS_MNTPOINT != 0;
            }
            if returned.dirattr & libc::ATTR_DIR_ALLOCSIZE != 0 {
                size = read::<libc::off_t>(buf, &mut at) as u64;
            }
            // The entry reports the covered directory; ask for the mounted volume's device.
            if mount_point {
                match std::fs::symlink_metadata(parent.join(&name)) {
                    Ok(meta) => dev = meta.dev(),
                    Err(_) => {
                        *errors += 1;
                        return None;
                    }
                }
            }
        } else {
            if returned.fileattr & libc::ATTR_FILE_LINKCOUNT != 0 {
                nlink = read::<u32>(buf, &mut at) as u64;
            }
            if returned.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 {
                size = read::<libc::off_t>(buf, &mut at) as u64;
            }
        }
        let dataless = flags & super::SF_DATALESS != 0;
        let mut ext_flags = 0u64;
        if returned.forkattr & libc::ATTR_CMNEXT_EXT_FLAGS != 0 {
            ext_flags = read::<u64>(buf, &mut at);
        }
        // Pure clones (every block shared) need their clone id and count, fetched by the
        // caller (see `sharing_of`). Partial clones are only marked: their private size is
        // costly, so it is looked up later, only when "frees if deleted" needs it.
        let sharing = (!is_dir && ext_flags & EF_MAY_SHARE_BLOCKS != 0).then_some(Sharing {
            clone_id: 0,
            refs: 1,
            private: PRIVATE_UNKNOWN,
            all: ext_flags & EF_SHARES_ALL_BLOCKS != 0,
        });
        Some(Entry { name, is_dir, dev, ino, nlink, size, dataless, sharing })
    }

    const EF_MAY_SHARE_BLOCKS: u64 = 0x1;
    const EF_SHARES_ALL_BLOCKS: u64 = 0x40;

    /// Clone id and count for one pure clone in the directory `fd`.
    fn sharing_of(fd: libc::c_int, name: &str) -> Option<Sharing> {
        let c_name = std::ffi::CString::new(name).ok()?;
        let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
        attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        attrs.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
        attrs.forkattr = libc::ATTR_CMNEXT_CLONEID | ATTR_CMNEXT_CLONE_REFCNT;
        #[repr(C, packed)]
        struct Reply {
            length: u32,
            returned: libc::attribute_set_t,
            clone_id: u64,
            refs: u32,
        }
        let mut reply: Reply = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::getattrlistat(
                fd,
                c_name.as_ptr(),
                &mut attrs as *mut _ as *mut libc::c_void,
                &mut reply as *mut _ as *mut libc::c_void,
                std::mem::size_of::<Reply>(),
                (libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW) as libc::c_ulong,
            )
        };
        let wanted = libc::ATTR_CMNEXT_CLONEID | ATTR_CMNEXT_CLONE_REFCNT;
        if result != 0 || reply.returned.forkattr & wanted != wanted {
            return None;
        }
        Some(Sharing { clone_id: reply.clone_id, refs: reply.refs, private: 0, all: true })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The bulk reader must agree with plain lstat on every field the scanner uses.
    #[test]
    fn bulk_matches_lstat() {
        let dirs = [
            std::env::var("HOME").unwrap(),
            "/usr/bin".into(),
            "/private/etc".into(),
            "/".into(),
        ];
        for dir in dirs {
            let dir = Path::new(&dir);
            let mut bulk = Dir::open(dir).unwrap().list(dir, true).unwrap().entries;
            let mut std = list_std(dir).unwrap().entries;
            bulk.sort_by(|a, b| a.name.cmp(&b.name));
            std.sort_by(|a, b| a.name.cmp(&b.name));
            let names = |v: &[Entry]| v.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
            assert_eq!(names(&bulk), names(&std), "names differ in {dir:?}");
            // A folder's own allocation read directly must equal what its parent's listing says.
            for b in bulk.iter().filter(|e| e.is_dir && !e.dataless) {
                if let Some(alloc) = dir_alloc(&dir.join(&b.name)) {
                    // Mount points report the mounted volume's root instead; the scanner never
                    // crosses them, so compare only folders on this directory's own volume.
                    let parent_dev = std::fs::symlink_metadata(dir).map(|m| { use std::os::unix::fs::MetadataExt; m.dev() }).ok();
                    if parent_dev == Some(b.dev) {
                        assert_eq!(alloc, b.size, "{dir:?}/{} own allocation", b.name);
                    }
                }
            }
            for (b, s) in bulk.iter().zip(&std) {
                assert_eq!(b.is_dir, s.is_dir, "{dir:?}/{} is_dir", b.name);
                assert_eq!(b.dataless, s.dataless, "{dir:?}/{} dataless", b.name);
                // Mount points report the covered directory's inode, so only compare files.
                if !b.is_dir {
                    assert_eq!(b.ino, s.ino, "{dir:?}/{} ino", b.name);
                    assert_eq!(b.dev, s.dev, "{dir:?}/{} dev", b.name);
                    assert_eq!(b.size, s.size, "{dir:?}/{} size", b.name);
                    assert_eq!(b.nlink, s.nlink, "{dir:?}/{} nlink", b.name);
                }
            }
        }
    }
}
