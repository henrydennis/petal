//! Filesystem scanning and the in-memory size tree.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gpui::SharedString;

use crate::disk::{self, DiskLayout};
use crate::dirlist;
use crate::findings;
use crate::live::{LIVE_DEPTH, LiveNode};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    /// Not a real folder: an exact amount of space with no browsable contents, such as
    /// the macOS system volume or the part of the disk that couldn't be read.
    Other,
}

#[derive(Clone)]
pub struct Node {
    pub name: SharedString,
    pub size: u64,
    pub kind: Kind,
    pub parent: Option<usize>,
    /// Sorted by size, largest first.
    pub children: Vec<usize>,
    /// Number of files contained (1 for a file).
    pub items: u64,
}

pub struct Tree {
    pub root_path: PathBuf,
    pub nodes: Vec<Node>,
    pub errors: u64,
    /// Cloud-only folders that were skipped rather than downloaded.
    pub cloud_only: u64,
}

impl Tree {
    pub const ROOT: usize = 0;

    pub fn path_of(&self, mut ix: usize) -> PathBuf {
        let mut parts = Vec::new();
        while let Some(parent) = self.nodes[ix].parent {
            parts.push(self.nodes[ix].name.clone());
            ix = parent;
        }
        let mut path = self.root_path.clone();
        for part in parts.iter().rev() {
            path.push(part.as_ref());
        }
        path
    }

    /// The node at `path`, if the scan reached it.
    pub fn find(&self, path: &Path) -> Option<usize> {
        let relative = path.strip_prefix(&self.root_path).ok()?;
        let mut at = Self::ROOT;
        for component in relative.components() {
            let name = component.as_os_str().to_string_lossy();
            at = *self.nodes[at].children.iter().find(|&&c| self.nodes[c].name.as_ref() == name)?;
        }
        Some(at)
    }

    /// Chain of nodes from the root down to `ix`, inclusive.
    pub fn ancestry(&self, mut ix: usize) -> Vec<usize> {
        let mut chain = vec![ix];
        while let Some(parent) = self.nodes[ix].parent {
            chain.push(parent);
            ix = parent;
        }
        chain.reverse();
        chain
    }

    pub fn is_ancestor_or_self(&self, ancestor: usize, mut ix: usize) -> bool {
        loop {
            if ix == ancestor {
                return true;
            }
            match self.nodes[ix].parent {
                Some(parent) => ix = parent,
                None => return false,
            }
        }
    }

    /// Detach a node (after it was trashed) and subtract its size from its ancestors.
    pub fn remove(&mut self, ix: usize) {
        let Some(parent) = self.nodes[ix].parent else {
            return;
        };
        let (size, items) = (self.nodes[ix].size, self.nodes[ix].items);
        self.nodes[parent].children.retain(|&c| c != ix);
        self.nodes[ix].parent = None;

        let mut cur = Some(parent);
        while let Some(p) = cur {
            let node = &mut self.nodes[p];
            node.size = node.size.saturating_sub(size);
            node.items = node.items.saturating_sub(items);
            cur = node.parent;
            // Shrinking a node can change its rank among its siblings.
            if let Some(gp) = cur {
                sort_children(&mut self.nodes, gp);
            }
        }
    }
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cloud_only: AtomicU64,
    pub cancelled: AtomicBool,
    pub current: Mutex<String>,
    /// Running directory totals for drawing the chart mid-scan.
    pub live: Arc<LiveNode>,
    /// When the outline and hotspot passes finished (ms after the scan started; 0 = not yet).
    pub outline_done_ms: AtomicU64,
    pub hotspots_done_ms: AtomicU64,
    /// Set when scanning the startup disk: its exact per-volume usage.
    pub layout: std::sync::OnceLock<Arc<DiskLayout>>,
    /// Folders listed so far.
    pub dirs: AtomicU64,
    /// Files and folders on the volume (when scanning a whole volume), for progress.
    pub expected_items: std::sync::OnceLock<u64>,
    /// Hotspot folders as they finish, with exact sizes.
    pub early_findings: Mutex<Vec<findings::Early>>,
    pub started: std::sync::OnceLock<std::time::Instant>,
}

struct Raw {
    name: String,
    size: u64,
    kind: Kind,
    items: u64,
    children: Vec<Raw>,
}

struct Walker<'a> {
    progress: &'a Progress,
    allowed_devices: HashSet<u64>,
    skip: HashSet<PathBuf>,
    hardlinks: Mutex<HashSet<(u64, u64)>>,
    /// Hotspot folders already scanned; the main walk splices these in instead of
    /// reading them again.
    prescanned: Mutex<HashMap<PathBuf, Raw>>,
    /// Read-only copy of the prescanned paths, so the main walk can check membership
    /// without taking a lock for every folder.
    prescanned_paths: HashSet<PathBuf>,
}

/// Allocated size on disk, which is what actually frees up when a file is deleted.
fn disk_size(meta: &fs::Metadata) -> u64 {
    meta.blocks() * 512
}

impl Walker<'_> {

    fn admissible_dir(&self, path: &Path, entry: &dirlist::Entry) -> bool {
        entry.is_dir && !entry.dataless && !self.skip.contains(path) && self.allowed_devices.contains(&entry.dev)
    }

    /// Outline pass: list the top `depth_left` levels breadth-first so every folder there
    /// is named on screen at once. Records structure only; the main walk counts the bytes.
    fn outline(&self, path: &Path, live: &LiveNode, depth_left: usize) {
        if depth_left == 0 || self.progress.cancelled.load(Ordering::Relaxed) {
            return;
        }
        crate::clock::gate();
        let Ok(listing) = dirlist::Dir::open(path).and_then(|dir| dir.list(path, false)) else { return };
        let subdirs: Vec<(PathBuf, Arc<LiveNode>)> = listing
            .entries
            .iter()
            .filter_map(|entry| {
                let child = path.join(&entry.name);
                self.admissible_dir(&child, entry).then(|| (child, live.child(&entry.name)))
            })
            .collect();
        subdirs.par_iter().for_each(|(child, node)| self.outline(child, node, depth_left - 1));
    }

    /// Hotspot pass: scan likely space hogs first so their totals are exact within seconds.
    fn prescan_hotspots(&self, root: &Path, hotspots: &[(PathBuf, usize)]) {
        let results: Vec<(PathBuf, Raw)> = hotspots
            .par_iter()
            .filter_map(|(path, category)| {
                let relative = path.strip_prefix(root).ok()?;
                // Attach to the live tree at the same node the main walk will use.
                let mut live = self.progress.live.clone();
                let mut depth = 0;
                for component in relative.components() {
                    depth += 1;
                    if depth <= LIVE_DEPTH {
                        live = live.child(&component.as_os_str().to_string_lossy());
                    }
                }
                // Read the folder's own allocation now (the same attribute its parent's listing
                // reports), so its total is exact, and final, as soon as this pass ends.
                let own = dirlist::dir_alloc(path).unwrap_or(0);
                let raw = self.walk_dir(None, path, display_name(path), own, &live, depth);
                if depth <= LIVE_DEPTH {
                    live.done.store(true, Ordering::Release);
                }
                // Publish the exact size straight away, as a finding.
                self.progress.early_findings.lock().unwrap().push(findings::Early {
                    category: *category,
                    path: path.clone(),
                    size: raw.size,
                    at_ms: self.progress.started.get().map(|s| s.elapsed().as_millis() as u64).unwrap_or(0),
                });
                Some((path.clone(), raw))
            })
            .collect();
        self.prescanned.lock().unwrap().extend(results);
    }

    /// `live` is this directory's own live node if it is within `LIVE_DEPTH`, else its
    /// nearest live ancestor's.
    fn walk_dir(
        &self,
        parent: Option<&dirlist::Dir>,
        path: &Path,
        name: String,
        own: u64,
        live: &LiveNode,
        depth: usize,
    ) -> Raw {
        if self.progress.cancelled.load(Ordering::Relaxed) {
            return Raw { name, size: 0, kind: Kind::Dir, items: 0, children: Vec::new() };
        }
        crate::clock::gate();
        if let Ok(mut current) = self.progress.current.try_lock() {
            *current = path.to_string_lossy().into_owned();
        }
        self.progress.dirs.fetch_add(1, Ordering::Relaxed);

        // Relative open is the fast path; fall back to the full path (e.g. on EMFILE).
        let dir = match parent {
            Some(parent) => parent.open_at(&name).or_else(|_| dirlist::Dir::open(path)),
            None => dirlist::Dir::open(path),
        };
        let listing = dir.and_then(|dir| dir.list(path, false).map(|listing| (dir, listing)));
        let (dir, entries) = match listing {
            Ok((dir, listing)) => {
                if listing.errors > 0 {
                    self.progress.errors.fetch_add(listing.errors, Ordering::Relaxed);
                }
                (Some(dir), listing.entries)
            }
            Err(_) => {
                self.progress.errors.fetch_add(1, Ordering::Relaxed);
                (None, Vec::new())
            }
        };

        // Files are cheap: tally them inline and publish progress once per directory.
        // Only subdirectories become parallel tasks.
        let mut children: Vec<Raw> = Vec::with_capacity(entries.len());
        let mut subdirs = Vec::new();
        let (mut files, mut bytes) = (0u64, 0u64);
        for entry in entries {
            if entry.is_dir {
                subdirs.push(entry);
                continue;
            }
            let mut size = entry.size;
            if entry.nlink > 1 && !self.hardlinks.lock().unwrap().insert((entry.dev, entry.ino)) {
                size = 0;
            }
            files += 1;
            bytes += size;
            children.push(Raw { name: entry.name, size, kind: Kind::File, items: 1, children: Vec::new() });
        }
        self.progress.files.fetch_add(files, Ordering::Relaxed);
        self.progress.bytes.fetch_add(bytes, Ordering::Relaxed);
        live.record(own + bytes, files);

        let subdir_results: Vec<Raw> = subdirs
            .into_par_iter()
            .filter_map(|entry| self.walk_subdir(dir.as_ref(), path, entry, live, depth + 1))
            .collect();
        drop(dir);
        children.extend(subdir_results);
        children.sort_by(|a, b| b.size.cmp(&a.size));

        let size = own + children.iter().map(|c| c.size).sum::<u64>();
        let items = children.iter().map(|c| c.items).sum();
        Raw { name, size, kind: Kind::Dir, items, children }
    }

    fn walk_subdir(
        &self,
        dir: Option<&dirlist::Dir>,
        parent: &Path,
        entry: dirlist::Entry,
        parent_live: &LiveNode,
        depth: usize,
    ) -> Option<Raw> {
        let path = parent.join(&entry.name);
        if self.skip.contains(&path) || !self.allowed_devices.contains(&entry.dev) {
            return None;
        }
        let own_live = (depth <= LIVE_DEPTH).then(|| parent_live.child(&entry.name));
        let live = own_live.as_deref().unwrap_or(parent_live);
        if self.prescanned_paths.contains(&path) {
            if let Some(raw) = self.prescanned.lock().unwrap().remove(&path) {
                return Some(raw);
            }
        }
        if entry.dataless {
            // Its contents live in the cloud and take no space here; don't make macOS fetch them.
            self.progress.cloud_only.fetch_add(1, Ordering::Relaxed);
            live.record(entry.size, 0);
            // Only the folder's own node is final; `live` may be an ancestor's node here.
            if let Some(own) = &own_live {
                own.done.store(true, Ordering::Release);
            }
            return Some(Raw { name: entry.name, size: entry.size, kind: Kind::Dir, items: 0, children: Vec::new() });
        }
        let raw = self.walk_dir(dir, &path, entry.name, entry.size, live, depth);
        if let Some(own) = &own_live {
            if !self.progress.cancelled.load(Ordering::Relaxed) {
                own.done.store(true, Ordering::Release);
            }
        }
        Some(raw)
    }
}

pub fn scan(root: &Path, progress: &Progress) -> Tree {
    let _ = progress.started.set(std::time::Instant::now());
    if root == Path::new("/") {
        if let Some(layout) = disk::startup_layout(startup_disk_name()) {
            // Scan only the Data volume: the sealed macOS volume, Preboot, swap and the
            // rest come from APFS's exact per-volume usage instead.
            let layout = Arc::new(layout);
            let _ = progress.layout.set(layout.clone());
            if let Some(items) = volume_items(&layout.data_root) {
                let _ = progress.expected_items.set(items);
            }
            let bases = findings::Bases::for_root(&layout.data_root);
            let mut tree = scan_with_bases(&layout.data_root, progress, &bases);
            add_volume_slices(&mut tree, &layout);
            return tree;
        }
    }
    if volume_used(root).is_some() {
        if let Some(items) = volume_items(root) {
            let _ = progress.expected_items.set(items);
        }
    }
    scan_with_bases(root, progress, &findings::Bases::for_root(root))
}

/// Files and folders in use on the volume mounted at `mount` (exact, from APFS).
pub fn volume_items(mount: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(mount.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_files.saturating_sub(stat.f_ffree))
}

/// Turn a Data-volume tree into a whole-disk tree: the other volumes become exact slices,
/// and whatever the Data volume holds beyond what could be read becomes "Not readable",
/// so the total matches what Finder reports as used.
fn add_volume_slices(tree: &mut Tree, layout: &DiskLayout) {
    let scanned = tree.nodes[Tree::ROOT].size;
    let unreadable = layout.data_used.saturating_sub(scanned);
    let slices = layout
        .extras
        .iter()
        .cloned()
        .chain((unreadable > 0).then(|| (disk::NOT_READABLE.to_string(), unreadable)));
    let mut added = 0;
    for (name, size) in slices {
        let ix = tree.nodes.len();
        tree.nodes.push(Node { name: name.into(), size, kind: Kind::Other, parent: Some(Tree::ROOT), children: Vec::new(), items: 0 });
        tree.nodes[Tree::ROOT].children.push(ix);
        added += size;
    }
    let root = &mut tree.nodes[Tree::ROOT];
    root.name = layout.name.clone().into();
    root.size += added;
    sort_children(&mut tree.nodes, Tree::ROOT);
}

/// Largest first, except the "not scanned / not readable" remainder, which always
/// comes last so it reads as the tail of the ring.
pub fn sort_children(nodes: &mut [Node], ix: usize) {
    let mut children = std::mem::take(&mut nodes[ix].children);
    children.sort_by_key(|&c| {
        let node = &nodes[c];
        let remainder = node.kind == Kind::Other && (node.name.as_ref() == disk::NOT_SCANNED || node.name.as_ref() == disk::NOT_READABLE);
        (remainder, std::cmp::Reverse(node.size))
    });
    nodes[ix].children = children;
}

/// The startup disk's name as Finder shows it.
pub fn startup_disk_name() -> String {
    fs::read_dir("/Volumes")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .find(|entry| fs::canonicalize(entry.path()).ok().as_deref() == Some(Path::new("/")))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .unwrap_or_else(|| "Startup Disk".to_string())
}

/// `bases` locate the hotspot folders (none: no hotspot pass).
fn scan_with_bases(root: &Path, progress: &Progress, bases: &findings::Bases) -> Tree {
    let root_meta = fs::symlink_metadata(root).ok();
    let mut allowed_devices: HashSet<u64> = root_meta.iter().map(|m| m.dev()).collect();
    let mut skip: HashSet<PathBuf> = ["/dev", "/Volumes", "/System/Volumes", "/net", "/home"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    skip.remove(root);

    // On macOS, `/` is a sealed system volume whose user data lives on a separate
    // APFS volume that is stitched in with firmlinks (e.g. /Users, /Applications).
    if root == Path::new("/") {
        if let Ok(data) = fs::metadata("/System/Volumes/Data") {
            allowed_devices.insert(data.dev());
        }
    }

    let mut walker = Walker {
        progress,
        allowed_devices,
        skip,
        hardlinks: Mutex::new(HashSet::new()),
        prescanned: Mutex::new(HashMap::new()),
        prescanned_paths: HashSet::new(),
    };
    let own = root_meta.as_ref().map(disk_size).unwrap_or(0);
    dirlist::raise_fd_limit();
    dirlist::disable_cloud_downloads();
    let walk_start = std::time::Instant::now();
    let elapsed_ms = || walk_start.elapsed().as_millis() as u64;
    walker.outline(root, &progress.live, OUTLINE_DEPTH);
    progress.outline_done_ms.store(elapsed_ms().max(1), Ordering::Relaxed);
    let hotspots = hotspots(root, bases, &walker);
    walker.prescan_hotspots(root, &hotspots);
    walker.prescanned_paths = hotspots.into_iter().map(|(path, _)| path).collect();
    progress.hotspots_done_ms.store(elapsed_ms().max(1), Ordering::Relaxed);
    let raw = walker.walk_dir(None, root, display_name(root), own, &progress.live, 0);
    debug_assert!(walker.prescanned.lock().unwrap().is_empty(), "a hotspot was never spliced in");
    let flatten_start = std::time::Instant::now();

    let mut nodes = Vec::new();
    flatten(raw, None, &mut nodes);
    if std::env::var_os("PETAL_PHASES").is_some() {
        eprintln!("  walk {:.3}s  flatten {:.3}s", (flatten_start - walk_start).as_secs_f64(), flatten_start.elapsed().as_secs_f64());
    }
    Tree {
        root_path: root.to_path_buf(),
        nodes,
        errors: progress.errors.load(Ordering::Relaxed),
        cloud_only: progress.cloud_only.load(Ordering::Relaxed),
    }
}

/// What deleting these paths would actually free, walking them with APFS clone reporting
/// on (in parallel). Cloned data counts only if every file sharing it is among the paths;
/// a partially shared file frees just its private bytes. Runs off the UI thread.
pub fn frees_of(paths: &[PathBuf]) -> u64 {
    #[derive(Default)]
    struct Tally {
        freed: AtomicU64,
        // clone id -> (members found, members in total, bytes)
        families: Mutex<HashMap<u64, (u32, u32, u64)>>,
        // (device, inode) -> (links found, links in total, bytes): a hard-linked file is
        // only freed once every link to it is deleted.
        links: Mutex<HashMap<(u64, u64), (u64, u64, u64)>>,
    }
    fn file(tally: &Tally, path: &Path, size: u64, sharing: Option<dirlist::Sharing>, link: Option<(u64, u64, u64)>) {
        if let Some((dev, ino, nlink)) = link.filter(|l| l.2 > 1) {
            let mut links = tally.links.lock().unwrap();
            let entry = links.entry((dev, ino)).or_insert((0, nlink, size));
            entry.0 += 1;
            return;
        }
        match sharing {
            Some(info) if info.all && info.refs > 1 => {
                let mut families = tally.families.lock().unwrap();
                let family = families.entry(info.clone_id).or_insert((0, info.refs, 0));
                family.0 += 1;
                family.2 = family.2.max(size);
            }
            Some(_) => {
                tally.freed.fetch_add(dirlist::private_size(path).unwrap_or(size).min(size), Ordering::Relaxed);
            }
            None => {
                tally.freed.fetch_add(size, Ordering::Relaxed);
            }
        }
    }
    fn dir(tally: &Tally, path: &Path, dev: u64) {
        let Ok(listing) = dirlist::Dir::open(path).and_then(|d| d.list(path, true)) else { return };
        let mut subdirs = Vec::new();
        for entry in listing.entries {
            if entry.is_dir {
                tally.freed.fetch_add(entry.size, Ordering::Relaxed);
                if !entry.dataless && entry.dev == dev {
                    subdirs.push(path.join(&entry.name));
                }
            } else {
                let link = Some((entry.dev, entry.ino, entry.nlink));
                file(tally, &path.join(&entry.name), entry.size, entry.sharing, link);
            }
        }
        subdirs.par_iter().for_each(|sub| dir(tally, sub, dev));
    }
    let tally = Tally::default();
    paths.par_iter().for_each(|path| {
        let Ok(meta) = fs::symlink_metadata(path) else { return };
        if meta.is_dir() {
            tally.freed.fetch_add(dirlist::dir_alloc(path).unwrap_or(0), Ordering::Relaxed);
            dir(&tally, path, meta.dev());
        } else {
            file(&tally, path, disk_size(&meta), dirlist::file_sharing(path), Some((meta.dev(), meta.ino(), meta.nlink() as u64)));
        }
    });
    let families = tally.families.into_inner().unwrap();
    let links = tally.links.into_inner().unwrap();
    tally.freed.into_inner()
        + families.values().filter(|(found, refs, _)| found >= refs).map(|f| f.2).sum::<u64>()
        + links.values().filter(|(found, total, _)| found >= total).map(|l| l.2).sum::<u64>()
}

/// Levels listed breadth-first before the main walk, so the chart can name them at once.
const OUTLINE_DEPTH: usize = 2;

/// Hotspots that exist under `root`, on a scanned volume, and don't nest inside each other.
/// Each comes with its index in the findings catalog.
fn hotspots(root: &Path, bases: &findings::Bases, walker: &Walker) -> Vec<(PathBuf, usize)> {
    let mut found: Vec<(PathBuf, usize)> = Vec::new();
    for (category, entry) in findings::CATALOG.iter().enumerate() {
        let Some(path) = bases.locate(entry) else { continue };
        if path == root || !path.starts_with(root) || found.iter().any(|(f, _)| path.starts_with(f) || f.starts_with(&path)) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        let dataless = {
            use std::os::macos::fs::MetadataExt as _;
            meta.st_flags() & 0x4000_0000 != 0
        };
        if !meta.is_dir() || dataless || !walker.allowed_devices.contains(&meta.dev()) {
            continue;
        }
        // Only folders the walk itself would skip matter, i.e. those below the scan root.
        if path.ancestors().take_while(|a| *a != root).any(|a| walker.skip.contains(a)) {
            continue;
        }
        found.push((path, category));
    }
    found
}

/// Time `scan` over several runs and print a stable fingerprint of the result.
pub fn bench(root: &Path, runs: usize) {
    let mut times = Vec::new();
    for run in 0..runs {
        let progress = Progress::default();
        let start = std::time::Instant::now();
        let tree = scan(root, &progress);
        let elapsed = start.elapsed().as_secs_f64();
        times.push(elapsed);
        if std::env::var_os("PETAL_PHASES").is_some() {
            let t = std::time::Instant::now();
            let mut found = findings::from_tree(&tree, &findings::Bases::for_root(&tree.root_path));
            let instant = t.elapsed().as_secs_f64();
            for finding in found.iter_mut() {
                let paths: Vec<PathBuf> = finding.nodes.iter().map(|&ix| tree.path_of(ix)).collect();
                let t = std::time::Instant::now();
                let frees = frees_of(&paths);
                eprintln!("  {}: allocated {}, frees {} (worked out in {:.2}s)", finding.title, format_size(finding.size), format_size(frees), t.elapsed().as_secs_f64());
                finding.size = frees;
            }
            eprintln!(
                "  findings {:.2}s: {}",
                instant,
                found.iter().map(|f| format!("{} {}", f.title, format_size(f.size))).collect::<Vec<_>>().join(", ")
            );
        }
        let root_node = &tree.nodes[Tree::ROOT];
        if let Some(layout) = progress.layout.get() {
            let other: u64 = root_node.children.iter().filter(|&&c| tree.nodes[c].kind == Kind::Other).map(|&c| tree.nodes[c].size).sum();
            eprintln!(
                "  disk used {}  data scanned {}  other volumes + not readable {}  chart total {} ({})",
                layout.container_used,
                root_node.size - other,
                other,
                root_node.size,
                if root_node.size == layout.container_used { "matches disk" } else { "MISMATCH" }
            );
        }
        eprintln!(
            "run {run}: {elapsed:.3}s  files={} bytes={} nodes={} errors={} cloud_only={}",
            root_node.items,
            root_node.size,
            tree.nodes.len(),
            tree.errors,
            tree.cloud_only
        );
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("median {:.3}s  min {:.3}s  max {:.3}s", times[times.len() / 2], times[0], times[times.len() - 1]);
}

/// Bytes in use on the volume, if `root` is the top of a volume (so a full scan of
/// it should account for roughly that much).
pub fn volume_used(root: &Path) -> Option<u64> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(root.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let mount_point = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) };
    if Path::new(std::ffi::OsStr::from_bytes(mount_point.to_bytes())) != root {
        return None;
    }
    let (total, free) = statvfs(root)?;
    Some(total.saturating_sub(free))
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn flatten(raw: Raw, parent: Option<usize>, nodes: &mut Vec<Node>) -> usize {
    let ix = nodes.len();
    nodes.push(Node {
        name: raw.name.into(),
        size: raw.size,
        kind: raw.kind,
        parent,
        children: Vec::with_capacity(raw.children.len()),
        items: raw.items,
    });
    for child in raw.children {
        let child_ix = flatten(child, Some(ix), nodes);
        nodes[ix].children.push(child_ix);
    }
    ix
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    value /= 1000.0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

pub fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub struct Volume {
    pub name: String,
    pub path: PathBuf,
    pub total: u64,
    pub free: u64,
}

fn statvfs(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let frsize = stat.f_frsize as u64;
    Some((stat.f_blocks as u64 * frsize, stat.f_bavail as u64 * frsize))
}

/// The startup disk plus anything mounted under /Volumes.
pub fn volumes() -> Vec<Volume> {
    let mut root_name = String::from("Startup Disk");
    let mut others = Vec::new();
    let root_dev = fs::metadata("/").map(|m| m.dev()).ok();
    let startup_container = disk::container_at(Path::new("/"));
    let data_dev = fs::metadata("/System/Volumes/Data").map(|m| m.dev()).ok();
    if let Ok(entries) = fs::read_dir("/Volumes") {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if fs::canonicalize(&path).ok().as_deref() == Some(Path::new("/")) {
                root_name = name;
                continue;
            }
            // Unmounted placeholders (e.g. /Volumes/Recovery) are plain folders on the startup disk.
            let dev = fs::metadata(&path).map(|m| m.dev()).ok();
            if dev.is_none() || dev == root_dev || dev == data_dev {
                continue;
            }
            // Recovery, Update and the like live in the startup disk's own container: they
            // are part of the startup disk, not separate disks.
            if startup_container.is_some() && disk::container_at(&path) == startup_container {
                continue;
            }
            if let Some((total, free)) = statvfs(&path) {
                if total > 0 {
                    others.push(Volume { name, path, total, free });
                }
            }
        }
    }
    let mut volumes = Vec::new();
    if let Some((total, free)) = statvfs(Path::new("/")) {
        volumes.push(Volume { name: root_name, path: PathBuf::from("/"), total, free });
    }
    others.sort_by(|a, b| a.name.cmp(&b.name));
    volumes.extend(others);
    volumes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; bytes]).unwrap();
    }

    #[test]
    fn scans_sizes_and_removes_nodes() {
        let dir = std::env::temp_dir().join(format!("petal-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir.join("big/a.bin"), 1 << 20);
        write(&dir.join("big/nested/b.bin"), 1 << 19);
        write(&dir.join("small.txt"), 10);
        fs::hard_link(dir.join("big/a.bin"), dir.join("link.bin")).unwrap();

        let tree = scan(&dir, &Progress::default());
        let root = &tree.nodes[Tree::ROOT];
        assert_eq!(root.items, 4);
        // Two links to the same 1 MiB file count once, whichever the parallel walk sees first.
        assert!(root.size < (1 << 20) * 2);
        assert!(root.size >= (1 << 20) + (1 << 19));
        let big = root.children.iter().copied()
            .find(|&c| tree.nodes[c].name.as_ref() == "big").unwrap();
        // Children are sorted largest first and path_of round-trips.
        let sizes: Vec<u64> = root.children.iter().map(|&c| tree.nodes[c].size).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]));
        assert_eq!(tree.path_of(big), dir.join("big"));

        let mut tree = tree;
        let nested = tree.nodes[big].children.iter().copied()
            .find(|&c| tree.nodes[c].name.as_ref() == "nested").unwrap();
        let (before_root, nested_size) = (tree.nodes[Tree::ROOT].size, tree.nodes[nested].size);
        tree.remove(nested);
        assert_eq!(tree.nodes[Tree::ROOT].size, before_root - nested_size);
        assert_eq!(tree.nodes[Tree::ROOT].items, 3);
        assert!(!tree.nodes[big].children.contains(&nested));

        fs::remove_dir_all(&dir).unwrap();
    }

    /// Hotspots are scanned first and spliced in; the result must be identical to a
    /// plain scan, and the live totals must add up to the same bytes.
    #[test]
    fn hotspot_splice_matches_plain_scan() {
        let dir = std::env::temp_dir().join(format!("petal-hotspots-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let home = dir.join("Users/someone");
        write(&home.join("Downloads/movie.mov"), 3 << 20);
        write(&home.join("Downloads/nested/deeper/a.zip"), 1 << 20);
        write(&home.join(".Trash/old.dmg"), 2 << 20);
        write(&home.join("Library/Caches/com.app/blob"), 1 << 19);
        write(&home.join("Library/Prefs/p.plist"), 100);
        write(&home.join("Documents/notes.txt"), 5000);
        write(&dir.join("Applications/App.app/bin"), 1 << 20);

        let fingerprint = |tree: &Tree| {
            let root = &tree.nodes[Tree::ROOT];
            (root.size, root.items, tree.nodes.len())
        };
        let plain = scan_with_bases(&dir, &Progress::default(), &findings::Bases::default());
        let progress = Progress::default();
        let spliced = scan_with_bases(&dir, &progress, &findings::Bases { home: Some(home.clone()), user_temp: None });
        assert_eq!(fingerprint(&plain), fingerprint(&spliced));
        let live = crate::live::snapshot(&progress.live, &dir, None).tree;
        assert_eq!(live.nodes[Tree::ROOT].size, spliced.nodes[Tree::ROOT].size, "live totals");
        // Same per-folder sizes, not just the same total.
        let downloads = |tree: &Tree| {
            let users = tree.nodes[Tree::ROOT].children.iter().find(|&&c| tree.nodes[c].name.as_ref() == "Users").copied().unwrap();
            let someone = tree.nodes[users].children[0];
            tree.nodes[someone].children.iter().map(|&c| (tree.nodes[c].name.to_string(), tree.nodes[c].size)).collect::<Vec<_>>()
        };
        assert_eq!(downloads(&plain), downloads(&spliced));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(512), "512 bytes");
        assert_eq!(format_size(1_500_000), "1.50 MB");
        assert_eq!(format_size(13_400_000_000), "13.4 GB");
        assert_eq!(format_count(1345677), "1,345,677");
    }
    /// Ground truth for clone accounting on a real APFS volume: a disk image with an
    /// original file, three pure clones, an edited clone and a plain file. Checks the
    /// scanned total against the volume's own usage, then predicts what each deletion
    /// frees and compares with what the volume actually frees.
    /// Run with `cargo test -- --ignored clone_accounting_matches_apfs` (needs `hdiutil`).
    #[test]
    #[ignore]
    fn clone_accounting_matches_apfs() {
        use std::process::Command;
        let run = |cmd: &str, args: &[&str]| {
            let out = Command::new(cmd).args(args).output().unwrap();
            assert!(out.status.success(), "{cmd} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let name = format!("PetalClones{}", std::process::id());
        let image = std::env::temp_dir().join(format!("{name}.sparseimage"));
        let _ = fs::remove_file(&image);
        run("hdiutil", &["create", "-size", "300m", "-fs", "APFS", "-volname", &name, "-type", "SPARSE", image.to_str().unwrap()]);
        let attach = run("hdiutil", &["attach", "-nobrowse", image.to_str().unwrap()]);
        let mount = PathBuf::from(attach.lines().last().unwrap().split('\t').last().unwrap().trim());
        let used = || {
            run("sync", &[]);
            std::thread::sleep(std::time::Duration::from_millis(500));
            crate::disk::volume_used(&mount).unwrap()
        };

        let random = |path: &Path, mb: usize| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut data = vec![0u8; mb << 20];
            let mut x = 0x9e3779b97f4a7c15u64 ^ mb as u64;
            for chunk in data.chunks_mut(8) {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
            }
            fs::write(path, data).unwrap();
        };
        let before = used();
        random(&mount.join("a/orig.bin"), 40);
        for i in 1..=3 {
            fs::create_dir_all(mount.join("b")).unwrap();
            run("cp", &["-c", mount.join("a/orig.bin").to_str().unwrap(), mount.join(format!("b/clone{i}.bin")).to_str().unwrap()]);
        }
        fs::create_dir_all(mount.join("c")).unwrap();
        run("cp", &["-c", mount.join("a/orig.bin").to_str().unwrap(), mount.join("c/edited.bin").to_str().unwrap()]);
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = fs::OpenOptions::new().write(true).open(mount.join("c/edited.bin")).unwrap();
            f.seek(SeekFrom::Start(10 << 20)).unwrap();
            f.write_all(&vec![7u8; 4 << 20]).unwrap();
        }
        random(&mount.join("d/plain.bin"), 8);
        random(&mount.join("e/linked.bin"), 6);
        fs::create_dir_all(mount.join("f")).unwrap();
        fs::hard_link(mount.join("e/linked.bin"), mount.join("f/link.bin")).unwrap();
        let truth = used() - before;

        let mb = |b: u64| b as f64 / 1e6;
        let tree = scan(&mount, &Progress::default());
        let counted = tree.nodes[Tree::ROOT].size;
        // Sizes are allocation (like Finder): each clone counts its blocks. "Frees" is what
        // must match reality, checked below.
        eprintln!("truth {:.1} MB, allocated (sizes shown) {:.1} MB", mb(truth), mb(counted));

        // Predict, delete, measure; one folder at a time, rescanning in between.
        // e/ first: its file has another link in f/, so deleting e/ frees nothing yet.
        for folder in ["d", "c", "b", "a", "e", "f"] {
            let predicted = frees_of(&[mount.join(folder)]);
            let before = used();
            fs::remove_dir_all(mount.join(folder)).unwrap();
            let actual = before - used();
            eprintln!("delete {folder}/: predicted {:.2} MB, actually freed {:.2} MB", mb(predicted), mb(actual));
            let tolerance = 1 << 20;
            assert!(predicted.abs_diff(actual) <= tolerance, "{folder}: predicted {predicted}, freed {actual}");
        }
        run("hdiutil", &["detach", mount.to_str().unwrap()]);
        fs::remove_file(&image).unwrap();
    }

}
