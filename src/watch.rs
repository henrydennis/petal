//! Following changes on disk after a scan, with FSEvents.
//!
//! The scan notes the FSEvents position before it starts walking; once the results are
//! on screen, a stream from that position reports every folder that changed since,
//! including during the scan. Only those folders are read again (`scan::read_changes`),
//! so the chart stays current without a rescan.
//!
//! Events are per folder ("something in here changed"), coalesced by macOS. When macOS
//! can't say which folders changed (events dropped, history lost), it asks for the folder
//! to be rescanned as a whole, or the whole watch is out of date.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A folder that changed. `recursive`: rescan everything below it, not just its listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub recursive: bool,
}

#[derive(Default)]
pub struct Pending {
    pub changes: Vec<Change>,
    /// FSEvents lost track (the watched folder itself moved, or its history is gone):
    /// only a full rescan can be trusted.
    pub lost: bool,
}

/// The FSEvents position now. Note it before scanning to catch changes made during the scan.
pub fn current_event_id() -> u64 {
    unsafe { FSEventsGetCurrentEventId() }
}

/// A running FSEvents stream; stops when dropped.
pub struct Watch {
    stream: FSEventStreamRef,
    queue: DispatchQueue,
    pending: *const Mutex<Pending>,
}

impl Watch {
    /// Watch `root` and everything below it, replaying changes since `since`.
    pub fn start(root: &Path, since: u64) -> Option<Watch> {
        let c_root = CString::new(root.as_os_str().as_encoded_bytes()).ok()?;
        let pending = Arc::into_raw(Arc::new(Mutex::new(Pending::default())));
        unsafe {
            let path = CFStringCreateWithCString(std::ptr::null(), c_root.as_ptr(), K_CF_STRING_ENCODING_UTF8);
            let values = [path];
            let paths = CFArrayCreate(std::ptr::null(), values.as_ptr(), 1, &kCFTypeArrayCallBacks);
            let mut context = FSEventStreamContext {
                version: 0,
                info: pending as *mut c_void,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            let stream = FSEventStreamCreate(std::ptr::null(), callback, &mut context, paths, since, LATENCY_SECONDS, FLAG_NONE);
            CFRelease(paths);
            CFRelease(path);
            if stream.is_null() {
                drop(Arc::from_raw(pending));
                return None;
            }
            let queue = dispatch_queue_create(c"petal.watch".as_ptr(), std::ptr::null());
            FSEventStreamSetDispatchQueue(stream, queue);
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                dispatch_release(queue);
                drop(Arc::from_raw(pending));
                return None;
            }
            Some(Watch { stream, queue, pending })
        }
    }

    /// Changes reported since the last call.
    pub fn take(&self) -> Pending {
        let pending = unsafe { &*self.pending };
        std::mem::take(&mut *pending.lock().unwrap())
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            FSEventStreamStop(self.stream);
            // After this returns, the callback won't run again.
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
            drop(Arc::from_raw(self.pending));
        }
    }
}

/// Batching delay: macOS coalesces the events of a busy second into one delivery.
const LATENCY_SECONDS: f64 = 0.5;

extern "C" fn callback(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    let pending = unsafe { &*(info as *const Mutex<Pending>) };
    let paths = unsafe { std::slice::from_raw_parts(paths as *const *const c_char, count) };
    let flags = unsafe { std::slice::from_raw_parts(flags, count) };
    let Ok(mut pending) = pending.lock() else { return };
    for (&path, &flag) in paths.iter().zip(flags) {
        if flag & (EVENT_HISTORY_DONE | EVENT_IDS_WRAPPED) != 0 && flag & !(EVENT_HISTORY_DONE | EVENT_IDS_WRAPPED) == 0 {
            continue;
        }
        if flag & (EVENT_ROOT_CHANGED | EVENT_USER_DROPPED | EVENT_KERNEL_DROPPED) != 0 {
            pending.lost = true;
            continue;
        }
        let text = unsafe { CStr::from_ptr(path) }.to_string_lossy();
        let trimmed = text.trim_end_matches('/');
        pending.changes.push(Change {
            path: PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed }),
            recursive: flag & EVENT_MUST_SCAN_SUBDIRS != 0,
        });
    }
}

// --- FFI (CoreServices / CoreFoundation / libdispatch) -----------------------------

type FSEventStreamRef = *mut c_void;
type DispatchQueue = *mut c_void;
type CFTypeRef = *const c_void;

#[repr(C)]
struct FSEventStreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const FLAG_NONE: u32 = 0;
const EVENT_MUST_SCAN_SUBDIRS: u32 = 0x0001;
const EVENT_USER_DROPPED: u32 = 0x0002;
const EVENT_KERNEL_DROPPED: u32 = 0x0004;
const EVENT_IDS_WRAPPED: u32 = 0x0008;
const EVENT_HISTORY_DONE: u32 = 0x0010;
const EVENT_ROOT_CHANGED: u32 = 0x0020;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventsGetCurrentEventId() -> u64;
    fn FSEventStreamCreate(
        allocator: *const c_void,
        callback: extern "C" fn(FSEventStreamRef, *mut c_void, usize, *mut c_void, *const u32, *const u64),
        context: *mut FSEventStreamContext,
        paths: CFTypeRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithCString(allocator: *const c_void, text: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFArrayCreate(allocator: *const c_void, values: *const CFTypeRef, count: isize, callbacks: *const c_void) -> CFTypeRef;
    fn CFRelease(object: CFTypeRef);
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: DispatchQueue);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Real FSEvents: a change in a watched folder is reported for that folder.
    #[test]
    fn reports_changed_folder() {
        let dir = std::env::temp_dir().join(format!("petal-watch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let dir = dir.canonicalize().unwrap();
        let watch = Watch::start(&dir, current_event_id()).expect("stream starts");
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(dir.join("inner/new"), b"hello").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while Instant::now() < deadline && !seen.iter().any(|c: &Change| c.path == dir.join("inner")) {
            std::thread::sleep(Duration::from_millis(100));
            seen.extend(watch.take().changes);
        }
        std::fs::remove_dir_all(&dir).ok();
        assert!(seen.iter().any(|c| c.path == dir.join("inner")), "saw {seen:?}");
    }
}
