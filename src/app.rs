use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Context, CursorStyle, DispatchPhase, FocusHandle, FontWeight, HapticFeedbackStyle, HitboxBehavior,
    Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels,
    PromptLevel, Rgba, ScrollStrategy, SharedString, Stateful, Task, UniformListScrollHandle,
    Window, actions, canvas, div, prelude::*, px, relative, rgb, uniform_list,
};

use palette::IntoColor;

use crate::clock;
use crate::disk;
use crate::eta;
use crate::onboarding;
use crate::findings::{self, Finding, Safety};
use crate::live;
use crate::motion;
use crate::scan::{self, Kind, Progress, Tree, Volume, format_count, format_size};
use crate::sunburst::{self, Geometry, Hit, Segment, Target};

actions!(petal, [GoUp, OpenFolder, Rescan, StartOver]);

const BG: u32 = 0x1c1d21;
const PANEL: u32 = 0x232529;
const CARD: u32 = 0x2a2c31;
const CARD_HOVER: u32 = 0x33363c;
const BORDER: u32 = 0x34363c;
const TEXT: u32 = 0xe8e9ec;
const MUTED: u32 = 0x8d919a;
const ACCENT: u32 = 0x4f9dff;
const DANGER: u32 = 0xe5484d;

const ROW_HEIGHT: f32 = 30.0;
const ZOOM_DURATION: Duration = Duration::from_millis(450);

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
    /// Folder being looked at mid-scan, by name path from the root, so it survives the
    /// re-snapshots (node indices change between them).
    focus: Vec<SharedString>,
    hover: Option<Vec<SharedString>>,
    chart_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    eta: eta::Eta,
    finals: HashMap<Vec<SharedString>, Instant>,
    /// Segments ease between snapshots instead of jumping.
    motion: Rc<std::cell::RefCell<motion::ChartMotion>>,
    _tasks: [Task<()>; 2],
}

/// A snapshot of the running totals, laid out around the folder in focus.
struct LiveView {
    tree: Tree,
    /// Which folders are final, indexed like `tree.nodes`.
    done: Vec<bool>,
    focus: usize,
    segments: Rc<Vec<Segment>>,
    swatches: HashMap<usize, Hsla>,
    /// The "not scanned yet" slice of the startup disk, drawn pulsing.
    pending: Option<usize>,
    /// When each recently finalised folder became final, so it can glow briefly.
    settled_at: HashMap<usize, Instant>,
    /// Each segment's identity (folder path), aligned with `segments`, for smooth motion.
    keys: Vec<motion::Key>,
    /// Unique per layout, so the animation knows when to retarget.
    id: u64,
}

static NEXT_LAYOUT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

const LIVE_REFRESH: Duration = Duration::from_millis(100);
/// How long a folder glows after its total becomes final.
const SETTLE_GLOW: Duration = Duration::from_millis(700);
/// How long the "scan complete" banner stays up.
const BANNER_TIME: Duration = Duration::from_secs(8);

impl LiveView {
    /// `finals` remembers when each folder (by path) was first seen final, across snapshots.
    fn new(snapshot: live::LiveSnapshot, focus_path: &[SharedString], finals: &mut HashMap<Vec<SharedString>, Instant>) -> Self {
        let live::LiveSnapshot { tree, done } = snapshot;
        let pending = tree.nodes[Tree::ROOT]
            .children
            .iter()
            .copied()
            .find(|&c| tree.nodes[c].kind == Kind::Other && tree.nodes[c].name.as_ref() == disk::NOT_SCANNED);
        let now = clock::now();
        let mut settled_at = HashMap::new();
        for ix in (0..tree.nodes.len()).filter(|&ix| done[ix] && tree.nodes[ix].kind == Kind::Dir && ix != Tree::ROOT) {
            let at = *finals.entry(path_to(&tree, ix)).or_insert(now);
            if now - at < SETTLE_GLOW {
                settled_at.insert(ix, at);
            }
        }
        let mut view = Self {
            tree,
            done,
            focus: Tree::ROOT,
            segments: Rc::default(),
            swatches: HashMap::new(),
            pending,
            settled_at,
            keys: Vec::new(),
            id: 0,
        };
        view.refocus(focus_path);
        view
    }

    fn refocus(&mut self, focus_path: &[SharedString]) {
        self.focus = resolve_path(&self.tree, focus_path);
        let segments = sunburst::layout(&self.tree, self.focus);
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
                Target::Small { parent } => motion::Key::Small(path_to(&self.tree, parent)),
            })
            .collect();
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

/// Whether `ix` is `ancestor` or lies somewhere below it.
fn is_within(tree: &Tree, mut ix: usize, ancestor: usize) -> bool {
    loop {
        if ix == ancestor {
            return true;
        }
        match tree.nodes[ix].parent {
            Some(parent) => ix = parent,
            None => return false,
        }
    }
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
    anim_start: Instant,
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
}

impl Results {
    fn new(tree: Tree, findings: Vec<Finding>, elapsed: Duration, requested_root: PathBuf) -> Self {
        let mut results = Self {
            tree,
            focus: Tree::ROOT,
            segments: Rc::default(),
            swatches: HashMap::new(),
            chart_hover: None,
            list_hover: None,
            anim_start: clock::now(),
            chart_bounds: Rc::default(),
            collector: Vec::new(),
            list_scroll: UniformListScrollHandle::new(),
            elapsed,
            requested_root,
            findings,
            collector_frees: Cell::new(None),
            collector_version: 0,
            banner: None,
        };
        results.relayout();
        results
    }

    fn relayout(&mut self) {
        let segments = sunburst::layout(&self.tree, self.focus);
        self.swatches = segments
            .iter()
            .filter_map(|s| match s.target {
                Target::Node(ix) if s.depth == 1 => Some((ix, sunburst::base_color(s))),
                _ => None,
            })
            .collect();
        self.segments = Rc::new(segments);
    }

    fn navigate(&mut self, ix: usize) {
        if self.tree.nodes[ix].kind != Kind::Dir || ix == self.focus {
            return;
        }
        self.focus = ix;
        self.chart_hover = None;
        self.list_hover = None;
        self.anim_start = clock::now();
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
        if ix == Tree::ROOT || self.tree.nodes[ix].parent.is_none() || self.tree.nodes[ix].kind == Kind::Other {
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
    access: Access,
    _access_watch: Option<Task<()>>,
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
            access,
            _access_watch: None,
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
                        // Open the results where the user was looking during the scan.
                        let focus = match &this.screen {
                            Screen::Scanning(scanning) => scanning.focus.clone(),
                            _ => Vec::new(),
                        };
                        let mut results = Results::new(tree, findings, clock::since(started), root_for_results);
                        let ix = resolve_path(&results.tree, &focus);
                        if ix != Tree::ROOT {
                            results.navigate(ix);
                        }
                        // The chart is already on screen from the scan; don't replay the sweep.
                        results.anim_start = clock::now() - ZOOM_DURATION;
                        results.banner = Some(clock::now());
                        this.screen = Screen::Results(results);
                        this.resolve_pending_findings(cx);
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
                    let Screen::Scanning(scanning) = &mut this.screen else { return false };
                    let items = scanning.progress.files.load(Ordering::Relaxed) + scanning.progress.dirs.load(Ordering::Relaxed);
                    scanning.eta.update(clock::since(scanning.started).as_secs_f64(), items);
                    if scanning.live.is_none() || clock::since(scanning.last_snapshot) >= LIVE_REFRESH {
                        let layout = scanning.progress.layout.get().map(|l| &**l);
                        let root = layout.map(|l| l.data_root.clone()).unwrap_or_else(|| scanning.root.clone());
                        let snapshot = live::snapshot(&scanning.progress.live, &root, layout);
                        scanning.live = Some(Rc::new(LiveView::new(snapshot, &scanning.focus, &mut scanning.finals)));
                        scanning.last_snapshot = clock::now();
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
            focus: Vec::new(),
            hover: None,
            chart_bounds: Rc::default(),
            eta: eta::Eta::default(),
            finals: HashMap::new(),
            motion: Rc::default(),
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

    fn scanning(&mut self) -> Option<&mut Scanning> {
        match &mut self.screen {
            Screen::Scanning(s) => Some(s),
            _ => None,
        }
    }

    fn live_focus(&mut self, path: Vec<SharedString>, cx: &mut Context<Self>) {
        if let Some(scanning) = self.scanning() {
            scanning.focus = path;
            scanning.hover = None;
            // Re-lay out the current snapshot straight away rather than waiting for the next one.
            if let Some(view) = scanning.live.take() {
                let mut view = Rc::try_unwrap(view).unwrap_or_else(|rc| LiveView {
                    tree: Tree { root_path: rc.tree.root_path.clone(), nodes: rc.tree.nodes.clone(), errors: 0, cloud_only: 0 },
                    done: rc.done.clone(),
                    focus: rc.focus,
                    segments: rc.segments.clone(),
                    swatches: rc.swatches.clone(),
                    pending: rc.pending,
                    settled_at: rc.settled_at.clone(),
                    keys: rc.keys.clone(),
                    id: rc.id,
                });
                view.refocus(&scanning.focus);
                scanning.live = Some(Rc::new(view));
            }
            cx.notify();
        }
    }

    fn live_hover(&mut self, path: Option<Vec<SharedString>>, cx: &mut Context<Self>) {
        if let Some(scanning) = self.scanning() {
            if scanning.hover != path {
                scanning.hover = path;
                cx.notify();
            }
        }
    }

    /// Work out what the Collector frees, off the UI thread.
    fn collector_changed(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.results() else { return };
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
        for (i, finding) in r.findings.iter().enumerate().filter(|(_, f)| f.pending) {
            let paths: Vec<PathBuf> = finding.nodes.iter().map(|&ix| r.tree.path_of(ix)).collect();
            cx.spawn(async move |this, cx| {
                let frees = cx.background_spawn(async move { scan::frees_of(&paths) }).await;
                this.update(cx, |this, cx| {
                    if let Some(r) = this.results() {
                        if let Some(finding) = r.findings.get_mut(i) {
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
            let result = cx
                .background_spawn(async move { trash::delete_all(&paths) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        if let Some(r) = this.results() {
                            for &ix in &items {
                                if r.tree.is_ancestor_or_self(ix, r.focus) {
                                    r.focus = r.tree.nodes[ix].parent.unwrap_or(Tree::ROOT);
                                }
                                r.tree.remove(ix);
                            }
                            r.collector.clear();
                            r.collector_frees.set(None);
                            r.chart_hover = None;
                            r.list_hover = None;
                            r.anim_start = clock::now();
                            r.relayout();
                        }
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
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(".SystemUIFont")
            .text_sm()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().flex().child(content))
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
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            });

        match &self.screen {
            Screen::Results(r) => {
                let can_go_up = r.tree.nodes[r.focus].parent.is_some();
                bar = bar.child(
                    button("up", "‹")
                        .text_base()
                        .px_2()
                        .when(!can_go_up, |b| b.opacity(0.4))
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
                            .flex_shrink(1.)
                            .min_w(px(24.))
                            .truncate()
                            .when(is_last, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .when(!is_last, |d| d.text_color(rgb(MUTED)).flex_shrink_0())
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
                    .child(
                        div().text_xs().text_color(rgb(MUTED)).flex_none().child(format!(
                            "Scanned in {:.1}s",
                            r.elapsed.as_secs_f32()
                        )),
                    )
                    .child(
                        button("rescan", "Rescan")
                            .on_click(cx.listener(|this, _, window, cx| this.rescan(&Rescan, window, cx))),
                    )
                    .child(
                        button("start-over", "Disks")
                            .on_click(cx.listener(|this, _, window, cx| this.start_over(&StartOver, window, cx))),
                    );
            }
            _ => {
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
        let hovered = view.as_ref().zip(scanning.hover.as_ref()).map(|(v, path)| resolve_path(&v.tree, path));

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

        // Sidebar: the focused folder's contents.
        let mut rows = div().id("live-rows").flex_1().min_h_0().px_2().overflow_y_scroll().flex().flex_col();
        let mut crumbs = None;
        if let Some(view) = &view {
            let focus = &view.tree.nodes[view.focus];
            if view.focus != Tree::ROOT {
                let up = path_to(&view.tree, focus.parent.unwrap_or(Tree::ROOT));
                crumbs = Some(
                    div()
                        .id("live-up")
                        .px_4()
                        .pb_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(rgb(MUTED))
                        .cursor_pointer()
                        .hover(|s| s.text_color(rgb(TEXT)))
                        .child(format!("‹ {}", path_to(&view.tree, view.focus).iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" › ")))
                        .on_click(cx.listener(move |this, _, _, cx| this.live_focus(up.clone(), cx))),
                );
            }
            for &ix in &focus.children {
                let node = &view.tree.nodes[ix];
                let swatch = view.swatches.get(&ix).copied().unwrap_or(gpui::hsla(0., 0., 0.38, 1.));
                let is_final = view.is_final(ix);
                let is_hovered = hovered == Some(ix);
                let openable = node.kind == Kind::Dir && !node.children.is_empty();
                let path = path_to(&view.tree, ix);
                let hover_path = path.clone();
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
                        .when(is_hovered, |d| d.bg(rgb(CARD_HOVER)))
                        .when(openable, |d| d.cursor_pointer())
                        .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                            this.live_hover(hovering.then(|| hover_path.clone()), cx);
                        }))
                        .when(openable, |d| d.on_click(cx.listener(move |this, _, _, cx| this.live_focus(path.clone(), cx))))
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
                        )
                        .child(div().w(px(10.)).flex_none().text_color(rgb(MUTED)).child(if openable { "›" } else { "" })),
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
            .children(scanning.focus.is_empty().then(|| self.render_access_card(None, cx)).flatten())
            .when_some(
                (scanning.focus.is_empty())
                    .then(|| findings::early_findings(&progress.early_findings.lock().unwrap()))
                    .filter(|f| !f.is_empty()),
                |d, early| d.child(self.render_findings(&early, false, cx)),
            )
            .children(crumbs)
            .child(rows)
            .child(
                div()
                    .p_3()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_xs().text_color(rgb(MUTED)).child("Click a folder to look inside"))
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
        let entity = cx.entity().downgrade();

        // Centre label: whatever is under the pointer, else the folder in focus.
        let (title, lines): (String, Vec<String>) = match (&view, hovered) {
            (Some(v), Some(ix)) if ix != v.focus => {
                let node = &v.tree.nodes[ix];
                let state = match node.kind {
                    Kind::Other if node.name.as_ref() == disk::NOT_SCANNED => "still to scan",
                    Kind::Other => "exact, from APFS",
                    _ if v.is_final(ix) => "final",
                    _ => "still counting",
                };
                (node.name.to_string(), vec![size_label(v, ix), state.to_string()])
            }
            (Some(v), _) if v.focus != Tree::ROOT => {
                let node = &v.tree.nodes[v.focus];
                let state = if v.is_final(v.focus) { "final" } else { "still counting" };
                (node.name.to_string(), vec![size_label(v, v.focus), state.to_string(), "↑ click to go back".into()])
            }
            _ => match &layout {
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
            },
        };
        let label_width = scanning.chart_bounds.get().map(|b| Geometry::new(b).inner_radius * 1.7).unwrap_or(140.);
        let paint_view = view.clone();
        let hover_ix = hovered;

        let chart = div()
            .flex_1()
            .h_full()
            .relative()
            .child(
                canvas(
                    move |bounds, window, _| {
                        bounds_cell.set(Some(bounds));
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    },
                    move |bounds, hitbox, window, _| {
                        let geometry = Geometry::new(bounds);
                        geometry.paint_disc(window, geometry.outer_radius() + 6.0, to_hsla(0x18191c));
                        let Some(view) = paint_view else { return };
                        let pulse = 0.22 + 0.04 * (clock::since(started).as_secs_f32() * 3.0).sin();
                        let mut motion = motion.borrow_mut();
                        motion.retarget(view.id as usize, &view.keys, &view.segments, fraction);
                        motion.step(clock::now());
                        let fraction = motion.fraction;
                        for moving in motion.segments_mut() {
                            let Some(i) = moving.current else {
                                // Leaving the layout: keep its last colour while it shrinks away.
                                let (r0, r1) = geometry.ring_at(moving.depth);
                                geometry.paint_band(window, r0, r1, moving.start * fraction, moving.end * fraction, moving.color);
                                continue;
                            };
                            // Hue follows the animated position, so colours shift smoothly too.
                            let segment = &Segment { start: moving.start, end: moving.end, ..view.segments[i] };
                            let color = match segment.target {
                                Target::Node(ix) if Some(ix) == view.pending => gpui::hsla(0.0, 0.0, pulse, 1.0),
                                Target::Node(ix) => {
                                    let base = sunburst::base_color(segment);
                                    // Folders still being counted are drawn muted; they "settle" into
                                    // full colour when their total is final.
                                    let base = if view.tree.nodes[ix].kind == Kind::Dir && !view.is_final(ix) { sunburst::dim(base) } else { base };
                                    // A brief glow as the folder's total becomes final.
                                    let base = match view.settled_at.get(&ix) {
                                        Some(at) => {
                                            let k = 1.0 - (clock::since(*at).as_secs_f32() / SETTLE_GLOW.as_secs_f32()).min(1.0);
                                            gpui::hsla(base.hue.into_positive_degrees() / 360.0, base.saturation, (base.lightness + 0.2 * k).min(0.92), 1.0)
                                        }
                                        None => base,
                                    };
                                    match hover_ix {
                                        Some(h) if is_within(&view.tree, ix, h) => sunburst::highlight(base),
                                        _ => base,
                                    }
                                }
                                Target::Small { .. } => sunburst::base_color(segment),
                            };
                            moving.color = color;
                            let (r0, r1) = geometry.ring_at(moving.depth);
                            geometry.paint_band(window, r0, r1, moving.start * fraction, moving.end * fraction, color);
                        }
                        drop(motion);
                        if fraction < 0.9999 {
                            geometry.paint_sector(window, 1, fraction, 1.0, gpui::hsla(0.0, 0.0, pulse, 1.0));
                        }
                        window.request_animation_frame();
                        geometry.paint_disc(window, geometry.inner_radius - 1.0, to_hsla(0x2b2d33));

                        // Hit testing against exactly what was painted, so a newer snapshot can't
                        // shift what a click means.
                        let target_at = {
                            let view = view.clone();
                            move |position| -> Option<(bool, Vec<SharedString>)> {
                                match geometry.hit_test(position, &view.segments)? {
                                    Hit::Center => Some((true, path_to(&view.tree, view.tree.nodes[view.focus].parent.unwrap_or(Tree::ROOT)))),
                                    Hit::Segment(i) => match view.segments[i].target {
                                        Target::Node(ix) => Some((false, path_to(&view.tree, ix))),
                                        Target::Small { .. } => None,
                                    },
                                }
                            }
                        };
                        let openable = {
                            let view = view.clone();
                            move |path: &[SharedString]| {
                                let ix = resolve_path(&view.tree, path);
                                view.tree.nodes[ix].kind == Kind::Dir && !view.tree.nodes[ix].children.is_empty()
                            }
                        };
                        if let Some(position) = window.mouse_position().into() {
                            if bounds.contains(&position) {
                                if let Some((center, path)) = target_at(position) {
                                    if center || openable(&path) {
                                        window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                                    }
                                }
                            }
                        }
                        let (move_target, move_entity) = (target_at.clone(), entity.clone());
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble || !bounds.contains(&event.position) {
                                return;
                            }
                            let path = move_target(event.position).filter(|(center, _)| !center).map(|(_, p)| p);
                            move_entity.update(cx, |this, cx| this.live_hover(path, cx)).ok();
                        });
                        let click_entity = entity.clone();
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx: &mut App| {
                            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !bounds.contains(&event.position) {
                                return;
                            }
                            if let Some((center, path)) = target_at(event.position) {
                                if center || openable(&path) {
                                    click_entity.update(cx, |this, cx| this.live_focus(path, cx)).ok();
                                }
                            }
                        });
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .max_w(px(label_width))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_0p5()
                            .child(div().max_w_full().text_base().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
                            .children(lines.into_iter().map(|line| div().text_xs().text_color(rgb(MUTED)).child(line))),
                    ),
            );

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
                    .when(r.tree.cloud_only > 0, |d| {
                        d.child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                            "{} cloud-only folders not downloaded (use no space here)",
                            format_count(r.tree.cloud_only)
                        )))
                    })
                    .when(r.tree.errors > 0, |d| {
                        d.child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                            "{} items couldn’t be read (permissions)",
                            format_count(r.tree.errors)
                        )))
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
            .when(r.focus == Tree::ROOT && !r.findings.is_empty(), |d| d.child(self.render_findings(&r.findings, true, cx)))
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
    /// folder, + to collect it.
    fn render_findings(&self, findings: &[Finding], interactive: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let safe: u64 = findings.iter().filter(|f| f.safety == Safety::Safe).map(|f| f.size).sum();
        let mut list = div().id("findings").max_h(px(250.)).overflow_y_scroll().flex().flex_col().gap_0p5();
        for (i, finding) in findings.iter().enumerate() {
            let (tag, color) = match finding.safety {
                Safety::Safe => ("Safe to delete", 0x3fb950),
                Safety::Review => ("Review first", 0xd29922),
            };
            let target = finding.nodes.first().copied().filter(|_| finding.path.is_some());
            let nodes = finding.nodes.clone();
            list = list.child(
                div()
                    .id(("finding", i))
                    .group("finding")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .flex()
                    .flex_col()
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
                            .when(interactive, |d| {
                                d.child(
                                    icon_button(("collect-finding", i), "+", "Add to Collector")
                                        .invisible()
                                        .group_hover("finding", |s| s.visible())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            if let Some(r) = this.results() {
                                                for &ix in &nodes {
                                                    r.collect(ix);
                                                }
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
                            .child(div().min_w_0().text_color(rgb(MUTED)).truncate().child(finding.blurb.clone())),
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
                        .child(icon_button(("collect", ix), "+", "Add to Collector").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(r) = this.results() {
                                    r.collect(ix);
                                }
                                this.collector_changed(cx);
                                cx.notify();
                            },
                        )))
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
        // A dozen chips is plenty; collecting e.g. every node_modules adds hundreds.
        const MAX_CHIPS: usize = 12;
        let mut chips = div().flex().flex_wrap().gap_1();
        for &ix in r.collector.iter().take(MAX_CHIPS) {
            let node = &r.tree.nodes[ix];
            chips = chips.child(
                div()
                    .id(("chip", ix))
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
                    .child(div().truncate().child(node.name.clone()))
                    .child(
                        div()
                            .id(("uncollect", ix))
                            .px_1()
                            .rounded_full()
                            .cursor_pointer()
                            .text_color(rgb(MUTED))
                            .hover(|s| s.text_color(rgb(TEXT)))
                            .child("×")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.results() {
                                    r.collector.retain(|&c| c != ix);
                                }
                                this.collector_changed(cx);
                                cx.notify();
                            })),
                    ),
            );
        }
        if r.collector.len() > MAX_CHIPS {
            chips = chips.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("+{} more", r.collector.len() - MAX_CHIPS)),
            );
        }
        let size = r.collected_size();
        let frees = r.collector_frees.get();
        let shared_note = frees.filter(|&f| size > f + size / 100).map(|f| {
            format!("{} is shared with files outside the selection (APFS clones or hard links), so deleting won't free it", format_size(size - f))
        });

        div()
            .id("collector")
            .m_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(BG))
            .flex()
            .flex_col()
            .gap_2()
            .on_drag_over::<DraggedItem>(|style, _, _, _| style.border_color(rgb(ACCENT)).bg(rgb(CARD)))
            .on_drop(cx.listener(|this, item: &DraggedItem, _, cx| {
                if let Some(r) = this.results() {
                    r.collect(item.node);
                }
                this.collector_changed(cx);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Collector"))
                    .when(!empty, |d| {
                        d.child(div().text_color(rgb(MUTED)).child(match frees {
                            Some(frees) => format!("{} · frees {}", r.collector.len(), format_size(frees)),
                            None => format!("{} · calculating…", r.collector.len()),
                        }))
                    }),
            )
            .when_some(shared_note, |d, note| d.child(div().text_xs().text_color(rgb(MUTED)).child(note)))
            .when(empty, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("Drag items here (or press + on a row) to collect them for deletion."),
                )
            })
            .when(!empty, |d| {
                d.child(chips).child(
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(button("clear-collector", "Clear").on_click(cx.listener(|this, _, _, cx| {
                            if let Some(r) = this.results() {
                                r.collector.clear();
                            }
                            this.collector_changed(cx);
                            cx.notify();
                        })))
                        .child(
                            primary_button("trash", "Move to Trash…", DANGER)
                                .on_click(cx.listener(|this, _, window, cx| this.trash_collected(window, cx))),
                        ),
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
                let base = sunburst::base_color(s);
                let Some(target) = hovered else { return base };
                let lit = match (s.target, target) {
                    (a, b) if a == b => true,
                    (Target::Node(n), Target::Node(h)) => tree.is_ancestor_or_self(h, n),
                    (Target::Small { parent }, Target::Node(h)) => tree.is_ancestor_or_self(h, parent),
                    _ => false,
                };
                if lit { sunburst::highlight(base) } else { sunburst::dim(base) }
            })
            .collect();

        let can_go_up = tree.nodes[r.focus].parent.is_some();
        let pointer = match r.chart_hover {
            Some(Hit::Center) => can_go_up,
            Some(Hit::Segment(i)) => r.segments.get(i).is_some_and(|s| s.kind == Kind::Dir && matches!(s.target, Target::Node(_))),
            None => false,
        };
        let center_hovered = r.chart_hover == Some(Hit::Center) && can_go_up;

        // Label in the middle of the chart describes whatever is under the pointer.
        let (title, subtitle): (SharedString, String) = match hovered {
            Some(Target::Node(ix)) => {
                let node = &tree.nodes[ix];
                let detail = match node.kind {
                    Kind::Dir => format!("{} files", format_count(node.items)),
                    Kind::File => "file".into(),
                    Kind::Other if node.name.as_ref() == disk::NOT_READABLE => "needs Full Disk Access".into(),
                    Kind::Other => "exact, from APFS".into(),
                };
                (node.name.clone(), format!("{}\n{}", format_size(node.size), detail))
            }
            Some(Target::Small { parent }) => {
                let seg_size: u64 = tree.nodes[parent]
                    .children
                    .iter()
                    .map(|&c| tree.nodes[c].size)
                    .filter(|&s| (s as f32) < tree.nodes[parent].size as f32 * 0.004)
                    .sum();
                ("Smaller objects".into(), format_size(seg_size))
            }
            None if center_hovered => ("↑ Back".into(), format!("to “{}”", tree.nodes[tree.nodes[r.focus].parent.unwrap()].name)),
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
        let bounds_cell = r.chart_bounds.clone();
        let anim_start = r.anim_start;
        let entity = cx.entity().downgrade();

        div()
            .flex_1()
            .h_full()
            .relative()
            .child(
                canvas(
                    move |bounds, window, _| {
                        bounds_cell.set(Some(bounds));
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    },
                    move |bounds, hitbox, window, _| {
                        let geometry = Geometry::new(bounds);
                        let t = (clock::since(anim_start).as_secs_f32() / ZOOM_DURATION.as_secs_f32()).min(1.0);
                        let eased = 1.0 - (1.0 - t).powi(3);
                        if t < 1.0 {
                            window.request_animation_frame();
                        }

                        geometry.paint_disc(window, geometry.outer_radius() + 6.0, to_hsla(0x18191c));
                        for (segment, color) in segments.iter().zip(&colors) {
                            if segment.depth as f32 > 1.0 + eased * sunburst::MAX_DEPTH as f32 {
                                continue;
                            }
                            geometry.paint_sector(window, segment.depth, segment.start * eased, segment.end * eased, *color);
                        }
                        let center = if center_hovered { to_hsla(0x3a3d44) } else { to_hsla(0x2b2d33) };
                        geometry.paint_disc(window, geometry.inner_radius - 1.0, center);

                        if pointer {
                            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                        }

                        let segments_for_move = segments.clone();
                        let entity_for_move = entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            let hit = if bounds.contains(&event.position) {
                                geometry.hit_test(event.position, &segments_for_move)
                            } else {
                                None
                            };
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
                            if let Some(hit) = geometry.hit_test(event.position, &segments_for_click) {
                                entity_for_click.update(cx, |this, cx| this.chart_click(hit, cx)).ok();
                            }
                        });
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .max_w(px(label_width))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_0p5()
                            .child(
                                div()
                                    .max_w_full()
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(title),
                            )
                            .children(subtitle.lines().map(|line| {
                                div().text_color(rgb(MUTED)).text_xs().child(line.to_string())
                            })),
                    ),
            )
            .when(r.banner.is_some(), |d| {
                // The "done" moment: what the scan found, in one line.
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
                d.child(
                    div().absolute().top_4().left_0().right_0().flex().justify_center().child(
                        div()
                            .id("done-banner")
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
                            })),
                    ),
                )
            })
    }
}

fn icon_button(id: impl Into<gpui::ElementId>, glyph: &'static str, _tooltip: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
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
