use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, FocusHandle, FontWeight, HapticFeedbackStyle, HitboxBehavior,
    Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels,
    PromptLevel, Rgba, ScrollStrategy, SharedString, Stateful, Task, UniformListScrollHandle,
    Window, actions, canvas, div, prelude::*, px, relative, rgb, rgba, uniform_list,
};

use palette::IntoColor;

use crate::classify::{self, CATEGORIES, Category};
use crate::clock;
use crate::disk;
use crate::eta;
use crate::onboarding;
use crate::findings::{self, Finding, Fix, Safety};
use crate::live;
use crate::motion;
use crate::scan::{self, Kind, Progress, Tree, Volume, format_count, format_size};
use crate::sunburst::{self, Geometry, Hit, Segment, Target};
use crate::trashing;
use crate::treemap::{self, TreemapMotion};
use crate::watch;

actions!(petal, [GoUp, OpenFolder, Rescan, StartOver, ShowSunburst, ShowIcicle, ShowTreemap]);

const BG: u32 = 0x1c1d21;
const PANEL: u32 = 0x232529;
const CARD: u32 = 0x2a2c31;
const CARD_HOVER: u32 = 0x33363c;
const BORDER: u32 = 0x34363c;
const TEXT: u32 = 0xe8e9ec;
const MUTED: u32 = 0x8d919a;
const ACCENT: u32 = 0x4f9dff;
const DANGER: u32 = 0xe5484d;
/// ACCENT, faint: the background of something already in the Collector.
const ACCENT_TINT: u32 = 0x4f9dff1f;
const SAFE: u32 = 0x3fb950;
const WARNING: u32 = 0xd29922;

const ROW_HEIGHT: f32 = 30.0;

/// How the chart is coloured.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorBy {
    /// Each slice its own hue along the chart, so neighbouring folders stand apart.
    Folder,
    /// By what things are: apps, caches, photos… (see `classify`).
    Kind,
}

/// How the chart is drawn. All three show the same layout (`sunburst::layout`; the treemap
/// just its top level, see `ChartType::layout`), so colours, hover and zoom mean the same
/// thing in each.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChartType {
    /// Rings round the folder in focus.
    Sunburst,
    /// The sunburst unrolled into columns, one per level, with room for names along each bar
    /// (see `Geometry::icicle`).
    Icicle,
    /// Boxes sized by area (see `treemap`), one for each thing in the folder in focus; click
    /// a folder's box to zoom into it.
    Treemap,
}

impl ChartType {
    /// The chart's segments for the folder `focus`: as many levels as the sunburst and icicle
    /// show, and just the one for the treemap, which shows one level at a time. Keys, labels, swatches and categories are all made from these, so they line up
    /// whichever it is.
    fn layout(self, tree: &Tree, focus: usize) -> Vec<Segment> {
        match self {
            ChartType::Sunburst | ChartType::Icicle => sunburst::layout(tree, focus),
            ChartType::Treemap => sunburst::layout_to(tree, focus, 1),
        }
    }
}

/// How long a notice ("Copied …") stays up.
const NOTICE_TIME: Duration = Duration::from_millis(2500);

/// The label shown when hovering a button.
struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .font_family(".SystemUIFont")
            .text_xs()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(BORDER))
            .text_color(rgb(TEXT))
            .shadow_md()
            .child(self.0.clone())
    }
}

fn tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    move |_, cx| cx.new(|_| Tooltip(text.into())).into()
}

#[derive(Clone)]
struct DraggedItem {
    node: usize,
    name: SharedString,
    size: u64,
}

impl Render for DraggedItem {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_full()
            .bg(rgb(ACCENT))
            .text_color(rgb(0xffffff))
            .text_sm()
            .font_family(".SystemUIFont")
            .child(format!("{}  ·  {}", self.name, format_size(self.size)))
    }
}

enum Screen {
    Start(Vec<Volume>),
    Scanning(Scanning),
    Results(Results),
}

struct Scanning {
    root: PathBuf,
    progress: Arc<Progress>,
    started: Instant,
    /// Used bytes on the volume, when scanning a whole volume, so the chart can
    /// show how much is still to come.
    expected: Option<u64>,
    live: Option<Rc<LiveView>>,
    last_snapshot: Instant,
    chart_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    eta: eta::Eta,
    /// Segments ease between snapshots instead of jumping (`tiles` when it's a treemap).
    motion: Rc<std::cell::RefCell<motion::ChartMotion>>,
    tiles: Rc<std::cell::RefCell<TreemapMotion>>,
    _tasks: [Task<()>; 2],
}

impl Scanning {
    /// Take a fresh snapshot of the running totals for the live chart, laid out for `chart`.
    fn snapshot(&mut self, chart: ChartType) {
        let layout = self.progress.layout.get().map(|l| &**l);
        let root = layout.map(|l| l.data_root.clone()).unwrap_or_else(|| self.root.clone());
        let snapshot = live::snapshot(&self.progress.live, &root, layout);
        self.live = Some(Rc::new(LiveView::new(snapshot, chart)));
        self.last_snapshot = clock::now();
    }
}

/// A snapshot of the running totals, laid out. The scan's chart is look-only: the mouse
/// does nothing until the results are up, so nothing under the pointer changes as the
/// chart grows beneath it.
struct LiveView {
    tree: Tree,
    /// Which folders are final, indexed like `tree.nodes`.
    done: Vec<bool>,
    segments: Rc<Vec<Segment>>,
    swatches: HashMap<usize, Hsla>,
    /// The "not scanned yet" slice of the startup disk, drawn pulsing.
    pending: Option<usize>,
    /// When each recently finalised folder became final, so it can ease into full colour.
    settled_at: HashMap<usize, Instant>,
    /// Each segment's identity (folder path), aligned with `segments`, for smooth motion.
    keys: Vec<motion::Key>,
    /// Each segment's name and size, aligned with `segments`, for the icicle's and treemap's labels.
    labels: Vec<(SharedString, SharedString)>,
    /// Unique per layout, so the animation knows when to retarget.
    id: u64,
}

static NEXT_LAYOUT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// Identifies a set of results, so a check for changes from an earlier scan stops.
static NEXT_RESULTS_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// How often to check for changes on disk once the results are up.
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

const LIVE_REFRESH: Duration = Duration::from_millis(100);
/// How long a folder takes to ease into full colour once its total is final.
const SETTLE_FADE: Duration = Duration::from_millis(600);
/// How long the "scan complete" banner stays up.
const BANNER_TIME: Duration = Duration::from_secs(8);

impl LiveView {
    /// Lay out `snapshot` for `chart`.
    fn new(snapshot: live::LiveSnapshot, chart: ChartType) -> Self {
        let live::LiveSnapshot { tree, done, settled } = snapshot;
        let pending = tree.nodes[Tree::ROOT]
            .children
            .iter()
            .copied()
            .find(|&c| tree.nodes[c].kind == Kind::Other && tree.nodes[c].name.as_ref() == disk::NOT_SCANNED);
        let now = clock::now();
        let settled_at = settled
            .iter()
            .enumerate()
            .filter_map(|(ix, at)| Some((ix, (*at)?)))
            .filter(|&(ix, at)| tree.nodes[ix].kind == Kind::Dir && ix != Tree::ROOT && now - at < SETTLE_FADE)
            .collect();
        let mut view = Self {
            tree,
            done,
            segments: Rc::default(),
            swatches: HashMap::new(),
            pending,
            settled_at,
            keys: Vec::new(),
            labels: Vec::new(),
            id: 0,
        };
        view.lay_out(chart);
        view
    }

    fn lay_out(&mut self, chart: ChartType) {
        let segments = chart.layout(&self.tree, Tree::ROOT);
        self.swatches = segments
            .iter()
            .filter_map(|s| match s.target {
                Target::Node(ix) if s.depth == 1 => Some((ix, sunburst::base_color(s))),
                _ => None,
            })
            .collect();
        self.keys = segments
            .iter()
            .map(|s| match s.target {
                Target::Node(ix) => motion::Key::Node(path_to(&self.tree, ix)),
                Target::Small { first, .. } => motion::Key::Small(path_to(&self.tree, first)),
            })
            .collect();
        // Honest sizes, as in the list: folders still being counted show a lower bound.
        let (tree, done) = (&self.tree, &self.done);
        self.labels = segment_labels(tree, Tree::ROOT, &segments, |ix| {
            let node = &tree.nodes[ix];
            if node.kind == Kind::Dir && !done[ix] { format!("≥ {}", format_size(node.size)) } else { format_size(node.size) }
        });
        self.segments = Rc::new(segments);
        self.id = NEXT_LAYOUT_ID.fetch_add(1, Ordering::Relaxed);
    }

    fn is_final(&self, ix: usize) -> bool {
        self.done[ix]
    }
}

/// Follow folder names down from the root; stops at the deepest folder that exists.
fn resolve_path(tree: &Tree, path: &[SharedString]) -> usize {
    let mut at = Tree::ROOT;
    for name in path {
        match tree.nodes[at].children.iter().find(|&&c| tree.nodes[c].kind == Kind::Dir && &tree.nodes[c].name == name) {
            Some(&child) => at = child,
            None => break,
        }
    }
    at
}

/// Each segment's name and size, for labels on the chart. `size` gives a node's size as shown.
fn segment_labels(tree: &Tree, focus: usize, segments: &[Segment], size: impl Fn(usize) -> String) -> Vec<(SharedString, SharedString)> {
    let total = tree.nodes[focus].size as f64;
    segments
        .iter()
        .map(|s| match s.target {
            Target::Node(ix) => (tree.nodes[ix].name.clone(), size(ix).into()),
            Target::Small { .. } => ("Smaller objects".into(), format_size(((s.end - s.start) as f64 * total) as u64).into()),
        })
        .collect()
}

/// Folder names from the root down to `ix` (excluding the root itself).
fn path_to(tree: &Tree, mut ix: usize) -> Vec<SharedString> {
    let mut names = Vec::new();
    while let Some(parent) = tree.nodes[ix].parent {
        names.push(tree.nodes[ix].name.clone());
        ix = parent;
    }
    names.reverse();
    names
}

struct Results {
    tree: Tree,
    focus: usize,
    segments: Rc<Vec<Segment>>,
    swatches: HashMap<usize, Hsla>,
    chart_hover: Option<Hit>,
    list_hover: Option<usize>,
    /// Each segment's identity (folder path), aligned with `segments`, for smooth motion.
    keys: Rc<Vec<motion::Key>>,
    /// Unique per layout, so the animation knows when to retarget.
    layout_id: u64,
    /// Each segment's name and size, aligned with `segments`, for the icicle's and treemap's labels.
    labels: Rc<Vec<(SharedString, SharedString)>>,
    /// The chart morphs between layouts (carried over from the scan's live chart); `tiles`
    /// does the same for the treemap.
    motion: Rc<std::cell::RefCell<motion::ChartMotion>>,
    tiles: Rc<std::cell::RefCell<TreemapMotion>>,
    chart: ChartType,
    chart_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    collector: Vec<usize>,
    list_scroll: UniformListScrollHandle,
    elapsed: Duration,
    /// What the user asked to scan; for the startup disk this is `/`, while the tree
    /// itself is rooted at the Data volume.
    requested_root: PathBuf,
    findings: Vec<Finding>,
    /// What deleting the Collector's items frees (worked out in the background with
    /// `scan::frees_of`; `None` while calculating). `collector_version` guards against a
    /// slow result for an older selection.
    collector_frees: Cell<Option<u64>>,
    collector_version: u64,
    /// Set when the scan has just finished: show the "scan complete" banner.
    banner: Option<Instant>,
    color_by: ColorBy,
    /// What each node is, for colouring by kind. Filled in as nodes appear in the chart;
    /// node indices stay valid for the life of the tree, so nothing is classified twice.
    categories: HashMap<usize, Category>,
    /// The kind under the pointer in the chart's legend: the chart highlights just that.
    legend_hover: Option<Category>,
    /// Folders that look like someone's home folder (`home_folders`).
    homes: Vec<usize>,
    id: u64,
    /// Follows changes on disk since the scan started (see `watch`).
    watch: Option<watch::Watch>,
    /// Changed folders are being read again in the background.
    refreshing: bool,
    /// macOS lost track of changes; only a rescan can be trusted.
    out_of_date: bool,
    /// Findings whose savings are being worked out right now, by title and folders.
    surveying: Vec<(&'static str, Vec<usize>)>,
    /// The Collector's items are being moved to the Trash.
    trashing: bool,
    /// The last move to the Trash, confirmed in the Collector until it changes again.
    trashed: Option<Trashed>,
}

struct Trashed {
    /// The item's name, or how many there were.
    what: String,
    frees: u64,
}

impl Results {
    fn new(tree: Tree, findings: Vec<Finding>, elapsed: Duration, requested_root: PathBuf, color_by: ColorBy, chart: ChartType) -> Self {
        let mut results = Self {
            tree,
            focus: Tree::ROOT,
            segments: Rc::default(),
            swatches: HashMap::new(),
            chart_hover: None,
            list_hover: None,
            keys: Rc::default(),
            layout_id: 0,
            labels: Rc::default(),
            motion: Rc::default(),
            tiles: Rc::default(),
            chart,
            chart_bounds: Rc::default(),
            collector: Vec::new(),
            list_scroll: UniformListScrollHandle::new(),
            elapsed,
            requested_root,
            findings,
            collector_frees: Cell::new(None),
            collector_version: 0,
            banner: None,
            color_by,
            categories: HashMap::new(),
            legend_hover: None,
            homes: Vec::new(),
            id: NEXT_RESULTS_ID.fetch_add(1, Ordering::Relaxed),
            watch: None,
            refreshing: false,
            out_of_date: false,
            surveying: Vec::new(),
            trashing: false,
            trashed: None,
        };
        results.homes = home_folders(&results.tree);
        results.relayout();
        results
    }

    /// Lay the chart out again, as deep as its type shows, with everything aligned with it.
    fn relayout(&mut self) {
        let segments = self.chart.layout(&self.tree, self.focus);
        if self.color_by == ColorBy::Kind {
            for s in &segments {
                if let Target::Node(ix) = s.target {
                    let (tree, homes) = (&self.tree, &self.homes);
                    self.categories.entry(ix).or_insert_with(|| category_of(tree, homes, ix));
                }
            }
        }
        self.swatches = segments
            .iter()
            .filter_map(|s| match s.target {
                Target::Node(ix) if s.depth == 1 => Some((ix, self.color(s))),
                _ => None,
            })
            .collect();
        self.keys = Rc::new(
            segments
                .iter()
                .map(|s| match s.target {
                    Target::Node(ix) => motion::Key::Node(path_to(&self.tree, ix)),
                    Target::Small { first, .. } => motion::Key::Small(path_to(&self.tree, first)),
                })
                .collect(),
        );
        let tree = &self.tree;
        self.labels = Rc::new(segment_labels(tree, self.focus, &segments, |ix| format_size(tree.nodes[ix].size)));
        self.segments = Rc::new(segments);
        self.layout_id = NEXT_LAYOUT_ID.fetch_add(1, Ordering::Relaxed);
    }

    fn set_color_by(&mut self, color_by: ColorBy) {
        self.color_by = color_by;
        self.legend_hover = None;
        self.relayout();
    }

    fn category(&self, target: Target) -> Option<Category> {
        match target {
            Target::Node(ix) => self.categories.get(&ix).copied(),
            Target::Small { .. } => None,
        }
    }

    fn color(&self, segment: &Segment) -> Hsla {
        match (self.color_by, self.category(segment.target)) {
            (ColorBy::Kind, Some(category)) => {
                let base = classify::color(category);
                // A touch lighter per ring, so nested folders of the same kind stay apart.
                let lightness = (base.lightness + 0.015 * (segment.depth - 1) as f32).min(0.9);
                gpui::hsla(base.hue.into_positive_degrees() / 360.0, base.saturation, lightness, 1.0)
            }
            _ => sunburst::base_color(segment),
        }
    }

    fn navigate(&mut self, ix: usize) {
        if self.tree.nodes[ix].kind != Kind::Dir || ix == self.focus {
            return;
        }
        match self.chart {
            ChartType::Treemap => {
                let (from, to) = (path_to(&self.tree, self.focus), path_to(&self.tree, ix));
                self.tiles.borrow_mut().zoom(motion::Key::Node(from), motion::Key::Node(to));
            }
            ChartType::Sunburst | ChartType::Icicle => self.motion.borrow_mut().zoom(motion::Camera::between(&self.tree, self.focus, ix)),
        }
        self.focus = ix;
        self.chart_hover = None;
        self.list_hover = None;
        self.relayout();
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn hovered(&self) -> Option<Target> {
        match self.chart_hover {
            Some(Hit::Segment(i)) => self.segments.get(i).map(|s| s.target),
            Some(Hit::Center) => None,
            None => self.list_hover.map(Target::Node),
        }
    }

    fn collect(&mut self, ix: usize) {
        // Removing anything inside an app breaks it (and its code signature).
        if self.trashing || ix == Tree::ROOT || findings::is_inside_bundle(&self.tree, ix) || self.tree.nodes[ix].parent.is_none() || self.tree.nodes[ix].kind == Kind::Other {
            return;
        }
        if self.collector.iter().any(|&c| self.tree.is_ancestor_or_self(c, ix)) {
            return;
        }
        let tree = &self.tree;
        self.collector.retain(|&c| !tree.is_ancestor_or_self(ix, c));
        self.collector.push(ix);
        self.collector_frees.set(None);
    }

    fn collected_size(&self) -> u64 {
        self.collector.iter().map(|&c| self.tree.nodes[c].size).sum()
    }

    /// `ix`'s path as the user knows it: when scanning the startup disk the tree is rooted
    /// at the Data volume, but `/Users/…` is what Finder and Terminal show.
    fn shown_path(&self, ix: usize) -> PathBuf {
        let path = self.tree.path_of(ix);
        match path.strip_prefix(&self.tree.root_path) {
            Ok(relative) if self.requested_root != self.tree.root_path => self.requested_root.join(relative),
            _ => path,
        }
    }

    fn collected_set(&self) -> HashSet<usize> {
        self.collector.iter().copied().collect()
    }

    /// Whether every folder of each finding is in the Collector (itself or inside a
    /// collected folder).
    fn findings_collected(&self) -> Vec<bool> {
        let set = self.collected_set();
        self.findings
            .iter()
            .map(|f| f.fix == Fix::Trash && !f.nodes.is_empty() && f.nodes.iter().all(|&n| is_covered(&self.tree, &set, n)))
            .collect()
    }

    /// After a move to the Trash: drop what went from the findings and refresh their sizes.
    /// `touched` (worked out before the items were detached) says which findings held or
    /// sat around a trashed item.
    fn update_findings(&mut self, trashed: &HashSet<usize>, touched: &[bool]) {
        let tree = &self.tree;
        let mut touched = touched.iter();
        self.findings.retain_mut(|f| {
            if !touched.next().copied().unwrap_or(false) {
                return true;
            }
            f.nodes.retain(|&n| !is_covered(tree, trashed, n));
            f.size = match f.fix {
                Fix::Trash => f.nodes.iter().map(|&n| tree.nodes[n].size).sum(),
                Fix::GitGc => f.nodes.iter().map(|&n| findings::loose_objects_size(tree, n)).sum(),
            };
            f.pending = false;
            f.size >= findings::MIN_SIZE
        });
        self.findings.sort_by(|a, b| b.size.cmp(&a.size));
    }



}

/// Whether `ix` is one of `set` or inside one of them.
fn is_covered(tree: &Tree, set: &HashSet<usize>, mut ix: usize) -> bool {
    loop {
        if set.contains(&ix) {
            return true;
        }
        match tree.nodes[ix].parent {
            Some(parent) => ix = parent,
            None => return false,
        }
    }
}

/// What a node is. The APFS volume slices are the system's; the unscanned or unreadable
/// remainder could be anything.
fn category_of(tree: &Tree, homes: &[usize], ix: usize) -> Category {
    let node = &tree.nodes[ix];
    let path = match node.kind {
        Kind::Other if [disk::NOT_SCANNED, disk::NOT_READABLE].contains(&node.name.as_ref()) => return Category::Mixed,
        Kind::Other => return Category::System,
        _ => {
            // Inside a home folder, describe the path from it (`~/…`) wherever the home
            // folder lives, as the classifier expects.
            let chain = tree.ancestry(ix);
            match chain.iter().rposition(|a| homes.contains(a)) {
                Some(home) => chain[home + 1..].iter().fold(PathBuf::from("~"), |path, &a| path.join(tree.nodes[a].name.as_ref())),
                None => tree.path_of(ix),
            }
        }
    };
    classify::classify(&path, node.kind == Kind::File)
}

/// Folders near the top of the tree that look like a home folder: a Library and at least
/// two of the usual Desktop, Documents, Downloads, Movies, Music, Pictures. That finds
/// the user's own (`/Users/<name>`) and also one scanned on a backup disk or a copy.
fn home_folders(tree: &Tree) -> Vec<usize> {
    const USUAL: [&str; 6] = ["Desktop", "Documents", "Downloads", "Movies", "Music", "Pictures"];
    let mut homes = Vec::new();
    let mut level = vec![Tree::ROOT];
    for _ in 0..4 {
        let mut next = Vec::new();
        for &ix in &level {
            let names: Vec<&str> = tree.nodes[ix].children.iter().map(|&c| tree.nodes[c].name.as_ref()).collect();
            if names.contains(&"Library") && USUAL.iter().filter(|u| names.contains(u)).count() >= 2 {
                homes.push(ix);
                continue;
            }
            next.extend(tree.nodes[ix].children.iter().copied().filter(|&c| tree.nodes[c].kind == Kind::Dir));
        }
        level = next;
    }
    homes
}

/// Shows paths in the home folder as `~/…`, as the shell does.
fn abbreviate_home(path: &str) -> String {
    let Ok(home) = std::env::var("HOME") else { return path.to_string() };
    match path.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// When the process started, for launch-to-first-chart timing (`PETAL_TIMING=1`).
pub static LAUNCHED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    /// Full Disk Access: nothing to show.
    Granted,
    /// Missing: show the card explaining how to grant it.
    Missing,
    /// Granted while Petal was running: suggest rescanning.
    JustGranted,
    /// The user said "Not now".
    Dismissed,
}

pub struct Petal {
    screen: Screen,
    focus_handle: FocusHandle,
    error: Option<String>,
    /// A short confirmation at the bottom of the window; `notice_version` lets a newer
    /// one outlive the timer of the one it replaced.
    notice: Option<String>,
    /// The notice is a warning (something wasn't done), not a confirmation.
    notice_warning: bool,
    notice_version: u64,
    access: Access,
    _access_watch: Option<Task<()>>,
    /// Kept here rather than on the results, so a rescan keeps the user's choice.
    color_by: ColorBy,
    chart: ChartType,
}

impl Petal {
    pub fn new(initial: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let access = if onboarding::has_full_disk_access() { Access::Granted } else { Access::Missing };
        // Recordings of a sample folder don't need the Full Disk Access card.
        #[cfg(feature = "snapshot")]
        let access = if std::env::var_os("PETAL_HIDE_ACCESS").is_some() { Access::Dismissed } else { access };
        let mut this = Self {
            screen: Screen::Start(scan::volumes()),
            focus_handle,
            error: None,
            notice: None,
            notice_warning: false,
            notice_version: 0,
            access,
            _access_watch: None,
            color_by: ColorBy::Folder,
            chart: ChartType::Sunburst,
        };
        if access == Access::Missing {
            this._access_watch = Some(this.watch_access(cx));
        }
        // Before the scan starts, so a recording's clock is in charge from the first folder.
        #[cfg(feature = "snapshot")]
        crate::snapshot::install(window, cx);
        if let Some(path) = initial {
            this.start_scan(path, cx);
        }
        this
    }

    /// Re-check every couple of seconds while access is missing, so the card can react as
    /// soon as the user flips the switch in System Settings.
    fn watch_access(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let granted = onboarding::has_full_disk_access();
                let keep_watching = this
                    .update(cx, |this, cx| {
                        if granted && this.access != Access::Granted {
                            this.access = Access::JustGranted;
                            cx.notify();
                        }
                        !granted
                    })
                    .unwrap_or(false);
                if !keep_watching {
                    break;
                }
            }
        })
    }

    fn render_access_card(&self, not_readable: Option<u64>, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (title, body) = match self.access {
            Access::Granted | Access::Dismissed => return None,
            Access::Missing => (
                "See everything on your disk",
                match not_readable {
                    Some(bytes) => format!(
                        "{} couldn't be read. macOS keeps some folders private (Mail, Messages, Safari, other apps' data) unless Petal has Full Disk Access.",
                        format_size(bytes)
                    ),
                    None => "macOS keeps some folders private (Mail, Messages, Safari, other apps' data). Give Petal Full Disk Access so the scan can include them.".to_string(),
                },
            ),
            Access::JustGranted => ("Full Disk Access is on", "Rescan to include the folders that were private before.".to_string()),
        };
        let actions = div().flex().gap_2().justify_end().mt_1();
        let actions = match self.access {
            Access::JustGranted => actions.child(
                primary_button("access-rescan", "Rescan", ACCENT).on_click(cx.listener(|this, _, window, cx| {
                    this.access = Access::Granted;
                    this.rescan(&Rescan, window, cx);
                })),
            ),
            _ => actions
                .child(button("access-later", "Not now").on_click(cx.listener(|this, _, _, cx| {
                    this.access = Access::Dismissed;
                    cx.notify();
                })))
                .child(
                    primary_button("access-open", "Open Privacy Settings", ACCENT)
                        .on_click(cx.listener(|_, _, _, cx| cx.open_url(onboarding::FULL_DISK_ACCESS_SETTINGS))),
                ),
        };
        Some(
            div()
                .mx_3()
                .mb_2()
                .p_3()
                .rounded_lg()
                .bg(rgb(CARD))
                .border_1()
                .border_color(rgb(ACCENT))
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_xs().text_color(rgb(MUTED)).child(body))
                .when(self.access == Access::Missing, |d| {
                    d.child(div().text_xs().text_color(rgb(MUTED)).child("In Settings, turn on Petal (use + to add it if it isn't listed)."))
                })
                .child(actions),
        )
    }

    fn start_scan(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.error = None;
        onboarding::mark_first_run_done();
        let progress = Arc::new(Progress::default());
        let started = clock::now();
        // Before walking anything, so changes made during the scan are caught up on after.
        let since = watch::current_event_id();

        let scan_task = cx.spawn({
            let progress = progress.clone();
            let root = root.clone();
            let root_for_results = root.clone();
            async move |this, cx| {
                let (tree, findings) = cx
                    .background_spawn({
                        let progress = progress.clone();
                        async move {
                            let tree = scan::scan(&root, &progress);
                            // May survey clone sharing (e.g. pnpm's cloned node_modules), so
                            // keep it off the UI thread.
                            let findings = findings::from_tree(&tree, &findings::Bases::for_root(&tree.root_path));
                            (tree, findings)
                        }
                    })
                    .await;
                this.update(cx, |this, cx| {
                    if !progress.cancelled.load(Ordering::Relaxed) {
                        // Carry the chart on from where it is.
                        let (motion, tiles) = match &this.screen {
                            Screen::Scanning(scanning) => (scanning.motion.clone(), scanning.tiles.clone()),
                            _ => Default::default(),
                        };
                        let mut results = Results::new(tree, findings, clock::since(started), root_for_results, this.color_by, this.chart);
                        // The results are sorted by size, unlike the scan's chart: fold one away and open the other.
                        match this.chart {
                            ChartType::Treemap => tiles.borrow_mut().fold(),
                            ChartType::Sunburst | ChartType::Icicle => motion.borrow_mut().fold(),
                        }
                        results.motion = motion;
                        results.tiles = tiles;
                        results.banner = Some(clock::now());
                        results.watch = watch::Watch::start(&results.requested_root, since);
                        let id = results.id;
                        this.screen = Screen::Results(results);
                        this.resolve_pending_findings(cx);
                        this.follow_changes(id, cx);
                        // A light tap (felt only with a finger on a Force Touch trackpad).
                        cx.play_haptic_feedback(HapticFeedbackStyle::LevelChange);
                        cx.spawn(async move |this, cx| {
                            let shown = clock::now();
                            while clock::since(shown) < BANNER_TIME {
                                cx.background_executor().timer(Duration::from_millis(250)).await;
                            }
                            this.update(cx, |this, cx| {
                                if let Some(r) = this.results() {
                                    r.banner = None;
                                    cx.notify();
                                }
                            })
                            .ok();
                        })
                        .detach();
                        cx.notify();
                    }
                })
                .ok();
            }
        });

        // Keep the counters fresh, and re-snapshot the live chart a few times a second.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                let scanning = this.update(cx, |this, cx| {
                    cx.notify();
                    let chart = this.chart;
                    let Screen::Scanning(scanning) = &mut this.screen else { return false };
                    let items = scanning.progress.files.load(Ordering::Relaxed) + scanning.progress.dirs.load(Ordering::Relaxed);
                    scanning.eta.update(clock::since(scanning.started).as_secs_f64(), items);
                    // (A change of chart takes its own snapshot straight away, in `set_chart`.)
                    if scanning.live.is_none() || clock::since(scanning.last_snapshot) >= LIVE_REFRESH {
                        scanning.snapshot(chart);
                    }
                    true
                });
                if !matches!(scanning, Ok(true)) {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(50)).await;
            }
        });

        self.screen = Screen::Scanning(Scanning {
            expected: if root == std::path::Path::new("/") { None } else { scan::volume_used(&root) },
            root,
            progress,
            started,
            live: None,
            last_snapshot: started,
            chart_bounds: Rc::default(),
            eta: eta::Eta::default(),
            motion: Rc::default(),
            tiles: Rc::default(),
            _tasks: [scan_task, ticker],
        });
        cx.notify();
    }

    fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        if let Screen::Scanning(scanning) = &self.screen {
            scanning.progress.cancelled.store(true, Ordering::Relaxed);
        }
        self.screen = Screen::Start(scan::volumes());
        cx.notify();
    }

    fn open_folder(&mut self, _: &OpenFolder, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Scan".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await {
                if let Some(path) = paths.pop() {
                    this.update(cx, |this, cx| this.start_scan(path, cx)).ok();
                }
            }
        })
        .detach();
    }

    fn rescan(&mut self, _: &Rescan, _: &mut Window, cx: &mut Context<Self>) {
        let root = match &self.screen {
            Screen::Results(r) => r.requested_root.clone(),
            Screen::Scanning(s) => s.root.clone(),
            Screen::Start(_) => return,
        };
        self.cancel_scan(cx);
        self.start_scan(root, cx);
    }

    fn start_over(&mut self, _: &StartOver, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_scan(cx);
    }

    fn go_up(&mut self, _: &GoUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Results(r) = &mut self.screen {
            if let Some(parent) = r.tree.nodes[r.focus].parent {
                r.navigate(parent);
                cx.notify();
            }
        }
    }

    /// Work out what the Collector frees, off the UI thread.
    /// Check for changes on disk every second while these results are up.
    fn follow_changes(&mut self, id: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_INTERVAL).await;
                let keep_going = this.update(cx, |this, cx| this.check_changes(id, cx)).unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }

    /// Read changed folders again in the background. Returns false once these results are gone.
    fn check_changes(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(r) = self.results() else { return false };
        if r.id != id {
            return false;
        }
        let Some(watch) = &r.watch else { return false };
        if r.refreshing {
            return true;
        }
        let pending = watch.take();
        if pending.lost && !r.out_of_date {
            r.out_of_date = true;
            cx.notify();
        }
        let mut folders: HashMap<usize, bool> = HashMap::new();
        for change in pending.changes {
            if let Some(ix) = r.tree.changed_folder(&change.path) {
                *folders.entry(ix).or_default() |= change.recursive;
            }
        }
        // Anything inside a folder that's being rescanned whole comes along with it.
        let whole: Vec<usize> = folders.iter().filter(|&(_, &recursive)| recursive).map(|(&ix, _)| ix).collect();
        folders.retain(|&ix, _| !whole.iter().any(|&w| w != ix && r.tree.is_ancestor_or_self(w, ix)));
        if folders.is_empty() {
            return true;
        }
        let tree = &r.tree;
        let stale: Vec<scan::Stale> = folders
            .into_iter()
            .map(|(ix, recursive)| scan::Stale {
                ix,
                path: tree.path_of(ix),
                recursive,
                known_dirs: tree.nodes[ix].children.iter().filter(|&&c| tree.nodes[c].kind == Kind::Dir).map(|&c| tree.nodes[c].name.to_string()).collect(),
            })
            .collect();
        let root = tree.root_path.clone();
        r.refreshing = true;
        cx.spawn(async move |this, cx| {
            let (fresh, errors) = cx
                .background_spawn(async move {
                    let (count, whole, started) = (stale.len(), stale.iter().filter(|s| s.recursive).count(), Instant::now());
                    let errors = std::sync::atomic::AtomicU64::new(0);
                    let fresh = scan::read_changes(&root, stale, &errors);
                    if std::env::var_os("PETAL_TIMING").is_some() {
                        eprintln!("changes: read {count} folders ({whole} whole) in {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
                    }
                    (fresh, errors.into_inner())
                })
                .await;
            this.update(cx, |this, cx| this.apply_fresh(id, fresh, errors, cx)).ok();
        })
        .detach();
        true
    }

    /// Fold re-read folders into the tree, and keep the focus, Collector and findings in step.
    fn apply_fresh(&mut self, id: u64, fresh: Vec<(usize, scan::Fresh)>, errors: u64, cx: &mut Context<Self>) {
        let Some(r) = self.results() else { return };
        if r.id != id {
            return;
        }
        r.refreshing = false;
        let touched: Vec<usize> = fresh.iter().map(|(ix, _)| *ix).collect();
        let focus_path = path_to(&r.tree, r.focus);
        if scan::apply_changes(&mut r.tree, fresh) == 0 {
            return;
        }
        r.tree.errors += errors;
        if r.requested_root == std::path::Path::new("/") {
            scan::refresh_volume_slices(&mut r.tree);
        }
        // A folder that's gone (or renamed) leaves the view at its nearest surviving parent.
        r.focus = resolve_path(&r.tree, &focus_path);
        r.chart_hover = None;
        r.list_hover = r.list_hover.filter(|&ix| r.tree.is_attached(ix));

        let inside = |tree: &Tree, folder: usize| touched.iter().any(|&t| tree.is_ancestor_or_self(folder, t));
        let collected = r.collector.len();
        r.collector.retain(|&c| r.tree.is_attached(c));
        let collector_changed = r.collector.len() != collected || r.collector.iter().any(|&c| inside(&r.tree, c));
        let findings_changed = r.findings.iter().any(|f| f.nodes.iter().any(|&n| !r.tree.is_attached(n) || inside(&r.tree, n)));
        if findings_changed {
            let mut fresh = findings::from_tree(&r.tree, &findings::Bases::for_root(&r.tree.root_path));
            // Working out savings means walking the folders again (seconds, for caches), so
            // keep a finding's worked-out savings while its size has barely moved; a real
            // change (say, the Trash emptied) works them out again.
            for finding in &mut fresh {
                let same = r.findings.iter().find(|old| !old.pending && old.title == finding.title && old.nodes == finding.nodes);
                if let Some(old) = same {
                    let drift = old.allocated.abs_diff(finding.allocated);
                    if drift <= (old.allocated / 100).max(64 << 20) {
                        finding.size = old.size;
                        finding.allocated = old.allocated;
                        finding.pending = false;
                    }
                }
            }
            fresh.sort_by(|a, b| b.size.cmp(&a.size));
            r.findings = fresh;
        }
        r.relayout();
        if findings_changed {
            self.resolve_pending_findings(cx);
        }
        if collector_changed {
            self.collector_changed(cx);
        }
        cx.notify();
    }

    fn collector_changed(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.results() else { return };
        r.trashed = None;
        r.collector_version += 1;
        r.collector_frees.set(None);
        let version = r.collector_version;
        let paths: Vec<PathBuf> = r.collector.iter().map(|&ix| r.tree.path_of(ix)).collect();
        if paths.is_empty() {
            r.collector_frees.set(Some(0));
            return;
        }
        cx.spawn(async move |this, cx| {
            let frees = cx.background_spawn(async move { scan::frees_of(&paths) }).await;
            this.update(cx, |this, cx| {
                if let Some(r) = this.results() {
                    if r.collector_version == version {
                        r.collector_frees.set(Some(frees));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Findings whose real saving needs a clone survey (e.g. node_modules).
    fn resolve_pending_findings(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.results() else { return };
        let mut started = Vec::new();
        for finding in r.findings.iter().filter(|f| f.pending) {
            // Matched by what it is, not its position: changes on disk can rebuild the list
            // while this is still working.
            let key = (finding.title, finding.nodes.clone());
            if r.surveying.contains(&key) || started.contains(&key) {
                continue;
            }
            started.push(key.clone());
            let (title, nodes) = key.clone();
            let paths: Vec<PathBuf> = finding.nodes.iter().map(|&ix| r.tree.path_of(ix)).collect();
            cx.spawn(async move |this, cx| {
                let frees = cx.background_spawn(async move { scan::frees_of(&paths) }).await;
                this.update(cx, |this, cx| {
                    if let Some(r) = this.results() {
                        r.surveying.retain(|k| *k != key);
                        if let Some(finding) = r.findings.iter_mut().find(|f| f.pending && f.title == title && f.nodes == nodes) {
                            finding.size = frees;
                            finding.pending = false;
                        }
                        r.findings.sort_by(|a, b| b.size.cmp(&a.size));
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        r.surveying.extend(started);
    }

    /// For the recorder: has the scan finished?
    #[cfg(feature = "snapshot")]
    pub fn showing_results(&self) -> bool {
        matches!(self.screen, Screen::Results(_))
    }

    fn results(&mut self) -> Option<&mut Results> {
        match &mut self.screen {
            Screen::Results(r) => Some(r),
            _ => None,
        }
    }

    fn set_color_by(&mut self, color_by: ColorBy, cx: &mut Context<Self>) {
        self.color_by = color_by;
        if let Some(r) = self.results() {
            r.set_color_by(color_by);
        }
        cx.notify();
    }

    fn set_chart(&mut self, chart: ChartType, cx: &mut Context<Self>) {
        if self.chart == chart {
            return;
        }
        self.chart = chart;
        cx.set_menus(crate::menus(chart));
        // Lay it out again straight away (the treemap is one level deep, the others several),
        // and start the chart afresh, so it's revealed the way a new chart is rather than
        // morphing from a different shape.
        let (motion, tiles) = match &mut self.screen {
            Screen::Scanning(scanning) => {
                scanning.snapshot(chart);
                (&scanning.motion, &scanning.tiles)
            }
            Screen::Results(r) => {
                r.chart = chart;
                r.chart_hover = None;
                r.relayout();
                (&r.motion, &r.tiles)
            }
            Screen::Start(_) => return cx.notify(),
        };
        *motion.borrow_mut() = Default::default();
        *tiles.borrow_mut() = Default::default();
        cx.notify();
    }

    fn set_chart_hover(&mut self, hit: Option<Hit>, cx: &mut Context<Self>) {
        if let Some(r) = self.results() {
            if r.chart_hover != hit {
                r.chart_hover = hit;
                cx.notify();
            }
        }
    }

    fn chart_click(&mut self, hit: Hit, cx: &mut Context<Self>) {
        let Some(r) = self.results() else { return };
        match hit {
            Hit::Center => {
                if let Some(parent) = r.tree.nodes[r.focus].parent {
                    r.navigate(parent);
                }
            }
            Hit::Segment(i) => {
                if let Some(Target::Node(ix)) = r.segments.get(i).map(|s| s.target) {
                    r.navigate(ix);
                }
            }
        }
        cx.notify();
    }

    fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice_warning = false;
        self.notice = Some(text);
        self.notice_version += 1;
        let version = self.notice_version;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_TIME).await;
            this.update(cx, |this, cx| {
                if this.notice_version == version {
                    this.notice = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn show_warning(&mut self, text: String, cx: &mut Context<Self>) {
        self.show_notice(text, cx);
        self.notice_warning = true;
    }

    /// Add `nodes` to the Collector, leaving out (and saying so) anything inside an app.
    fn collect_items(&mut self, nodes: &[usize], cx: &mut Context<Self>) {
        let Some(r) = self.results() else { return };
        let mut refused = 0;
        for &ix in nodes {
            if findings::is_inside_bundle(&r.tree, ix) {
                refused += 1;
            } else {
                r.collect(ix);
            }
        }
        self.collector_changed(cx);
        if refused > 0 {
            self.show_warning("That's part of an app: deleting it would break the app. Trash the whole app instead.".into(), cx);
        }
        cx.notify();
    }

    fn copy_to_clipboard(&mut self, text: String, notice: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_notice(notice, cx);
    }

    fn trash_collected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.results() else { return };
        if r.collector.is_empty() {
            return;
        }
        let items = r.collector.clone();
        let paths: Vec<PathBuf> = items.iter().map(|&ix| r.tree.path_of(ix)).collect();
        let message = if items.len() == 1 {
            format!("Move “{}” to the Trash?", r.tree.nodes[items[0]].name)
        } else {
            format!("Move {} items to the Trash?", items.len())
        };
        // Quote what deleting really frees (clones and hard links shared with files outside
        // the selection stay), once it's known.
        let detail = match r.collector_frees.get() {
            Some(frees) => format!("This will free up {}. You can put items back from the Trash in Finder.", format_size(frees)),
            None => format!(
                "This will free up to {} (still working out how much is shared). You can put items back from the Trash in Finder.",
                format_size(r.collected_size())
            ),
        };
        let what = if items.len() == 1 {
            format!("“{}”", r.tree.nodes[items[0]].name)
        } else {
            format!("{} items", format_count(items.len() as u64))
        };
        let frees = r.collector_frees.get().unwrap_or_else(|| r.collected_size());
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &["Move to Trash", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let started = this.update(cx, |this, cx| {
                if let Some(r) = this.results() {
                    r.trashing = true;
                    cx.notify();
                }
            });
            if started.is_err() {
                return;
            }
            let result = cx
                .background_spawn(async move { trashing::move_to_trash(&paths) })
                .await;
            this.update(cx, |this, cx| {
                if let Some(r) = this.results() {
                    r.trashing = false;
                }
                match result {
                    Ok(()) => {
                        if let Some(r) = this.results() {
                            let set: HashSet<usize> = items.iter().copied().collect();
                            // Before detaching: afterwards an item no longer knows its ancestors.
                            let mut around = HashSet::new();
                            for &t in &items {
                                let mut at = r.tree.nodes[t].parent;
                                while let Some(p) = at.filter(|&p| around.insert(p)) {
                                    at = r.tree.nodes[p].parent;
                                }
                            }
                            let touched: Vec<bool> = r
                                .findings
                                .iter()
                                .map(|f| f.nodes.iter().any(|&n| around.contains(&n) || is_covered(&r.tree, &set, n)))
                                .collect();
                            for &ix in &items {
                                if r.tree.is_ancestor_or_self(ix, r.focus) {
                                    r.focus = r.tree.nodes[ix].parent.unwrap_or(Tree::ROOT);
                                }
                                r.tree.remove(ix);
                            }
                            r.update_findings(&set, &touched);
                            r.collector.clear();
                            r.collector_frees.set(None);
                            r.trashed = Some(Trashed { what, frees });
                            r.chart_hover = None;
                            r.list_hover = None;
                            r.relayout();
                        }
                        cx.play_haptic_feedback(HapticFeedbackStyle::LevelChange);
                    }
                    Err(error) => this.error = Some(format!("Couldn’t move to Trash: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for Petal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.screen {
            Screen::Start(volumes) => self.render_start(volumes, cx).into_any_element(),
            Screen::Scanning(scanning) => self.render_scanning(scanning, cx).into_any_element(),
            Screen::Results(_) => self.render_results(window, cx).into_any_element(),
        };

        div()
            .id("petal")
            .key_context("Petal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::rescan))
            .on_action(cx.listener(Self::start_over))
            .on_action(cx.listener(Self::go_up))
            .on_action(cx.listener(|this, _: &ShowSunburst, _, cx| this.set_chart(ChartType::Sunburst, cx)))
            .on_action(cx.listener(|this, _: &ShowIcicle, _, cx| this.set_chart(ChartType::Icicle, cx)))
            .on_action(cx.listener(|this, _: &ShowTreemap, _, cx| this.set_chart(ChartType::Treemap, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(".SystemUIFont")
            .text_sm()
            .child(self.render_toolbar(window, cx))
            .child(div().flex_1().min_h_0().flex().child(content))
            .when_some(self.notice.clone().filter(|_| self.error.is_none()), |el, notice| {
                el.child(
                    div().absolute().bottom_4().left_0().right_0().flex().justify_center().child(
                        div()
                            .px_4()
                            .py_2()
                            .rounded_full()
                            .bg(rgb(CARD))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .shadow_lg()
                            .flex()
                            .gap_2()
                            .child(if self.notice_warning {
                                div().text_color(rgb(WARNING)).font_weight(FontWeight::BOLD).child("!")
                            } else {
                                div().text_color(rgb(SAFE)).font_weight(FontWeight::BOLD).child("✓")
                            })
                            .child(notice),
                    ),
                )
            })
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .id("error")
                        .absolute()
                        .bottom_4()
                        .right_4()
                        .max_w(px(420.))
                        .px_4()
                        .py_2()
                        .rounded_lg()
                        .bg(rgb(DANGER))
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .child(error)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        })),
                )
            })
    }
}

fn button_base(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .cursor_pointer()
        .active(|s| s.opacity(0.8))
        // Keep clicks from starting a window drag in the toolbar.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.into())
}

fn button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>) -> Stateful<gpui::Div> {
    button_base(id, label)
        .bg(rgb(CARD))
        .border_color(rgb(BORDER))
        .hover(|s| s.bg(rgb(CARD_HOVER)))
}

fn primary_button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, color: u32) -> Stateful<gpui::Div> {
    button_base(id, label)
        .bg(rgb(color))
        .border_color(rgb(color))
        .text_color(rgb(0xffffff))
        .font_weight(FontWeight::MEDIUM)
        .hover(move |s| s.bg(lighten(rgb(color))))
}

fn lighten(color: Rgba) -> Hsla {
    let mut hsla: Hsla = color.into_color();
    hsla.lightness = (hsla.lightness + 0.07).min(1.0);
    hsla
}

fn to_hsla(color: u32) -> Hsla {
    rgb(color).into_color()
}

impl Petal {
    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // In a narrow window the toggles drop their captions, so the breadcrumbs keep room.
        let captions = window.viewport_size().width >= px(1100.);
        let mut bar = div()
            .id("toolbar")
            .h(px(46.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .pl(px(84.))
            .pr_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
            // The window owns its titlebar drag (`app_owns_titlebar_drag`), so this is the
            // only handler: buttons stop the mouse-down, and a double-click on the bar itself
            // does what System Settings says (zoom, minimize or nothing).
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            });

        match &self.screen {
            Screen::Results(r) => {
                let can_go_up = r.tree.nodes[r.focus].parent.is_some();
                // At the top there's nowhere to go: no hover, no pointer.
                let up = if can_go_up {
                    button("up", "‹")
                } else {
                    button_base("up", "‹").bg(rgb(CARD)).border_color(rgb(BORDER)).opacity(0.4).cursor_default()
                };
                bar = bar.child(
                    up.text_base()
                        .px_2()
                        .tooltip(tooltip("Enclosing Folder (⌘↑)"))
                        .on_click(cx.listener(|this, _, window, cx| this.go_up(&GoUp, window, cx))),
                );
                let chain = r.tree.ancestry(r.focus);
                let mut crumbs = div().flex().items_center().gap_1().min_w_0().overflow_hidden();
                for (i, &ix) in chain.iter().enumerate() {
                    let is_last = i + 1 == chain.len();
                    if i > 0 {
                        crumbs = crumbs.child(div().text_color(rgb(MUTED)).child("›"));
                    }
                    crumbs = crumbs.child(
                        div()
                            .id(("crumb", ix))
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .min_w(px(24.))
                            .truncate()
                            // Short of room, the folders above give way before the one you're in.
                            .when(is_last, |d| d.font_weight(FontWeight::SEMIBOLD).flex_shrink_0())
                            .when(!is_last, |d| d.text_color(rgb(MUTED)).flex_shrink(1.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(CARD_HOVER)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.results() {
                                    r.navigate(ix);
                                    cx.notify();
                                }
                            }))
                            .child(r.tree.nodes[ix].name.clone()),
                    );
                }
                bar = bar.child(crumbs).child(div().flex_1());
                bar = bar
                    .child(chart_toggle(self.chart, captions, cx))
                    .child(color_toggle(r.color_by, captions, cx))
                    .child(
                        div().text_xs().text_color(rgb(MUTED)).min_w(px(0.)).flex_shrink(1.).truncate().child(if r.out_of_date {
                            "Out of date · rescan to refresh".to_string()
                        } else {
                            format!("Scanned in {:.1}s", r.elapsed.as_secs_f32())
                        }),
                    )
                    .child(
                        button("rescan", "Rescan")
                            .tooltip(tooltip("Scan this folder again (⌘R)"))
                            .on_click(cx.listener(|this, _, window, cx| this.rescan(&Rescan, window, cx))),
                    )
                    .child(
                        button("start-over", "Disks")
                            .tooltip(tooltip("Back to the list of disks (⇧⌘D)"))
                            .on_click(cx.listener(|this, _, window, cx| this.start_over(&StartOver, window, cx))),
                    );
            }
            Screen::Scanning(_) => {
                bar = bar.child(div().font_weight(FontWeight::SEMIBOLD).child("Petal")).child(div().flex_1()).child(chart_toggle(self.chart, captions, cx));
            }
            Screen::Start(_) => {
                bar = bar.child(div().font_weight(FontWeight::SEMIBOLD).child("Petal"));
            }
        }
        bar
    }

    fn render_start(&self, volumes: &[Volume], cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_3().w(px(560.));
        for (i, volume) in volumes.iter().enumerate() {
            let used = volume.total.saturating_sub(volume.free);
            let fraction = used as f32 / volume.total.max(1) as f32;
            let path = volume.path.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .p_4()
                    .rounded_xl()
                    .bg(rgb(CARD))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(disk_gauge(fraction))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).truncate().child(volume.name.clone()))
                            .child(div().text_color(rgb(MUTED)).child(format!(
                                "{} used · {} free of {}",
                                format_size(used),
                                format_size(volume.free),
                                format_size(volume.total)
                            )))
                            .child(meter(fraction)),
                    )
                    .child(
                        primary_button(("scan", i), "Scan", ACCENT)
                            .on_click(cx.listener(move |this, _, _, cx| this.start_scan(path.clone(), cx))),
                    ),
            );
        }

        let home = std::env::var_os("HOME").map(PathBuf::from);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(div().text_3xl().font_weight(FontWeight::BOLD).child("Petal"))
                    .child(div().text_color(rgb(MUTED)).child("Find out what’s taking up space on your disk.")),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .gap_3()
                    .when_some(home, |el, home| {
                        el.child(
                            button("scan-home", "Scan Home Folder")
                                .on_click(cx.listener(move |this, _, _, cx| this.start_scan(home.clone(), cx))),
                        )
                    })
                    .child(
                        button("choose", "Choose Folder…  ⌘O")
                            .on_click(cx.listener(|this, _, window, cx| this.open_folder(&OpenFolder, window, cx))),
                    ),
            )
            .when(self.access != Access::Granted, |d| {
                d.child(
                    div()
                        .max_w(px(560.))
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .text_center()
                        .child("Tip: grant Full Disk Access in System Settings › Privacy & Security to include protected folders."),
                )
            })
    }

    fn render_scanning(&self, scanning: &Scanning, cx: &mut Context<Self>) -> impl IntoElement {
        let progress = &scanning.progress;
        let current = progress.current.lock().map(|c| abbreviate_home(&c)).unwrap_or_default();
        let scanned = progress.bytes.load(Ordering::Relaxed);
        let files = progress.files.load(Ordering::Relaxed);
        let elapsed = clock::since(scanning.started).as_secs_f32();
        let layout = progress.layout.get().cloned();
        let disk_name = layout.as_ref().map(|l| l.name.clone()).unwrap_or_else(|| scan::display_name(&scanning.root));
        let view = scanning.live.clone();
        if view.is_some() && std::env::var_os("PETAL_TIMING").is_some() {
            static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !REPORTED.swap(true, Ordering::Relaxed) {
                if let Some(launched) = LAUNCHED.get() {
                    eprintln!("timing: first live chart {:.0} ms after launch", launched.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }

        // Honest sizes: final folders show their size, folders still being counted show a lower bound.
        let size_label = |view: &LiveView, ix: usize| -> String {
            let node = &view.tree.nodes[ix];
            if node.kind == Kind::Other || view.is_final(ix) {
                format_size(node.size)
            } else if node.size == 0 {
                "counting…".to_string()
            } else {
                format!("≥ {}", format_size(node.size))
            }
        };

        // Sidebar: what's in the folder being scanned.
        let mut rows = div().id("live-rows").flex_1().min_h_0().px_2().overflow_y_scroll().flex().flex_col();
        if let Some(view) = &view {
            for &ix in &view.tree.nodes[Tree::ROOT].children {
                let node = &view.tree.nodes[ix];
                let swatch = view.swatches.get(&ix).copied().unwrap_or(gpui::hsla(0., 0., 0.38, 1.));
                let is_final = view.is_final(ix);
                rows = rows.child(
                    div()
                        .id(("live-row", ix))
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded_md()
                        .child(
                            div()
                                .size(px(10.))
                                .flex_none()
                                .rounded_full()
                                .bg(if is_final || node.kind == Kind::Other { swatch } else { sunburst::dim(swatch) }),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(node.name.clone()))
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(if is_final { rgb(TEXT) } else { rgb(MUTED) })
                                .child(size_label(view, ix)),
                        ),
                );
            }
        }

        let sidebar = div()
            .w(px(360.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).truncate().child(format!("Scanning “{disk_name}”…")))
                    .child(div().text_color(rgb(MUTED)).child(format!("{} · {} files", format_size(scanned), format_count(files))))
                    .child(match progress.expected_items.get() {
                        Some(&expected) => {
                            let items = files + progress.dirs.load(Ordering::Relaxed);
                            let done = eta::fraction(items, expected);
                            let left = scanning.eta.remaining(elapsed as f64, items, expected).map(eta::describe);
                            div()
                                .mt_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .h(px(6.))
                                        .w_full()
                                        .rounded_full()
                                        .bg(rgb(BORDER))
                                        .child(div().h_full().w(relative(done)).rounded_full().bg(rgb(ACCENT))),
                                )
                                .child(div().text_xs().text_color(rgb(MUTED)).child(match left {
                                    Some(left) => format!("{:.0}% · {left}", done * 100.0),
                                    None => format!("{:.0}%", done * 100.0),
                                }))
                                .into_any_element()
                        }
                        // A folder: no known total, so no percentage or countdown.
                        None => div().text_xs().text_color(rgb(MUTED)).child("Scanning…").into_any_element(),
                    })
                    .child(
                        div()
                            .h(px(16.))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .text_ellipsis_start()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(current),
                    ),
            )
            .children(self.render_access_card(None, cx))
            .when_some(
                Some(findings::early_findings(&progress.early_findings.lock().unwrap())).filter(|f| !f.is_empty()),
                |d, early| d.child(self.render_findings(&early, false, &[], cx)),
            )
            .child(rows)
            .child(
                div()
                    .p_3()
                    .flex()
                    .justify_end()
                    .items_center()
                    .child(button("cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_scan(cx)))),
            );

        // Chart: share of the volume found so far for plain volume scans (the startup disk's
        // remainder is its own slice instead).
        let fraction = match scanning.expected {
            Some(expected) if expected > 0 => (scanned as f64 / expected as f64).min(1.0) as f32,
            _ => 1.0,
        };
        let started = scanning.started;
        let motion = scanning.motion.clone();
        let bounds_cell = scanning.chart_bounds.clone();

        // The chart's label (centred on the sunburst, a strip above the icicle and treemap): how
        // far the scan has got.
        let (title, lines): (String, Vec<String>) = match &layout {
                Some(layout) => (
                    layout.name.clone(),
                    vec![
                        format!("{} used", format_size(layout.container_used)),
                        format!("{} of {} scanned", format_size(scanned), format_size(layout.data_used)),
                    ],
                ),
                None => (
                    format_size(view.as_ref().map(|v| v.tree.nodes[Tree::ROOT].size).unwrap_or(0).max(scanned)),
                    vec![match scanning.expected {
                        Some(expected) => format!("of {} used", format_size(expected)),
                        None => "scanning…".to_string(),
                    }],
                ),
        };
        let label_width = scanning.chart_bounds.get().map(|b| Geometry::new(b).inner_radius * 1.7).unwrap_or(140.);
        let chart_type = self.chart;
        let tiles = scanning.tiles.clone();
        let paint_view = view.clone();
        // The treemap's focus bar names what's being scanned.
        let focus = view.as_ref().map(|v| (v.tree.nodes[Tree::ROOT].name.clone(), SharedString::from(size_label(v, Tree::ROOT))));

        let chart = div()
            .flex_1()
            .h_full()
            .relative()
            .child(
                canvas(
                    move |bounds, _, _| bounds_cell.set(Some(bounds)),
                    move |bounds, _, window, cx| {
                        window.request_animation_frame();
                        let Some(view) = paint_view else {
                            paint_backdrop(chart_type, bounds, window);
                            return;
                        };
                        let pulse = gpui::hsla(0.0, 0.0, 0.22 + 0.04 * (clock::since(started).as_secs_f32() * 3.0).sin(), 1.0);
                        let layout = ChartLayout { id: view.id, keys: &view.keys, segments: &view.segments, labels: &view.labels };
                        let motions = ChartMotions { rings: &motion, tiles: &tiles };
                        paint_chart(chart_type, bounds, &layout, fraction, Some(pulse), to_hsla(0x2b2d33), focus, motions, window, cx, |i, start, end| {
                            // Hue follows the animated position, so colours shift smoothly too.
                            let segment = &Segment { start, end, ..view.segments[i] };
                            match segment.target {
                                Target::Node(ix) if Some(ix) == view.pending => pulse,
                                Target::Node(ix) => {
                                    let full = sunburst::base_color(segment);
                                    // Folders still being counted are drawn muted, and ease into full
                                    // colour once their total is final.
                                    if view.tree.nodes[ix].kind == Kind::Dir {
                                        let settled = match view.settled_at.get(&ix) {
                                            _ if !view.is_final(ix) => 0.0,
                                            Some(at) => {
                                                let t = (clock::since(*at).as_secs_f32() / SETTLE_FADE.as_secs_f32()).min(1.0);
                                                t * t * (3.0 - 2.0 * t)
                                            }
                                            None => 1.0,
                                        };
                                        sunburst::mix(sunburst::dim(full), full, settled)
                                    } else {
                                        full
                                    }
                                }
                                Target::Small { .. } => sunburst::base_color(segment),
                            }
                        });
                    },
                )
                .size_full(),
            )
            .child(chart_label(chart_type, label_width, title.into(), lines, None));

        div().size_full().flex().child(sidebar).child(chart)
    }

    fn render_results(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Screen::Results(r) = &self.screen else { unreachable!() };
        div()
            .size_full()
            .flex()
            .child(self.render_sidebar(r, cx))
            .child(self.render_chart(r, cx))
    }

    fn render_sidebar(&self, r: &Results, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = &r.tree.nodes[r.focus];
        let count = focus.children.len();
        div()
            .w(px(360.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).truncate().child(focus.name.clone()))
                    .child(div().text_color(rgb(MUTED)).child(format!(
                        "{} · {} files",
                        format_size(focus.size),
                        format_count(focus.items)
                    )))
                    // Counts for the whole scan, so only at its top, in one line; the
                    // tooltips explain them.
                    .when(r.focus == Tree::ROOT && (r.tree.cloud_only > 0 || r.tree.errors > 0), |d| {
                        let unreadable_reason = if self.access == Access::Missing {
                            "Private to macOS until Petal has Full Disk Access"
                        } else {
                            "Protected by macOS or owned by another user; not even Full Disk Access opens them"
                        };
                        d.child(
                            div()
                                .flex()
                                .gap_1()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .when(r.tree.cloud_only > 0, |d| {
                                    d.child(
                                        div()
                                            .id("cloud-only")
                                            .tooltip(tooltip("In iCloud and not downloaded, so they use no space on this Mac"))
                                            .child(format!("{} cloud-only", format_count(r.tree.cloud_only))),
                                    )
                                })
                                .when(r.tree.cloud_only > 0 && r.tree.errors > 0, |d| d.child("·"))
                                .when(r.tree.errors > 0, |d| {
                                    d.child(
                                        div()
                                            .id("unreadable")
                                            .tooltip(tooltip(unreadable_reason))
                                            .child(format!("{} unreadable", format_count(r.tree.errors))),
                                    )
                                }),
                        )
                    }),
            )
            .children((r.focus == Tree::ROOT).then(|| {
                let not_readable = r.tree.nodes[Tree::ROOT]
                    .children
                    .iter()
                    .find(|&&c| r.tree.nodes[c].kind == Kind::Other && r.tree.nodes[c].name.as_ref() == disk::NOT_READABLE)
                    .map(|&c| r.tree.nodes[c].size);
                self.render_access_card(not_readable, cx)
            }).flatten())
            .when(r.focus == Tree::ROOT && !r.findings.is_empty(), |d| {
                d.child(self.render_findings(&r.findings, true, &r.findings_collected(), cx))
            })
            .child(
                div().flex_1().min_h_0().px_2().child(
                    uniform_list("children", count, cx.processor(Self::render_rows))
                        .track_scroll(&r.list_scroll)
                        .size_full(),
                ),
            )
            .child(self.render_collector(r, cx))
    }

    /// Known space hogs with exact sizes. `interactive` (results only): click to open the
    /// folder, + to collect it. `collected[i]`: finding `i` is already in the Collector.
    fn render_findings(
        &self,
        findings: &[Finding],
        interactive: bool,
        collected: &[bool],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let safe: u64 = findings.iter().filter(|f| f.safety == Safety::Safe).map(|f| f.size).sum();
        let mut list = div().id("findings").max_h(px(250.)).overflow_y_scroll().flex().flex_col().gap_0p5();
        for (i, finding) in findings.iter().enumerate() {
            let (tag, color) = match finding.safety {
                Safety::Safe => ("Safe to delete", SAFE),
                Safety::Review => ("Review first", WARNING),
            };
            let target = finding.nodes.first().copied().filter(|_| finding.path.is_some());
            let nodes = finding.nodes.clone();
            let uncollect = finding.nodes.clone();
            let in_collector = collected.get(i).copied().unwrap_or(false);
            let fix = finding.fix;
            list = list.child(
                div()
                    .id(("finding", i))
                    .group("finding")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .flex()
                    .flex_col()
                    .when(in_collector, |d| d.bg(rgba(ACCENT_TINT)))
                    .when(interactive && target.is_some(), |d| {
                        d.cursor_pointer().hover(|s| s.bg(rgb(CARD_HOVER))).on_click(cx.listener(move |this, _, _, cx| {
                            if let (Some(r), Some(ix)) = (this.results(), target) {
                                r.navigate(ix);
                                cx.notify();
                            }
                        }))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(8.)).flex_none().rounded_full().bg(rgb(color)))
                            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(finding.title))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(64.))
                                    .text_right()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .when(finding.pending, |d| d.text_color(rgb(MUTED)))
                                    .child(if finding.pending { "…".to_string() } else { format_size(finding.size) }),
                            )
                            .when(interactive && fix == Fix::GitGc, |d| {
                                let git_dirs = nodes.clone();
                                d.child(
                                    icon_button(("copy-gc", i), "⧉", "Copy git gc Commands")
                                        .invisible()
                                        .group_hover("finding", |s| s.visible())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            let Some(r) = this.results() else { return };
                                            let paths: Vec<PathBuf> = git_dirs.iter().map(|&ix| r.shown_path(ix)).collect();
                                            let notice = match paths.len() {
                                                1 => "Copied the git gc command: paste it in Terminal".to_string(),
                                                n => format!("Copied git gc commands for {n} repositories: paste them in Terminal"),
                                            };
                                            this.copy_to_clipboard(findings::git_gc_commands(&paths), notice, cx);
                                        })),
                                )
                            })
                            .when(interactive && fix == Fix::Trash && !in_collector, |d| {
                                d.child(
                                    icon_button(("collect-finding", i), "+", "Add to Collector")
                                        .invisible()
                                        .group_hover("finding", |s| s.visible())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.collect_items(&nodes, cx);
                                        })),
                                )
                            })
                            .when(interactive && in_collector, |d| {
                                d.child(
                                    icon_button(("uncollect-finding", i), "✓", "Remove from Collector")
                                        .text_color(rgb(ACCENT))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            if let Some(r) = this.results().filter(|r| !r.trashing) {
                                                r.collector.retain(|c| !uncollect.contains(c));
                                            }
                                            this.collector_changed(cx);
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .pl(px(16.))
                            .flex()
                            .gap_1()
                            .text_xs()
                            .child(div().flex_none().text_color(rgb(color)).child(tag))
                            .child(div().flex_none().text_color(rgb(MUTED)).child("·"))
                            .child(if in_collector {
                                div().min_w_0().text_color(rgb(ACCENT)).truncate().child("In Collector")
                            } else {
                                div().min_w_0().text_color(rgb(MUTED)).truncate().child(finding.blurb.clone())
                            }),
                    ),
            );
        }
        div()
            .mx_3()
            .mb_2()
            .p_2()
            .rounded_lg()
            .bg(rgb(BG))
            .border_1()
            .border_color(rgb(BORDER))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Findings"))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(if findings.iter().any(|f| f.pending) {
                        "working out savings…".to_string()
                    } else if safe > 0 {
                        format!("{} safe to delete", format_size(safe))
                    } else {
                        String::new()
                    })),
            )
            .child(list)
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<Stateful<gpui::Div>> {
        let Screen::Results(r) = &self.screen else { return Vec::new() };
        let focus = &r.tree.nodes[r.focus];
        let hovered = r.hovered();
        let collected = r.collected_set();
        range
            .filter_map(|i| focus.children.get(i).copied())
            .map(|ix| {
                let node = &r.tree.nodes[ix];
                let is_hovered = hovered == Some(Target::Node(ix));
                let swatch = r.swatches.get(&ix).copied().unwrap_or(gpui::hsla(0., 0., 0.38, 1.));
                let fraction = node.size as f32 / focus.size.max(1) as f32;
                let is_dir = node.kind == Kind::Dir;
                let dragged = DraggedItem { node: ix, name: node.name.clone(), size: node.size };
                let path = r.tree.path_of(ix);
                let shown_path = r.shown_path(ix).to_string_lossy().into_owned();
                let in_collector = is_covered(&r.tree, &collected, ix);
                let in_app = findings::is_inside_bundle(&r.tree, ix);

                div()
                    .id(("row", ix))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .when(is_hovered, |d| d.bg(rgb(CARD_HOVER)))
                    .when(is_dir, |d| d.cursor_pointer())
                    .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                        if let Some(r) = this.results() {
                            if *hovering {
                                r.list_hover = Some(ix);
                            } else if r.list_hover == Some(ix) {
                                r.list_hover = None;
                            }
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(r) = this.results() {
                            r.navigate(ix);
                            cx.notify();
                        }
                    }))
                    .on_drag(dragged, |item, _, _, cx| cx.new(|_| item.clone()))
                    .child(div().size(px(10.)).flex_none().rounded_full().bg(swatch))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(node.name.clone()))
                            .child(
                                div()
                                    .h(px(2.))
                                    .w(relative(fraction.max(0.005)))
                                    .rounded_full()
                                    .bg(swatch)
                                    .opacity(0.6),
                            ),
                    )
                    .when(is_hovered && node.kind != Kind::Other, |d| {
                        d.child(
                            icon_button(("reveal", ix), "⌕", "Reveal in Finder").on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.reveal_path(&path);
                                },
                            )),
                        )
                        .child(icon_button(("copy-path", ix), "⧉", "Copy Path").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                let notice = format!("Copied {}", abbreviate_home(&shown_path));
                                this.copy_to_clipboard(shown_path.clone(), notice, cx);
                            },
                        )))
                        .when(!in_collector && !in_app, |d| {
                            d.child(icon_button(("collect", ix), "+", "Add to Collector").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.collect_items(&[ix], cx);
                                },
                            )))
                        })
                    })
                    .when(in_collector, |d| {
                        d.child(
                            icon_button(("uncollect-row", ix), "✓", "Remove from Collector")
                                .text_color(rgb(ACCENT))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(r) = this.results().filter(|r| !r.trashing) {
                                        r.collector.retain(|&c| c != ix);
                                    }
                                    this.collector_changed(cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(MUTED))
                            .text_xs()
                            .child(format_size(node.size)),
                    )
                    .child(
                        div()
                            .w(px(10.))
                            .flex_none()
                            .text_color(rgb(MUTED))
                            .child(if is_dir { "›" } else { "" }),
                    )
            })
            .collect()
    }

    fn render_collector(&self, r: &Results, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = r.collector.is_empty();
        let busy = r.trashing;
        // One chip per name: collecting every node_modules shouldn't make hundreds of
        // identical chips.
        let mut groups: Vec<(SharedString, Vec<usize>)> = Vec::new();
        for &ix in &r.collector {
            let name = &r.tree.nodes[ix].name;
            match groups.iter_mut().find(|(n, _)| n == name) {
                Some((_, members)) => members.push(ix),
                None => groups.push((name.clone(), vec![ix])),
            }
        }
        const MAX_CHIPS: usize = 12;
        let mut chips = div().flex().flex_wrap().gap_1().when(busy, |d| d.opacity(0.5));
        for (name, members) in groups.iter().take(MAX_CHIPS) {
            let count = members.len();
            let members = members.clone();
            chips = chips.child(
                div()
                    .id(("chip", members[0]))
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .pr_1()
                    .py_0p5()
                    .rounded_full()
                    .bg(rgb(CARD_HOVER))
                    .text_xs()
                    .max_w(px(300.))
                    .child(div().truncate().child(name.clone()))
                    .when(count > 1, |d| d.child(div().flex_none().text_color(rgb(MUTED)).child(format!("×{}", format_count(count as u64)))))
                    .when(!busy, |d| {
                        d.child(
                            div()
                                .id(("uncollect", members[0]))
                                .px_1()
                                .rounded_full()
                                .cursor_pointer()
                                .text_color(rgb(MUTED))
                                .hover(|s| s.text_color(rgb(TEXT)))
                                .child("×")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(r) = this.results() {
                                        r.collector.retain(|c| !members.contains(c));
                                    }
                                    this.collector_changed(cx);
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        if groups.len() > MAX_CHIPS {
            chips = chips.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("+{} more", groups.len() - MAX_CHIPS)),
            );
        }
        let size = r.collected_size();
        let frees = r.collector_frees.get();
        let shared_note = frees.filter(|&f| !busy && size > f + size / 100).map(|f| {
            format!("{} is shared with files outside the selection (APFS clones or hard links), so deleting won't free it", format_size(size - f))
        });
        let items = match r.collector.len() {
            1 => "1 item".to_string(),
            n => format!("{} items", format_count(n as u64)),
        };

        div()
            .id("collector")
            .m_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(if r.trashed.is_some() && empty { SAFE } else { BORDER }))
            .bg(rgb(BG))
            .flex()
            .flex_col()
            .gap_2()
            .on_drag_over::<DraggedItem>(|style, _, _, _| style.border_color(rgb(ACCENT)).bg(rgb(CARD)))
            .on_drop(cx.listener(|this, item: &DraggedItem, _, cx| this.collect_items(&[item.node], cx)))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Collector"))
                    .when(!empty, |d| {
                        d.child(div().text_color(rgb(MUTED)).child(match frees {
                            Some(frees) => format!("{items} · frees {}", format_size(frees)),
                            None => format!("{items} · calculating…"),
                        }))
                    }),
            )
            .when_some(shared_note, |d, note| d.child(div().text_xs().text_color(rgb(MUTED)).child(note)))
            .when(empty, |d| match &r.trashed {
                Some(trashed) => d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .font_weight(FontWeight::MEDIUM)
                                .child(div().text_color(rgb(SAFE)).font_weight(FontWeight::BOLD).child("✓"))
                                .child(format!("Moved {} to the Trash", trashed.what)),
                        )
                        .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                            "Freed {}. You can put items back from the Trash in Finder.",
                            format_size(trashed.frees)
                        ))),
                ),
                None => d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("Drag items here (or press + on a row) to collect them for deletion."),
                ),
            })
            .when(!empty, |d| {
                d.child(chips).child(
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .items_center()
                        .when(busy, |d| {
                            d.child(
                                primary_button("trash", "Moving to Trash…", DANGER)
                                    .opacity(0.6)
                                    .cursor_default(),
                            )
                        })
                        .when(!busy, |d| {
                            d.child(button("clear-collector", "Clear").on_click(cx.listener(|this, _, _, cx| {
                                if let Some(r) = this.results() {
                                    r.collector.clear();
                                }
                                this.collector_changed(cx);
                                cx.notify();
                            })))
                            .child(
                                primary_button("trash", "Move to Trash…", DANGER)
                                    .on_click(cx.listener(|this, _, window, cx| this.trash_collected(window, cx))),
                            )
                        }),
                )
            })
    }

    fn render_chart(&self, r: &Results, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = &r.tree;
        let hovered = r.hovered();
        let colors: Vec<Hsla> = r
            .segments
            .iter()
            .map(|s| {
                let base = r.color(s);
                if let Some(category) = r.legend_hover {
                    return if r.category(s.target) == Some(category) { sunburst::lift(base) } else { sunburst::fade(base) };
                }
                let Some(target) = hovered else { return base };
                let lit = match (s.target, target) {
                    (a, b) if a == b => true,
                    (Target::Node(n), Target::Node(h)) => tree.is_ancestor_or_self(h, n),
                    (Target::Small { parent, .. }, Target::Node(h)) => tree.is_ancestor_or_self(h, parent),
                    _ => false,
                };
                match (lit, r.color_by) {
                    (true, ColorBy::Folder) => sunburst::highlight(base),
                    (true, ColorBy::Kind) => sunburst::lift(base),
                    (false, _) => sunburst::dim(base),
                }
            })
            .collect();

        let can_go_up = tree.nodes[r.focus].parent.is_some();
        let pointer = match r.chart_hover {
            Some(Hit::Center) => can_go_up,
            Some(Hit::Segment(i)) => r.segments.get(i).is_some_and(|s| s.kind == Kind::Dir && matches!(s.target, Target::Node(_))),
            None => false,
        };
        let center_hovered = r.chart_hover == Some(Hit::Center) && can_go_up;

        // The chart's label (centred on the sunburst, a strip above the icicle and treemap)
        // describes whatever is under the pointer.
        let (title, subtitle): (SharedString, String) = match hovered {
            Some(Target::Node(ix)) => {
                let node = &tree.nodes[ix];
                let detail = match node.kind {
                    Kind::Dir => format!("{} files", format_count(node.items)),
                    Kind::File => "file".into(),
                    Kind::Other if node.name.as_ref() == disk::NOT_READABLE => "needs Full Disk Access".into(),
                    Kind::Other => "exact, from APFS".into(),
                };
                let kind = match r.category(Target::Node(ix)) {
                    Some(category) if r.color_by == ColorBy::Kind && node.kind != Kind::Other => format!("\n{}", category.label()),
                    _ => String::new(),
                };
                (node.name.clone(), format!("{}\n{detail}{kind}", format_size(node.size)))
            }
            // Only the chart has these, and its label already says how big the run is.
            Some(Target::Small { .. }) => {
                let size = match r.chart_hover {
                    Some(Hit::Segment(i)) => r.labels.get(i).map(|(_, size)| size.to_string()),
                    _ => None,
                };
                ("Smaller objects".into(), size.unwrap_or_default())
            }
            None if center_hovered => ("↑ Back".into(), format!("to “{}”", tree.nodes[tree.nodes[r.focus].parent.unwrap()].name)),
            // The treemap's focus bar names it already, just below.
            None if r.chart == ChartType::Treemap => (SharedString::default(), String::new()),
            None => {
                let node = &tree.nodes[r.focus];
                (node.name.clone(), format_size(node.size))
            }
        };
        let label_width = r
            .chart_bounds
            .get()
            .map(|b| Geometry::new(b).inner_radius * 1.7)
            .unwrap_or(140.);

        let segments = r.segments.clone();
        let (keys, labels, layout_id) = (r.keys.clone(), r.labels.clone(), r.layout_id);
        let (motion, tiles, chart_type) = (r.motion.clone(), r.tiles.clone(), r.chart);
        // Colouring by folder takes the hue from the angle, so it follows the motion.
        let hue_follows = r.color_by == ColorBy::Folder;
        let bounds_cell = r.chart_bounds.clone();
        let entity = cx.entity().downgrade();
        let focus_node = &tree.nodes[r.focus];
        let focus = Some((focus_node.name.clone(), SharedString::from(format_size(focus_node.size))));
        // The "done" moment: what the scan found, in one line.
        let banner = r.banner.is_some().then(|| {
            let total = tree.nodes[Tree::ROOT].size;
            let pending = r.findings.iter().any(|f| f.pending);
            let safe: u64 = r.findings.iter().filter(|f| f.safety == Safety::Safe).map(|f| f.size).sum();
            let savings = if pending {
                " · working out savings…".to_string()
            } else if safe > 0 {
                format!(" · {} safe to delete", format_size(safe))
            } else {
                String::new()
            };
            div()
                .id("done-banner")
                .flex_none()
                .px_4()
                .py_2()
                .rounded_full()
                .bg(rgb(CARD))
                .border_1()
                .border_color(rgb(0x3fb950))
                .shadow_lg()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .child(div().text_color(rgb(0x3fb950)).font_weight(FontWeight::BOLD).child("✓"))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Scan complete in {:.1} s", r.elapsed.as_secs_f32())))
                .child(div().text_color(rgb(MUTED)).child(format!("{} accounted for{savings}", format_size(total))))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(r) = this.results() {
                        r.banner = None;
                        cx.notify();
                    }
                }))
        });
        // The icicle and treemap have a label strip to put it in.
        let (in_strip, centred) = if chart_type == ChartType::Sunburst { (None, banner) } else { (banner, None) };
        // The icicle and treemap leave their legend room below them; the sunburst's sits in a corner.
        let legend = (r.color_by == ColorBy::Kind).then(|| render_legend(r, chart_type != ChartType::Sunburst, cx));

        div()
            .flex_1()
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .child(
                canvas(
                    move |bounds, window, _| {
                        bounds_cell.set(Some(bounds));
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    },
                    move |bounds, hitbox, window, cx| {
                        let center = if center_hovered { to_hsla(0x3a3d44) } else { to_hsla(0x2b2d33) };
                        let layout = ChartLayout { id: layout_id, keys: &keys, segments: &segments, labels: &labels };
                        let motions = ChartMotions { rings: &motion, tiles: &tiles };
                        let frame = paint_chart(chart_type, bounds, &layout, 1.0, None, center, focus, motions, window, cx, |i, start, end| {
                            let color = colors[i];
                            if hue_follows { gpui::hsla(((start + end) / 2.0).rem_euclid(1.0), color.saturation, color.lightness, color.alpha) } else { color }
                        });
                        if frame.moving {
                            window.request_animation_frame();
                        }
                        // Mid-fold, nothing is where it's drawn: leave the pointer be until it's done.
                        if frame.folding {
                            return;
                        }

                        if pointer {
                            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                        }

                        let hits = Rc::new(frame.hits);
                        let (hits_for_move, segments_for_move) = (hits.clone(), segments.clone());
                        let entity_for_move = entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            let hit = if bounds.contains(&event.position) { hits_for_move.hit(event.position, &segments_for_move) } else { None };
                            entity_for_move.update(cx, |this, cx| this.set_chart_hover(hit, cx)).ok();
                        });
                        let segments_for_click = segments.clone();
                        let entity_for_click = entity.clone();
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx: &mut App| {
                            if phase != DispatchPhase::Bubble
                                || event.button != MouseButton::Left
                                || !bounds.contains(&event.position)
                            {
                                return;
                            }
                            if let Some(hit) = hits.hit(event.position, &segments_for_click) {
                                entity_for_click.update(cx, |this, cx| this.chart_click(hit, cx)).ok();
                            }
                        });
                    },
                )
                .flex_1()
                .min_h_0()
                .w_full(),
            )
            .child(chart_label(chart_type, label_width, title, subtitle.lines().map(str::to_string).collect(), in_strip))
            .children(legend)
            // Over the sunburst, the banner is centred above the circle.
            .children(centred.map(|banner| div().absolute().top_4().left_0().right_0().flex().justify_center().child(banner)))
    }
}

/// A layout as a chart canvas paints it: the segments, with their keys (for motion) and
/// labels (for the icicle and treemap), all aligned.
struct ChartLayout<'a> {
    id: u64,
    keys: &'a [motion::Key],
    segments: &'a [Segment],
    labels: &'a [(SharedString, SharedString)],
}

/// The chart's motion: `rings` for the sunburst and icicle, `tiles` for the treemap.
#[derive(Clone, Copy)]
struct ChartMotions<'a> {
    rings: &'a std::cell::RefCell<motion::ChartMotion>,
    tiles: &'a std::cell::RefCell<TreemapMotion>,
}

/// Where things were painted, for the pointer.
enum ChartHits {
    Rings(Geometry),
    /// The treemap's tiles, and its focus bar.
    Tiles(Vec<Option<Bounds<Pixels>>>, Bounds<Pixels>),
}

impl ChartHits {
    fn hit(&self, position: gpui::Point<Pixels>, segments: &[Segment]) -> Option<Hit> {
        match self {
            ChartHits::Rings(geometry) => geometry.hit_test(position, segments),
            ChartHits::Tiles(tiles, bar) => treemap::hit_test(tiles, *bar, position),
        }
    }
}

struct ChartFrame {
    hits: ChartHits,
    /// Still moving: paint another frame.
    moving: bool,
    /// What's drawn isn't where things are (see `ChartMotion::folding`).
    folding: bool,
}

/// The plate the chart sits on.
const CHART_BACKDROP: u32 = 0x18191c;

/// Just the plate, for before there's anything to chart.
fn paint_backdrop(chart: ChartType, bounds: Bounds<Pixels>, window: &mut Window) {
    match chart {
        ChartType::Sunburst => Geometry::new(bounds).paint_backdrop(window, to_hsla(CHART_BACKDROP)),
        ChartType::Icicle => Geometry::icicle(bounds).paint_backdrop(window, to_hsla(CHART_BACKDROP)),
        ChartType::Treemap => paint_treemap_backdrop(treemap::content_area(bounds).1, window),
    }
}

fn paint_treemap_backdrop(area: Bounds<Pixels>, window: &mut Window) {
    window.paint_quad(gpui::fill(area.dilate(px(6.)), to_hsla(CHART_BACKDROP)).corner_radii(px(8.)));
}

/// Move the chart on a frame and paint it, as whichever type it is: what the scan's and the
/// results' canvases share. `fraction` is the share of the chart scanned so far, with
/// `pending` the colour of the rest; `center` colours what stands for the folder in focus and
/// takes you up a level (the sunburst's centre, the icicle's first column, the treemap's focus
/// bar), and `focus` titles the treemap's focus bar (name, size). `color` is as for
/// `ChartMotion::paint`.
#[allow(clippy::too_many_arguments)]
fn paint_chart(
    chart: ChartType,
    bounds: Bounds<Pixels>,
    layout: &ChartLayout,
    fraction: f32,
    pending: Option<Hsla>,
    center: Hsla,
    focus: Option<(SharedString, SharedString)>,
    motions: ChartMotions,
    window: &mut Window,
    cx: &mut App,
    color: impl FnMut(usize, f32, f32) -> Hsla,
) -> ChartFrame {
    let label = |i: usize| layout.labels.get(i).cloned();
    match chart {
        ChartType::Sunburst | ChartType::Icicle => {
            let geometry = if chart == ChartType::Icicle { Geometry::icicle(bounds) } else { Geometry::new(bounds) };
            geometry.paint_backdrop(window, to_hsla(CHART_BACKDROP));
            let mut motion = motions.rings.borrow_mut();
            motion.retarget(layout.id as usize, layout.keys, layout.segments, fraction);
            let moving = motion.step(clock::now());
            let fraction = motion.fraction();
            let painted = motion.paint(window, &geometry, color);
            let folding = motion.folding();
            drop(motion);
            if let Some(pending) = pending.filter(|_| fraction < 0.9999) {
                geometry.paint_sector(window, 1, fraction, 1.0, pending);
            }
            // Labels would only flicker as the fold resizes everything under them.
            if chart == ChartType::Icicle && !folding {
                sunburst::paint_icicle_labels(&geometry, &painted, label, window, cx);
            }
            geometry.paint_center(window, center);
            ChartFrame { hits: ChartHits::Rings(geometry), moving, folding }
        }
        ChartType::Treemap => {
            let (bar, area) = treemap::content_area(bounds);
            paint_treemap_backdrop(area, window);
            let mut tiles = motions.tiles.borrow_mut();
            tiles.retarget(layout.id as usize, layout.keys, layout.segments, area, fraction);
            let moving = tiles.step(clock::now());
            let fraction = tiles.fraction();
            let painted = tiles.paint(window, color);
            let folding = tiles.folding();
            let hits = ChartHits::Tiles(tiles.tiles().to_vec(), bar);
            drop(tiles);
            if let Some(pending) = pending.filter(|_| fraction < 0.9999) {
                // The tiles are squeezed to the left by `fraction`; the rest is still to come.
                let x = area.left() + area.size.width * fraction + px(1.);
                let rest = Bounds::from_corners(gpui::point(x.min(area.right()), area.top()), area.bottom_right());
                window.paint_quad(gpui::fill(rest, pending).corner_radii(px(2.)));
            }
            if !folding {
                treemap::paint_labels(&painted, label, window, cx);
            }
            if let Some((title, size)) = focus {
                treemap::paint_focus_bar(bar, &title, &size, center, window, cx);
            }
            ChartFrame { hits, moving, folding }
        }
    }
}

/// The label over the chart: in the middle of a sunburst (at most `max_width` across), or in
/// a strip across the top of the icicle and treemap, which leave room for it there. The strip
/// can end with `trailing` (the scan-complete banner), which the label gives way to.
fn chart_label(chart: ChartType, max_width: f32, title: SharedString, lines: Vec<String>, trailing: Option<Stateful<gpui::Div>>) -> gpui::Div {
    if chart != ChartType::Sunburst {
        return div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(52.))
            .px(px(18.))
            .flex()
            .items_center()
            .gap_3()
            .child(div().min_w_0().flex_shrink_0().max_w(relative(0.6)).text_base().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
            .child(div().min_w_0().flex_1().text_xs().text_color(rgb(MUTED)).truncate().child(lines.join(" · ")))
            .children(trailing);
    }
    div().absolute().inset_0().flex().flex_col().items_center().justify_center().child(
        div()
            .max_w(px(max_width))
            .flex()
            .flex_col()
            .items_center()
            .gap_0p5()
            .child(div().max_w_full().text_base().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
            .children(lines.into_iter().map(|line| div().text_xs().text_color(rgb(MUTED)).child(line))),
    )
}

/// One choice in a toolbar toggle ("Colour: Folder | Kind").
fn toggle_option(id: &'static str, label: &'static str, selected: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(rgb(CARD_HOVER)).text_color(rgb(TEXT)))
        .when(!selected, |d| d.text_color(rgb(MUTED)).hover(|s| s.text_color(rgb(TEXT))))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label)
}

/// A row of `toggle_option`s, with its caption when there's room.
fn toggle(caption: Option<&'static str>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded_lg()
        .border_1()
        .border_color(rgb(BORDER))
        .text_xs()
        .children(caption.map(|caption| div().pl_1p5().pr_0p5().text_color(rgb(MUTED)).child(caption)))
}

/// "Colour: Folder | Kind" in the toolbar.
fn color_toggle(current: ColorBy, caption: bool, cx: &mut Context<Petal>) -> impl IntoElement {
    let option = |id, label, value: ColorBy, cx: &mut Context<Petal>| {
        toggle_option(id, label, current == value).on_click(cx.listener(move |this, _, _, cx| this.set_color_by(value, cx)))
    };
    toggle(caption.then_some("Colour")).child(option("color-folder", "Folder", ColorBy::Folder, cx)).child(option("color-kind", "Kind", ColorBy::Kind, cx))
}

/// "Chart: Sunburst | Icicle | Treemap" in the toolbar.
fn chart_toggle(current: ChartType, caption: bool, cx: &mut Context<Petal>) -> impl IntoElement {
    let option = |id, label, hint, value: ChartType, cx: &mut Context<Petal>| {
        toggle_option(id, label, current == value).tooltip(tooltip(hint)).on_click(cx.listener(move |this, _, _, cx| this.set_chart(value, cx)))
    };
    toggle(caption.then_some("Chart"))
        .child(option("chart-sunburst", "Sunburst", "Rings round the folder (⌘1)", ChartType::Sunburst, cx))
        .child(option("chart-icicle", "Icicle", "Columns, one per level, with names (⌘2)", ChartType::Icicle, cx))
        .child(option("chart-treemap", "Treemap", "Boxes sized by space, one level at a time (⌘3)", ChartType::Treemap, cx))
}

/// Which colour means which kind, for the kinds in view. Hover a kind to highlight it.
/// `in_row`: a row under the chart, rather than a list in its corner.
fn render_legend(r: &Results, in_row: bool, cx: &mut Context<Petal>) -> gpui::Div {
    let legend = if in_row {
        div().flex_none().flex().flex_wrap().justify_center().gap_x_3().gap_y_1().px_4().pb_3().text_xs()
    } else {
        div().absolute().left_4().bottom_4().flex().flex_col().gap_0p5().p_2().rounded_lg().bg(rgb(PANEL)).border_1().border_color(rgb(BORDER)).text_xs()
    };
    let mut legend = legend;
    for category in CATEGORIES.into_iter().filter(|&c| r.segments.iter().any(|s| r.category(s.target) == Some(c))) {
        let lit = r.legend_hover.is_none_or(|h| h == category);
        legend = legend.child(
            div()
                .id(("legend", category as usize))
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .text_color(rgb(if lit { TEXT } else { MUTED }))
                .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                    if let Some(r) = this.results() {
                        if *hovering {
                            r.legend_hover = Some(category);
                        } else if r.legend_hover == Some(category) {
                            r.legend_hover = None;
                        }
                        cx.notify();
                    }
                }))
                .child(div().size(px(10.)).flex_none().rounded_full().bg(classify::color(category)))
                .child(category.label()),
        );
    }
    legend
}

fn icon_button(id: impl Into<gpui::ElementId>, glyph: &'static str, label: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .tooltip(tooltip(label))
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_color(rgb(MUTED))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(BORDER)).text_color(rgb(TEXT)))
        .child(glyph)
}

fn meter(fraction: f32) -> impl IntoElement {
    let color = if fraction > 0.9 { DANGER } else { ACCENT };
    div()
        .mt_1()
        .h(px(6.))
        .w_full()
        .rounded_full()
        .bg(rgb(BORDER))
        .child(div().h_full().w(relative(fraction.clamp(0.0, 1.0))).rounded_full().bg(rgb(color)))
}

fn disk_gauge(fraction: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let geometry = Geometry::ring(bounds.center(), 13.0, 23.0);
            let fraction = fraction.clamp(0.0, 1.0);
            let color = if fraction > 0.9 { to_hsla(DANGER) } else { to_hsla(ACCENT) };
            geometry.paint_sector(window, 1, 0.0, 1.0, to_hsla(BORDER));
            geometry.paint_sector(window, 1, 0.0, fraction, color);
        },
    )
    .size(px(48.))
    .flex_none()
}
