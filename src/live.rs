//! Running totals published while a scan is in progress, so the chart can be drawn
//! long before the scan finishes.
//!
//! Only the top [`LIVE_DEPTH`] levels of directories get a node. Every scanned
//! directory adds its bytes once to its nearest node, so the cost is one atomic
//! add per directory, and a snapshot only has to copy this small tree.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::disk::{self, DiskLayout};
use crate::scan::{self, Kind, Node, Progress, Tree};

pub const LIVE_DEPTH: usize = 4;

#[derive(Default)]
pub struct LiveNode {
    pub name: String,
    /// Bytes attributed directly here: this directory's own files, plus everything
    /// below it that is deeper than `LIVE_DEPTH`.
    pub bytes: AtomicU64,
    pub files: AtomicU64,
    pub children: Mutex<LiveChildren>,
    /// Set once everything below this folder has been counted: its size is final.
    pub done: AtomicBool,
    /// When a snapshot first had something here, as a sequence number: snapshots list
    /// folders in this order (see `snapshot`).
    seen: OnceLock<u64>,
    /// When a snapshot first found this folder final.
    settled: OnceLock<Instant>,
}

/// Hands out `LiveNode::seen`.
static NEXT_SEEN: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct LiveChildren {
    pub list: Vec<Arc<LiveNode>>,
    index: HashMap<String, usize>,
}

impl LiveNode {
    /// The child with this name, created on first use. The outline, hotspot and main
    /// passes all reach the same folders, and must share one node per folder.
    pub fn child(&self, name: &str) -> Arc<LiveNode> {
        let mut children = self.children.lock().unwrap();
        if let Some(&ix) = children.index.get(name) {
            return children.list[ix].clone();
        }
        let child = Arc::new(LiveNode { name: name.to_owned(), ..Default::default() });
        let ix = children.list.len();
        children.index.insert(name.to_owned(), ix);
        children.list.push(child.clone());
        child
    }

    pub fn record(&self, bytes: u64, files: u64) {
        if bytes > 0 {
            self.bytes.fetch_add(bytes, Ordering::Relaxed);
        }
        if files > 0 {
            self.files.fetch_add(files, Ordering::Relaxed);
        }
    }
}

/// A `Tree` of directory totals as they stand right now. For the startup disk, the
/// other volumes and the not-yet-scanned remainder are added so the chart always shows
/// the whole disk.
///
/// Folders are listed in the order they first had something in them, not by size: while
/// they're being counted, folders constantly overtake each other, and re-sorting would
/// send them sliding across one another in the chart at every snapshot. This way the
/// chart grows in place; the results are sorted by size. Folders appearing together go
/// largest first.
pub fn snapshot(live: &LiveNode, root_path: &Path, layout: Option<&DiskLayout>) -> LiveSnapshot {
    let mut nodes = Vec::new();
    let mut done = Vec::new();
    let mut settled = Vec::new();
    let now = crate::clock::now();
    add(live, scan::display_name(root_path), None, now, &mut nodes, &mut done, &mut settled);
    if let Some(layout) = layout {
        let scanned = nodes[Tree::ROOT].size;
        let remaining = layout.data_used.saturating_sub(scanned);
        let slices = layout.extras.iter().cloned().chain(std::iter::once((disk::NOT_SCANNED.to_string(), remaining)));
        for (name, size) in slices {
            let ix = nodes.len();
            // The volume slices are exact from the start; the remainder obviously isn't.
            done.push(name != disk::NOT_SCANNED);
            settled.push(None);
            nodes.push(Node { name: name.into(), size, kind: Kind::Other, parent: Some(Tree::ROOT), children: Vec::new(), items: 0 });
            nodes[Tree::ROOT].children.push(ix);
            nodes[Tree::ROOT].size += size;
        }
        // Added after the folders, with the remainder last, so the order holds still too.
        nodes[Tree::ROOT].name = layout.name.clone().into();
    }
    LiveSnapshot { tree: Tree { root_path: root_path.to_path_buf(), nodes, errors: 0, cloud_only: 0, ..Default::default() }, done, settled }
}

/// The live totals as a tree, plus which folders are final and since when (indexed like
/// `tree.nodes`).
pub struct LiveSnapshot {
    pub tree: Tree,
    pub done: Vec<bool>,
    pub settled: Vec<Option<Instant>>,
}

fn add(
    live: &LiveNode,
    name: String,
    parent: Option<usize>,
    now: Instant,
    nodes: &mut Vec<Node>,
    done: &mut Vec<bool>,
    settled: &mut Vec<Option<Instant>>,
) -> usize {
    let ix = nodes.len();
    let is_done = live.done.load(Ordering::Acquire);
    done.push(is_done);
    settled.push(is_done.then(|| *live.settled.get_or_init(|| now)));
    nodes.push(Node {
        name: name.into(),
        size: 0,
        kind: Kind::Dir,
        parent,
        children: Vec::new(),
        items: 0,
    });
    let kids: Vec<Arc<LiveNode>> = live.children.lock().unwrap().list.clone();
    let mut size = live.bytes.load(Ordering::Relaxed);
    let mut items = live.files.load(Ordering::Relaxed);
    let mut children = Vec::with_capacity(kids.len());
    for kid in &kids {
        let child = add(kid, kid.name.clone(), Some(ix), now, nodes, done, settled);
        size += nodes[child].size;
        items += nodes[child].items;
        children.push((child, kid));
    }
    children.sort_by_key(|&(c, _)| std::cmp::Reverse(nodes[c].size));
    for (c, kid) in &children {
        if nodes[*c].size > 0 {
            kid.seen.get_or_init(|| NEXT_SEEN.fetch_add(1, Ordering::Relaxed));
        }
    }
    // Stable, so folders with nothing in them yet stay in size order, at the end.
    children.sort_by_key(|(_, kid)| kid.seen.get().copied().unwrap_or(u64::MAX));
    let children: Vec<usize> = children.into_iter().map(|(c, _)| c).collect();
    let node = &mut nodes[ix];
    node.size = size;
    node.items = items;
    node.children = children;
    ix
}

/// Real top-level folders: not the exact volume slices or the not-yet-scanned remainder,
/// which are known up front and would make every metric look instant.
fn folders(tree: &Tree) -> impl Iterator<Item = usize> + '_ {
    tree.nodes[Tree::ROOT].children.iter().copied().filter(|&c| tree.nodes[c].kind != Kind::Other)
}

/// Bytes found by the scan so far.
fn scanned(tree: &Tree) -> u64 {
    let other: u64 = tree.nodes[Tree::ROOT].children.iter().filter(|&&c| tree.nodes[c].kind == Kind::Other).map(|&c| tree.nodes[c].size).sum();
    tree.nodes[Tree::ROOT].size - other
}

/// Bytes in folders already marked final, counting each final folder once (not again
/// for final folders inside it), plus the root's own files once the root is final.
fn final_bytes(snapshot: &LiveSnapshot) -> u64 {
    fn walk(s: &LiveSnapshot, ix: usize) -> u64 {
        let node = &s.tree.nodes[ix];
        if s.done[ix] && node.kind == Kind::Dir {
            return node.size;
        }
        node.children.iter().filter(|&&c| s.tree.nodes[c].kind == Kind::Dir).map(|&c| walk(s, c)).sum()
    }
    walk(snapshot, Tree::ROOT)
}

/// The size of the folder at `ix` in `from`, looked up by path in `tree`.
fn find_size(tree: &Tree, from: &Tree, ix: usize) -> Option<u64> {
    let mut names = Vec::new();
    let mut cur = ix;
    while let Some(parent) = from.nodes[cur].parent {
        names.push(from.nodes[cur].name.clone());
        cur = parent;
    }
    let mut at = Tree::ROOT;
    for name in names.iter().rev() {
        at = *tree.nodes[at].children.iter().find(|&&c| &tree.nodes[c].name == name)?;
    }
    Some(tree.nodes[at].size)
}

/// Top-level shares of the scanned bytes, with the root's own files as their own bucket.
fn shares(tree: &Tree) -> HashMap<String, f64> {
    let total = scanned(tree).max(1) as f64;
    let mut out: HashMap<String, f64> =
        folders(tree).map(|c| (tree.nodes[c].name.to_string(), tree.nodes[c].size as f64 / total)).collect();
    let in_children: u64 = folders(tree).map(|c| tree.nodes[c].size).sum();
    out.insert("\0files".into(), scanned(tree).saturating_sub(in_children) as f64 / total);
    out
}

/// Total-variation distance between two share distributions (0 = identical, 1 = disjoint).
fn distance(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let keys: std::collections::HashSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .map(|k| (a.get(k).unwrap_or(&0.0) - b.get(k).unwrap_or(&0.0)).abs())
        .sum::<f64>()
        / 2.0
}

/// `petal --bench-live <path> [runs]`: how soon the live chart shows the right picture.
pub fn bench(root: &Path, runs: usize) {
    let mut results = Vec::new();
    for run in 0..runs {
        let progress = Arc::new(Progress::default());
        let start = Instant::now();
        let handle = {
            let (progress, root) = (progress.clone(), root.to_path_buf());
            std::thread::spawn(move || {
                let tree = scan::scan(&root, &progress);
                (tree, start.elapsed())
            })
        };
        let mut samples: Vec<(f64, LiveSnapshot)> = Vec::new();
        // Items (files + folders) scanned at each sample, for progress and time-left.
        let mut items_at: Vec<u64> = Vec::new();
        let mut snapshot_cost = Duration::ZERO;
        let mut slowest = Duration::ZERO;
        while !handle.is_finished() {
            std::thread::sleep(Duration::from_millis(100));
            let t = start.elapsed().as_secs_f64();
            let before = Instant::now();
            let snap = snapshot(&progress.live, root, progress.layout.get().map(|l| &**l));
            let cost = before.elapsed();
            snapshot_cost += cost;
            slowest = slowest.max(cost);
            samples.push((t, snap));
            items_at.push(progress.files.load(Ordering::Relaxed) + progress.dirs.load(Ordering::Relaxed));
        }
        let (tree, total) = handle.join().unwrap();
        let outline_s = progress.outline_done_ms.load(Ordering::Relaxed) as f64 / 1000.0;
        let hotspots_s = progress.hotspots_done_ms.load(Ordering::Relaxed) as f64 / 1000.0;
        let final_live = snapshot(&progress.live, root, progress.layout.get().map(|l| &**l));
        let final_snap = &final_live.tree;
        let gate = final_snap.nodes[Tree::ROOT].size == tree.nodes[Tree::ROOT].size;
        // Every folder that was ever marked final must have its final size by then.
        let mut premature = 0;
        for (_, sample) in &samples {
            for (ix, &done) in sample.done.iter().enumerate() {
                let node = &sample.tree.nodes[ix];
                if done && node.kind == Kind::Dir && ix != Tree::ROOT {
                    if let Some(final_size) = find_size(final_snap, &sample.tree, ix) {
                        if final_size != node.size {
                            if premature < 8 && std::env::var_os("PETAL_DEBUG_PREMATURE").is_some() {
                                let path: Vec<String> = {
                                    let mut names = Vec::new();
                                    let mut cur = ix;
                                    while let Some(p) = sample.tree.nodes[cur].parent {
                                        names.push(sample.tree.nodes[cur].name.to_string());
                                        cur = p;
                                    }
                                    names.into_iter().rev().collect()
                                };
                                eprintln!("  premature: /{} marked final at {} but ends {}", path.join("/"), node.size, final_size);
                            }
                            premature += 1;
                        }
                    }
                }
            }
        }

        let target = shares(final_snap);
        let errors: Vec<(f64, f64)> = samples.iter().map(|(t, s)| (*t, distance(&shares(&s.tree), &target))).collect();
        // The chart is "useful" from the first sample after which it never strays past the threshold.
        let useful = |threshold: f64| {
            let mut when = total.as_secs_f64();
            for (t, e) in errors.iter().rev() {
                if *e > threshold {
                    break;
                }
                when = *t;
            }
            when
        };
        let u5 = useful(0.05);
        // Coverage: share of the final bytes already on screen.
        let final_total = scanned(final_snap).max(1) as f64;
        let covered = |fraction: f64| {
            samples
                .iter()
                .find(|(_, s)| scanned(&s.tree) as f64 >= fraction * final_total)
                .map(|(t, _)| *t)
                .unwrap_or(total.as_secs_f64())
        };
        let (c50, c90) = (covered(0.5), covered(0.9));
        // Finality: share of the final bytes that sit in folders already marked final.
        let finalized = |fraction: f64| {
            samples
                .iter()
                .find(|(_, s)| final_bytes(s) as f64 >= fraction * final_total)
                .map(|(t, _)| *t)
                .unwrap_or(total.as_secs_f64())
        };
        let (f50, f90) = (finalized(0.5), finalized(0.9));
        // Ranking: from when the three biggest top-level folders are in their final order for good.
        // (Snapshots list folders in the order they appeared, so sort by size here.)
        let top3 = |tree: &Tree| -> Vec<String> {
            let mut biggest: Vec<usize> = folders(tree).collect();
            biggest.sort_by_key(|&c| std::cmp::Reverse(tree.nodes[c].size));
            biggest.into_iter().take(3).map(|c| tree.nodes[c].name.to_string()).collect()
        };
        let final_top3 = top3(final_snap);
        let mut rank = total.as_secs_f64();
        for (t, s) in samples.iter().rev() {
            if top3(&s.tree) != final_top3 {
                break;
            }
            rank = *t;
        }
        // Findings: when they appeared, and whether each early size equals the final tree's.
        let early = progress.early_findings.lock().unwrap().clone();
        let shown: Vec<&crate::findings::Early> = early.iter().filter(|e| e.size >= crate::findings::MIN_SIZE).collect();
        let first_finding = shown.iter().map(|e| e.at_ms).min().map(|ms| ms as f64 / 1000.0).unwrap_or(f64::NAN);
        let last_finding = shown.iter().map(|e| e.at_ms).max().map(|ms| ms as f64 / 1000.0).unwrap_or(f64::NAN);
        let wrong_findings = early
            .iter()
            .filter(|e| tree.find(&e.path).map(|ix| tree.nodes[ix].size) != Some(e.size))
            .count();
        // Progress bar and time left, exactly as the UI computes them.
        let progress_report = match progress.expected_items.get() {
            Some(&expected) => {
                let total_s = total.as_secs_f64();
                let mut eta = crate::eta::Eta::default();
                let mut errors_at = [f64::NAN; 3];
                let mut last_percent = 0u32;
                let mut last_change = 0.0f64;
                let mut longest_stall = 0.0f64;
                for ((t, _), &items) in samples.iter().zip(&items_at) {
                    eta.update(*t, items);
                    let percent = (crate::eta::fraction(items, expected) * 100.0) as u32;
                    if percent != last_percent {
                        longest_stall = longest_stall.max(t - last_change);
                        last_percent = percent;
                        last_change = *t;
                    }
                    for (slot, mark) in [0.25, 0.5, 0.75].iter().enumerate() {
                        if errors_at[slot].is_nan() && *t >= mark * total_s {
                            if let Some(left) = eta.remaining(*t, items, expected) {
                                errors_at[slot] = left - (total_s - t);
                            }
                        }
                    }
                }
                longest_stall = longest_stall.max(total_s - last_change);
                format!(
                    "  bar ends at {last_percent}%  longest stall {longest_stall:.1}s  eta error @25/50/75% {:+.1}/{:+.1}/{:+.1}s",
                    errors_at[0], errors_at[1], errors_at[2]
                )
            }
            None => String::new(),
        };
        let avg_ms = snapshot_cost.as_secs_f64() * 1000.0 / samples.len().max(1) as f64;
        eprintln!(
            "run {run}: outline {outline_s:.2}s  hotspots {hotspots_s:.2}s  total {:.2}s  cover50 {c50:.2}s  cover90 {c90:.2}s  final50 {f50:.2}s  final90 {f90:.2}s  top3 {rank:.2}s  useful@5% {u5:.2}s  snapshot avg {avg_ms:.2}ms max {:.2}ms nodes {}  gate {}  premature-final {premature}  findings {} (first {first_finding:.2}s, last {last_finding:.2}s, wrong {wrong_findings}){progress_report}",
            total.as_secs_f64(),
            slowest.as_secs_f64() * 1000.0,
            final_snap.nodes.len(),
            if gate { "ok" } else { "MISMATCH" },
            shown.len(),
        );
        results.push([total.as_secs_f64(), c50, c90, rank, u5, f50, f90]);
    }
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let m = |i: usize| median(results.iter().map(|r| r[i]).collect());
    println!(
        "median total {:.2}s  cover50 {:.2}s  cover90 {:.2}s  final50 {:.2}s  final90 {:.2}s  top3 {:.2}s  useful@5% {:.2}s",
        m(0), m(1), m(2), m(5), m(6), m(3), m(4)
    );
}
