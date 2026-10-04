//! Treemap: the folder in focus's contents as rectangles whose areas are their sizes, one
//! level at a time. There are no boxes within boxes: to see inside a folder, click it and the
//! chart zooms in, as the sunburst does.
//!
//! It takes the sunburst's `Vec<Segment>` (laid out one ring deep, `sunburst::layout_to`; a
//! segment's share of the focus folder, `end - start`, is its weight), so colours, keys,
//! hover, categories and the legend work the same whichever chart is showing. Anything deeper
//! that's passed anyway gets no tile: it's never painted and never hit.
//!
//! Motion follows `motion.rs`: every edge of every tile follows the same critically damped
//! spring, and tiles are matched across layouts by `Key`, so the picture morphs as one piece.
//! - The first layout is revealed all at once, each tile growing and fading in, the biggest
//!   a moment ahead of the smallest.
//! - New tiles grow from their centres; tiles that are gone shrink away where they were,
//!   fading, beneath what grows into their place.
//! - Changing the folder in focus moves a camera: one affine map for the whole chart, so the
//!   folder's tile opens out to fill the chart while its contents fade in over it (or, going
//!   up, the chart shrinks back into the folder's tile as that fades in over it). What's
//!   underneath stays opaque until what's on top has all but faded in, so the backdrop never
//!   shows through. Painting is clipped to the tile area, which tiles leave and enter by.
//! - When the scan finishes, its chart fades away while the results are revealed in its place
//!   (`fold`); the scan's tiles are in first-seen order and the results' largest first, so
//!   morphing between them would only be a shuffle.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{App, Bounds, ContentMask, Font, FontWeight, Hsla, Pixels, Point, SharedString, TextAlign, Window, fill, hsla, point, px, size};

use crate::motion::{Key, Spring, smootherstep};
use crate::scan::{Kind, Tree};
use crate::sunburst::{Hit, Segment, fit_line, label_color};

/// Room above the chart for the app's hover label strip.
const TOP_MARGIN: f32 = 52.0;
const SIDE_MARGIN: f32 = 16.0;
/// The focus folder's bar across the top of the chart, and the gap under it.
const FOCUS_BAR: f32 = 22.0;
const FOCUS_GAP: f32 = 3.0;
/// Each tile is inset by this inside its slot, so neighbours stand apart.
const GAP: f32 = 1.0;
const CORNER: f32 = 2.0;
/// Revealing the first layout: each tile grows in over `REVEAL_EACH`, from `REVEAL_FROM` of
/// its size, starting up to `REVEAL_STAGGER` after the biggest, by how far down the sizes it
/// comes. `REVEAL_SECONDS` covers the lot.
const REVEAL_EACH: f32 = 0.45;
const REVEAL_STAGGER: f32 = 0.3;
const REVEAL_SECONDS: f32 = REVEAL_EACH + REVEAL_STAGGER;
const REVEAL_FROM: f32 = 0.6;
/// How long the chart as drawn takes to fade away in a fold.
const FADE_SECONDS: f32 = 0.35;
/// In a zoom, how far the arriving tiles fade in before those leaving start to fade out.
/// Fading both at once would let the backdrop show through (a quarter of it halfway), so the
/// ones leaving stay opaque underneath until there's little left to show through.
const HOLD_UNTIL: f32 = 0.9;

/// The chart's two parts within the canvas: the focus folder's bar (clicking it goes up a
/// level) and the area the tiles fill. Both sit below the strip the app keeps for its hover
/// label.
pub fn content_area(bounds: Bounds<Pixels>) -> (Bounds<Pixels>, Bounds<Pixels>) {
    let r = Rect::of(bounds);
    let content = Rect { x0: r.x0 + SIDE_MARGIN, y0: r.y0 + TOP_MARGIN, x1: r.x1 - SIDE_MARGIN, y1: r.y1 - SIDE_MARGIN }.valid();
    let bar = Rect { y1: (content.y0 + FOCUS_BAR).min(content.y1), ..content };
    let tiles = Rect { y0: (bar.y1 + FOCUS_GAP).min(content.y1), ..content };
    (bar.bounds(), tiles.bounds())
}

/// Lay the segments out as a squarified treemap filling `area`: one tile per top-level
/// segment, in the order given (the results are largest first; the scan's chart is in
/// first-seen order, which keeps its tiles fairly stable as it grows), with areas in
/// proportion to their share of the focus folder. Aligned with `segments`; anything deeper
/// than the top level gets `None`, as the treemap shows one level only.
pub fn layout(segments: &[Segment], area: Bounds<Pixels>) -> Vec<Option<Bounds<Pixels>>> {
    let area = Rect::of(area).valid();
    let shown: Vec<usize> = (0..segments.len()).filter(|&i| segments[i].depth == 1).collect();
    let weights: Vec<f64> = shown.iter().map(|&i| (segments[i].end - segments[i].start).max(0.0) as f64).collect();
    let mut tiles = vec![None; segments.len()];
    // The whole area is the whole focus folder: contents too small to show are left out of
    // the segments, and their share is left empty rather than handed to the rest.
    for (&i, slot) in shown.iter().zip(squarify(&weights, 1.0, area)) {
        tiles[i] = Some(slot.gap(GAP).bounds());
    }
    tiles
}

/// Squarified treemap (Bruls, Huizing and van Wijk): split `rect` into one slot per weight,
/// in order, with areas in proportion to the weights, keeping slots as square as it can.
/// Rows are laid along the shorter side and grown while that improves their worst aspect
/// ratio. Zero weights get an empty slot at the centre. `whole` is the weight of all of
/// `rect`: when it's more than the weights add up to, the rest is left empty at the end, as
/// if it were one more (unseen) weight. Linear in the number of weights.
fn squarify(weights: &[f64], whole: f64, rect: Rect) -> Vec<Rect> {
    let (cx, cy) = rect.center();
    let n = weights.len();
    // One more slot, for the remainder, dropped at the end.
    let mut out = vec![Rect::point(cx, cy); n + 1];
    let mut items: Vec<usize> = (0..weights.len()).filter(|&i| weights[i] > 0.0 && weights[i].is_finite()).collect();
    let sum: f64 = items.iter().map(|&i| weights[i]).sum();
    // Rounding in the shares is ignored: only a real remainder (over 0.1%) is left empty.
    let rest = if whole.is_finite() && whole > sum * 1.001 { whole - sum } else { 0.0 };
    let weights: Vec<f64> = weights.iter().copied().chain([rest]).collect();
    if rest > 0.0 {
        items.push(weights.len() - 1);
    }
    let total = sum + rest;
    let (width, height) = (rect.w() as f64, rect.h() as f64);
    if total <= 0.0 || width <= 0.0 || height <= 0.0 {
        out.truncate(n);
        return out;
    }
    let scale = width * height / total;
    let (mut x0, mut y0, x1, y1) = (rect.x0 as f64, rect.y0 as f64, rect.x1 as f64, rect.y1 as f64);
    // How far from square the worst slot in a row would be.
    let worst = |sum: f64, lo: f64, hi: f64, side: f64| {
        let (s2, side2) = (sum * sum, side * side);
        (side2 * hi / s2).max(s2 / (side2 * lo))
    };
    let mut i = 0;
    while i < items.len() {
        let (w, h) = ((x1 - x0).max(0.0), (y1 - y0).max(0.0));
        let side = w.min(h);
        let first = weights[items[i]] * scale;
        let (mut sum, mut lo, mut hi) = (first, first, first);
        let mut ratio = worst(sum, lo, hi, side);
        let mut j = i + 1;
        while j < items.len() {
            let a = weights[items[j]] * scale;
            let next = worst(sum + a, lo.min(a), hi.max(a), side);
            if next > ratio {
                break;
            }
            (sum, lo, hi, ratio) = (sum + a, lo.min(a), hi.max(a), next);
            j += 1;
        }
        // The last row takes whatever is left, so rounding never leaves a sliver uncovered.
        let last = j == items.len();
        let row = &items[i..j];
        let mut along = 0.0;
        if w >= h {
            // A column down the left of what's left.
            let thick = if last || h <= 0.0 { w } else { (sum / h).min(w) };
            for (n, &k) in row.iter().enumerate() {
                let a = weights[k] * scale;
                let (from, to) = (y0 + h * along / sum, if n + 1 == row.len() { y1 } else { y0 + h * (along + a) / sum });
                out[k] = Rect { x0: x0 as f32, y0: from as f32, x1: (x0 + thick) as f32, y1: to as f32 };
                along += a;
            }
            x0 += thick;
        } else {
            // A row along the top.
            let thick = if last || w <= 0.0 { h } else { (sum / w).min(h) };
            for (n, &k) in row.iter().enumerate() {
                let a = weights[k] * scale;
                let (from, to) = (x0 + w * along / sum, if n + 1 == row.len() { x1 } else { x0 + w * (along + a) / sum });
                out[k] = Rect { x0: from as f32, y0: y0 as f32, x1: to as f32, y1: (y0 + thick) as f32 };
                along += a;
            }
            y0 += thick;
        }
        i = j;
    }
    out.truncate(n);
    out
}

/// A folder filled this much by one folder inside it shows as just that one box, as if
/// clicking it had only made the box bigger. Such chains are everywhere (an app's `Contents`,
/// a crate's `src`, a build's `target/release`), so the treemap opens and leaves them as one.
const FILLED: f64 = 0.99;

/// The folder inside `ix` that fills it (`FILLED`), if one does.
fn filled_by(tree: &Tree, ix: usize) -> Option<usize> {
    let node = &tree.nodes[ix];
    let &inner = node.children.iter().max_by_key(|&&c| tree.nodes[c].size)?;
    let size = tree.nodes[inner].size as f64;
    (tree.nodes[inner].kind == Kind::Dir && node.size > 0 && size >= node.size as f64 * FILLED).then_some(inner)
}

/// Where opening the folder `ix` goes: into it, and on through any folder that one folder
/// fills, to the first that shows more than one box.
pub fn opened(tree: &Tree, mut ix: usize) -> usize {
    while let Some(inner) = filled_by(tree, ix) {
        ix = inner;
    }
    ix
}

/// Where going up from `focus` goes: its folder, or, past any folder that one folder fills
/// (which would show just a box leading back down), the first that shows more than one.
pub fn enclosing(tree: &Tree, focus: usize) -> Option<usize> {
    let mut up = tree.nodes[focus].parent?;
    while let (Some(_), Some(parent)) = (filled_by(tree, up), tree.nodes[up].parent) {
        up = parent;
    }
    Some(up)
}

/// The tile under `position` (tiles never overlap), or the focus bar as `Hit::Center`.
pub fn hit_test(tiles: &[Option<Bounds<Pixels>>], focus_bar: Bounds<Pixels>, position: Point<Pixels>) -> Option<Hit> {
    let (x, y) = (f32::from(position.x), f32::from(position.y));
    if Rect::of(focus_bar).contains(x, y) {
        return Some(Hit::Center);
    }
    tiles.iter().position(|tile| tile.is_some_and(|tile| Rect::of(tile).contains(x, y))).map(Hit::Segment)
}

/// A rectangle by its edges, in pixels, for the arithmetic (and the springs, one per edge).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn of(bounds: Bounds<Pixels>) -> Self {
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        Self { x0: x, y0: y, x1: x + f32::from(bounds.size.width), y1: y + f32::from(bounds.size.height) }
    }

    fn bounds(self) -> Bounds<Pixels> {
        Bounds { origin: point(px(self.x0), px(self.y0)), size: size(px(self.w().max(0.0)), px(self.h().max(0.0))) }
    }

    fn point(x: f32, y: f32) -> Self {
        Self { x0: x, y0: y, x1: x, y1: y }
    }

    fn w(&self) -> f32 {
        self.x1 - self.x0
    }

    fn h(&self) -> f32 {
        self.y1 - self.y0
    }

    fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    /// Never inside out: an inverted side collapses to its middle.
    fn valid(self) -> Self {
        let (cx, cy) = self.center();
        let (x0, x1) = if self.x1 >= self.x0 { (self.x0, self.x1) } else { (cx, cx) };
        let (y0, y1) = if self.y1 >= self.y0 { (self.y0, self.y1) } else { (cy, cy) };
        Self { x0, y0, x1, y1 }
    }

    /// Inset by `d` on each side, on each axis where that leaves something.
    fn gap(self, d: f32) -> Self {
        let (x0, x1) = if self.w() > 2.0 * d { (self.x0 + d, self.x1 - d) } else { (self.x0, self.x1) };
        let (y0, y1) = if self.h() > 2.0 * d { (self.y0 + d, self.y1 - d) } else { (self.y0, self.y1) };
        Self { x0, y0, x1, y1 }
    }

    /// Scaled by `k` about its centre.
    fn scaled(self, k: f32) -> Self {
        let (cx, cy) = self.center();
        Self { x0: cx + (self.x0 - cx) * k, y0: cy + (self.y0 - cy) * k, x1: cx + (self.x1 - cx) * k, y1: cy + (self.y1 - cy) * k }
    }

    /// Each edge held within `to`. Holding everything the same way keeps tiles side by side,
    /// since no edge passes another.
    fn clamped(self, to: Rect) -> Self {
        Self { x0: self.x0.clamp(to.x0, to.x1), y0: self.y0.clamp(to.y0, to.y1), x1: self.x1.clamp(to.x0, to.x1), y1: self.y1.clamp(to.y0, to.y1) }
    }

    /// Grown by its own width and height on every side.
    fn surroundings(self) -> Self {
        Self { x0: self.x0 - self.w(), y0: self.y0 - self.h(), x1: self.x1 + self.w(), y1: self.y1 + self.h() }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    fn visible(&self) -> bool {
        self.w() >= 0.5 && self.h() >= 0.5
    }
}

/// A map of the plane that scales and shifts each axis: x → x × sx + ox, y → y × sy + oy.
/// The camera, and the way a new size of window takes the chart along.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    sx: f32,
    ox: f32,
    sy: f32,
    oy: f32,
}

impl Affine {
    /// The map taking `from` onto `to`; `None` when `from` has no area to map.
    fn between(from: Rect, to: Rect) -> Option<Self> {
        if from.w() <= 1e-3 || from.h() <= 1e-3 {
            return None;
        }
        let (sx, sy) = (to.w() / from.w(), to.h() / from.h());
        Some(Self { sx, ox: to.x0 - from.x0 * sx, sy, oy: to.y0 - from.y0 * sy })
    }

    fn inverse(&self) -> Option<Self> {
        (self.sx.abs() > 1e-6 && self.sy.abs() > 1e-6).then(|| Self { sx: 1.0 / self.sx, ox: -self.ox / self.sx, sy: 1.0 / self.sy, oy: -self.oy / self.sy })
    }

    fn apply(&self, r: Rect) -> Rect {
        Rect { x0: r.x0 * self.sx + self.ox, y0: r.y0 * self.sy + self.oy, x1: r.x1 * self.sx + self.ox, y1: r.y1 * self.sy + self.oy }
    }

    /// Carry a spring along: where it is, how fast it's going, and where it's headed.
    fn carry(&self, spring: &mut Spring, horizontal: bool) {
        let (s, o) = if horizontal { (self.sx, self.ox) } else { (self.sy, self.oy) };
        spring.x = spring.x * s + o;
        spring.v *= s;
        spring.target = spring.target * s + o;
    }
}

/// `find` for the folder `key`, or failing that for the nearest folder it's inside: the
/// treemap shows one level, so a jump of more than one (along the breadcrumbs, say) zooms by
/// the folder on the way that's on screen. The root (no names) is never on screen, so it
/// isn't tried.
fn nearest<T>(key: &Key, find: impl Fn(&Key) -> Option<T>) -> Option<T> {
    let Key::Node(path) = key else { return find(key) };
    (1..=path.len()).rev().find_map(|n| find(&Key::Node(path[..n].to_vec())))
}

/// One tile on its way to (or away from) its place in the latest layout.
struct TileMotion {
    /// Left, top, right and bottom.
    edges: [Spring; 4],
    /// Its start and end in turns, so hues taken from them follow the motion.
    turns: [Spring; 2],
    alpha: Spring,
    /// Index into the latest layout; `None` once the tile has left it.
    current: Option<usize>,
    /// Colour last drawn with, for tiles that are on their way out.
    color: Hsla,
    /// On its way out in a zoom, and kept opaque until the arriving tiles have faded in over
    /// it (`HOLD_UNTIL`), so only one layer is ever see-through at a time.
    held: bool,
}

impl TileMotion {
    fn rect(&self) -> Rect {
        Rect { x0: self.edges[0].x, y0: self.edges[1].x, x1: self.edges[2].x, y1: self.edges[3].x }
    }

    fn target(&self) -> Rect {
        Rect { x0: self.edges[0].target, y0: self.edges[1].target, x1: self.edges[2].target, y1: self.edges[3].target }
    }

    fn aim(&mut self, r: Rect) {
        for (edge, at) in self.edges.iter_mut().zip([r.x0, r.y0, r.x1, r.y1]) {
            edge.target = at;
        }
    }

    fn step(&mut self, dt: f32) {
        self.edges.iter_mut().chain(&mut self.turns).chain([&mut self.alpha]).for_each(|s| s.step(dt));
    }

    fn settled(&self) -> bool {
        // Edges are in pixels, so they're settled once within a hundredth of one.
        self.edges.iter().all(|e| (e.x - e.target).abs() < 0.01 && e.v.abs() < 0.1)
            && self.turns.iter().all(Spring::settled)
            && self.alpha.settled()
    }

    fn carry(&mut self, map: &Affine) {
        for (k, edge) in self.edges.iter_mut().enumerate() {
            map.carry(edge, k % 2 == 0);
        }
    }
}

fn still(r: Rect) -> [Spring; 4] {
    [Spring::still(r.x0, r.x0), Spring::still(r.y0, r.y0), Spring::still(r.x1, r.x1), Spring::still(r.y1, r.y1)]
}

/// Springs starting at `from`, headed for `to`.
fn moving(from: Rect, to: Rect) -> [Spring; 4] {
    [Spring::still(from.x0, to.x0), Spring::still(from.y0, to.y0), Spring::still(from.x1, to.x1), Spring::still(from.y1, to.y1)]
}

/// The chart as last drawn, fading away in a fold.
struct Ghost {
    rect: Rect,
    color: Hsla,
    alpha: f32,
}

struct Fade {
    /// Seconds in.
    t: f32,
    ghosts: Vec<Ghost>,
    /// The next layout has arrived (it may be empty: an empty folder).
    opened: bool,
}

/// A tile where it is right now.
struct Placed {
    /// Index into the latest layout; `None` for tiles on their way out.
    index: Option<usize>,
    rect: Rect,
    alpha: f32,
    color: Hsla,
}

/// A tile of the latest layout as `TreemapMotion::paint` drew it, for its label.
#[derive(Clone, Copy, Debug)]
pub struct PaintedTile {
    /// Index into the latest layout's segments.
    pub index: usize,
    pub rect: Bounds<Pixels>,
    pub alpha: f32,
    /// The tile area, which its label is kept inside as well as its own rect.
    pub clip: Bounds<Pixels>,
    /// The colour it was painted, before its alpha, so its label can be made to read on it.
    pub color: Hsla,
}

/// Motion for the treemap; see the module docs. The app calls `retarget` and `step` each
/// frame, then `paint`, then `paint_labels` with what was painted, and hit tests against
/// `tiles()`. `paint_focus_bar` draws the focus folder's bar, separately, since the app
/// knows its title and whether it's hovered.
pub struct TreemapMotion {
    tiles: HashMap<Key, TileMotion>,
    /// The latest layout, with its keys.
    layout: Vec<Option<Bounds<Pixels>>>,
    keys: Vec<Key>,
    /// How far down the latest layout's sizes each tile comes, from 0 (the biggest) to 1 (the
    /// smallest): how late it starts in a reveal.
    ranks: Vec<f32>,
    /// The area the tiles fill.
    area: Option<Rect>,
    last_frame: Option<Instant>,
    /// Which layout the targets come from, to retarget only on change.
    layout_id: usize,
    /// Share of the area the chart covers, across (volume scans fill it as they go).
    fraction: Spring,
    /// Applied to the next layout: the focus changed, from one folder to another.
    zoom: Option<(Key, Key)>,
    /// Seconds into revealing the first layout; `None` once done (or not started).
    reveal: Option<f32>,
    revealed: bool,
    fade: Option<Fade>,
    /// A fold is under way: fading out, then revealing what comes next.
    folded: bool,
}

impl Default for TreemapMotion {
    fn default() -> Self {
        Self {
            tiles: HashMap::new(),
            layout: Vec::new(),
            keys: Vec::new(),
            ranks: Vec::new(),
            area: None,
            last_frame: None,
            layout_id: 0,
            fraction: Spring::still(1.0, 1.0),
            zoom: None,
            reveal: None,
            revealed: false,
            fade: None,
            folded: false,
        }
    }
}

impl TreemapMotion {
    /// The next layout is centred on a different folder: `from` is the old focus's key and
    /// `to` the new one's. Move there as a camera would. The root's key (`Key::Node` of no
    /// names) is never in a layout, so it needs no special case: going into a folder from
    /// the root finds the folder in the old chart, and coming back out finds it in the new.
    pub fn zoom(&mut self, from: Key, to: Key) {
        self.zoom = Some((from, to));
    }

    /// Fade the chart as drawn away, and reveal the next layout in its place, as the first
    /// one is: for a layout so differently ordered (the scan's chart re-sorted by size) that
    /// morphing into it would be a shuffle.
    pub fn fold(&mut self) {
        let mut ghosts: Vec<Ghost> = Vec::new();
        if let Some(fade) = self.fade.take() {
            // Already fading: carry on from how faded it is.
            let k = 1.0 - smootherstep(fade.t / FADE_SECONDS);
            ghosts.extend(fade.ghosts.into_iter().map(|g| Ghost { alpha: g.alpha * k, ..g }));
        }
        ghosts.extend(self.placed().into_iter().filter(|p| p.alpha >= 0.01 && p.rect.visible()).map(|p| Ghost { rect: p.rect, color: p.color, alpha: p.alpha }));
        self.fade = Some(Fade { t: 0.0, ghosts, opened: false });
        self.folded = true;
        self.tiles.clear();
        self.layout.clear();
        self.keys.clear();
        self.ranks.clear();
        self.zoom = None;
        self.reveal = None;
        self.layout_id = 0;
    }

    /// Whether the chart is folding: what's drawn isn't where things are, so it shouldn't
    /// respond to the pointer. That lasts until what comes next is fully revealed.
    pub fn folding(&self) -> bool {
        self.fade.is_some() || (self.folded && self.reveal.is_some())
    }

    pub fn fraction(&self) -> f32 {
        self.fraction.x
    }

    /// The latest layout's tiles, where they're headed, for hit testing. (Not squeezed by
    /// `fraction`: only the scan's chart, which isn't interactive, covers less than all.)
    pub fn tiles(&self) -> &[Option<Bounds<Pixels>>] {
        &self.layout
    }

    /// Aim every tile at its place in a new layout filling `area` (the tile area from
    /// `content_area`). Lays out again only when the layout or the area changes.
    pub fn retarget(&mut self, layout_id: usize, keys: &[Key], segments: &[Segment], area: Bounds<Pixels>, fraction: f32) {
        if !self.revealed {
            // Nothing to grow from yet: start at the first real share instead.
            self.fraction = Spring::still(fraction, fraction);
        }
        self.fraction.target = fraction;
        let area_rect = Rect::of(area).valid();
        let resized = self.area.is_some_and(|old| old != area_rect);
        if self.layout_id == layout_id && !resized && self.area.is_some() {
            return;
        }
        // A new size of window: take everything in flight along with the area, so only the
        // change in arrangement is left to animate.
        if let Some(map) = self.area.filter(|_| resized).and_then(|old| Affine::between(old, area_rect)) {
            for motion in self.tiles.values_mut() {
                motion.carry(&map);
            }
            if let Some(fade) = &mut self.fade {
                for ghost in &mut fade.ghosts {
                    ghost.rect = map.apply(ghost.rect);
                }
            }
        }
        self.area = Some(area_rect);
        let new_layout = self.layout_id != layout_id;
        let layout = layout(segments, area);
        let shown = layout.iter().any(Option::is_some);
        // Tiles the camera carries off (or brings in) go no further than an area's width or
        // height past its edges, as the sunburst's camera stops at a full turn: zooming into a
        // sliver of a folder can stretch the chart hundreds of times over.
        let bounds = area_rect.surroundings();

        // The camera: one map from the old chart to the new one, and the same for turns
        // (old = new × scale + offset), so hues taken from them move with it.
        let mut camera = None;
        let mut turn_camera = None;
        // Only a new layout uses the zoom up: a new size of window alone leaves it waiting.
        let pending = if new_layout { self.zoom.take() } else { None };
        if let Some((from, to)) = pending {
            // Zooming in: the folder's tile (in the old chart) opens out to fill the area.
            let into = nearest(&to, |key| self.tiles.get(key).filter(|m| m.current.is_some()).map(|m| (m.target(), m.turns[0].target, m.turns[1].target)));
            if let Some((rect, t0, t1)) = into {
                camera = Affine::between(rect, area_rect);
                turn_camera = Some((t1 - t0, t0));
            }
            if camera.is_none() {
                // Zooming out: the whole old chart shrinks into the folder's tile in the new one.
                turn_camera = None;
                let out_of = nearest(&from, |key| keys.iter().position(|k| k == key).and_then(|i| layout.get(i).copied().flatten().map(|tile| (i, tile))));
                if let Some((i, tile)) = out_of {
                    camera = Affine::between(area_rect, Rect::of(tile));
                    let (f0, f1) = (segments[i].start, segments[i].end);
                    turn_camera = (f1 > f0).then(|| (1.0 / (f1 - f0), -f0 / (f1 - f0)));
                }
            }
            if camera.is_none() {
                // Neither folder is in the other's chart: cross-fade.
                turn_camera = None;
                self.fold();
            }
        }
        self.layout_id = layout_id;
        // In place straight away: the very first layout, and the one after a fold, both
        // revealed.
        let first = !self.revealed || (self.fade.is_some() && self.tiles.is_empty());
        if first && new_layout && shown {
            self.reveal = Some(0.0);
        }
        self.revealed |= shown;
        if let Some(fade) = &mut self.fade {
            fade.opened = true;
        }

        for motion in self.tiles.values_mut() {
            motion.current = None;
        }
        for (i, (key, segment)) in keys.iter().zip(segments).enumerate() {
            let Some(tile) = layout[i] else { continue };
            let target = Rect::of(tile);
            if let Some(motion) = self.tiles.get_mut(key) {
                motion.aim(target);
                motion.turns[0].target = segment.start;
                motion.turns[1].target = segment.end;
                motion.alpha.target = 1.0;
                motion.held = false;
                motion.current = Some(i);
                continue;
            }
            let at_rest = [Spring::still(segment.start, segment.start), Spring::still(segment.end, segment.end)];
            let (edges, turns, alpha) = if first {
                // The reveal brings the whole first chart in.
                (still(target), at_rest, Spring::still(1.0, 1.0))
            } else if let Some(start) = camera.and_then(|c| c.inverse()).map(|c| c.apply(target).clamped(bounds)) {
                // Where it would have been in the old chart, had it been drawn: inside the
                // folder zoomed into, or, zooming out, past the edges.
                let turn = |at: f32| Spring::still(turn_camera.map(|(k, o)| (at * k + o).clamp(0.0, 1.0)).unwrap_or(at), at);
                (moving(start, target), [turn(segment.start), turn(segment.end)], Spring::still(0.0, 1.0))
            } else {
                // Grow from its centre.
                let (cx, cy) = target.center();
                (moving(Rect::point(cx, cy), target), at_rest, Spring::still(0.0, 1.0))
            };
            self.tiles.insert(key.clone(), TileMotion { edges, turns, alpha, current: Some(i), color: hsla(0., 0., 0., 0.), held: false });
        }
        // Tiles that are gone: carried off by the camera, or shrinking away where they were.
        // The camera's stay opaque while what arrives fades in over them: the folder zoomed
        // into opens out beneath its contents, and going up, the old contents shrink away
        // beneath the folder's tile.
        for motion in self.tiles.values_mut().filter(|m| m.current.is_none()) {
            let target = motion.target();
            let to = match camera {
                Some(camera) => camera.apply(target).clamped(bounds),
                None => {
                    let (cx, cy) = target.center();
                    Rect::point(cx, cy)
                }
            };
            motion.aim(to);
            // (One already fading out carries on fading.)
            motion.held = camera.is_some() && motion.alpha.target > 0.0;
            if !motion.held {
                motion.alpha.target = 0.0;
            }
        }
        // Biggest first, for the reveal.
        let weight = |i: usize| segments[i].end - segments[i].start;
        let mut order: Vec<usize> = (0..layout.len()).filter(|&i| layout[i].is_some()).collect();
        order.sort_by(|&a, &b| weight(b).total_cmp(&weight(a)));
        let last = order.len().saturating_sub(1).max(1) as f32;
        self.ranks = vec![0.0; layout.len()];
        for (rank, &i) in order.iter().enumerate() {
            self.ranks[i] = rank as f32 / last;
        }
        self.keys = keys.to_vec();
        self.layout = layout;
    }

    /// Advance by the time since the last frame. Returns whether anything is still moving.
    pub fn step(&mut self, now: Instant) -> bool {
        let dt = self.last_frame.map(|last| (now - last).as_secs_f32()).unwrap_or(0.0).min(0.1);
        self.last_frame = Some(now);
        self.fraction.step(dt);
        let mut moving = !self.fraction.settled();
        if let Some(t) = &mut self.reveal {
            *t += dt;
            moving = true;
            if *t >= REVEAL_SECONDS {
                self.reveal = None;
            }
        }
        if let Some(fade) = &mut self.fade {
            fade.t += dt;
            moving = true;
            if fade.t >= FADE_SECONDS && fade.opened {
                self.fade = None;
            }
        }
        if self.fade.is_none() && self.reveal.is_none() {
            self.folded = false;
        }
        for motion in self.tiles.values_mut() {
            motion.step(dt);
        }
        // Tiles held in a zoom start to fade once what's arriving has all but faded in (or
        // straight away if nothing is: an empty folder).
        let arrived = self.tiles.values().filter(|m| m.current.is_some()).map(|m| m.alpha.x).fold(1.0f32, f32::min);
        if arrived >= HOLD_UNTIL {
            for motion in self.tiles.values_mut().filter(|m| m.held) {
                motion.held = false;
                motion.alpha.target = 0.0;
            }
        }
        self.tiles.retain(|_, motion| {
            let settled = motion.settled();
            moving |= !settled || motion.held;
            // Departed tiles go once they've shrunk away, faded out, or come to rest; held
            // ones wait to fade.
            motion.current.is_some() || motion.held || (!settled && motion.rect().visible() && motion.alpha.x >= 0.01)
        });
        moving
    }

    /// Every tile where it is right now, in painting order: tiles on their way out first, so
    /// what grows into their place (or, zooming, fades in over them) is drawn on top.
    fn placed(&self) -> Vec<Placed> {
        let Some(area) = self.area else { return Vec::new() };
        let fraction = self.fraction.x;
        // Volume scans fill the area from the left as they go.
        let squeeze = |r: Rect| Rect { x0: area.x0 + (r.x0 - area.x0) * fraction, x1: area.x0 + (r.x1 - area.x0) * fraction, ..r };
        let leaving = self.tiles.values().filter(|m| m.current.is_none());
        let mut out: Vec<Placed> = leaving.map(|m| Placed { index: None, rect: squeeze(m.rect()), alpha: m.alpha.x.clamp(0.0, 1.0), color: m.color }).collect();
        for (i, key) in self.keys.iter().enumerate() {
            // Only the latest layout's own tiles: a segment too deep to show has none.
            let Some(motion) = self.tiles.get(key).filter(|m| m.current == Some(i)) else { continue };
            let (mut rect, mut alpha) = (motion.rect(), motion.alpha.x.clamp(0.0, 1.0));
            if let Some(t) = self.reveal {
                let k = smootherstep((t - self.ranks[i] * REVEAL_STAGGER) / REVEAL_EACH);
                (rect, alpha) = (rect.scaled(REVEAL_FROM + (1.0 - REVEAL_FROM) * k), alpha * k);
            }
            out.push(Placed { index: Some(i), rect: squeeze(rect), alpha, color: motion.color });
        }
        out
    }

    /// Paint every tile where it is right now, and say what was painted (for labels).
    /// `color` gets the tile's index in the latest layout and its current start and end in
    /// turns (so hues taken from them follow the motion, as on the sunburst).
    pub fn paint(&mut self, window: &mut Window, mut color: impl FnMut(usize, f32, f32) -> Hsla) -> Vec<PaintedTile> {
        for (i, key) in self.keys.iter().enumerate() {
            if let Some(motion) = self.tiles.get_mut(key).filter(|m| m.current == Some(i)) {
                motion.color = color(i, motion.turns[0].x, motion.turns[1].x);
            }
        }
        let Some(area) = self.area else { return Vec::new() };
        let clip = area.bounds();
        let placed = self.placed();
        // Kept inside the tile area: the camera sends tiles off past its edges, and brings
        // them in from there.
        window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
            if let Some(fade) = &self.fade {
                let k = 1.0 - smootherstep(fade.t / FADE_SECONDS);
                for ghost in &fade.ghosts {
                    paint_tile(window, ghost.rect, ghost.color, ghost.alpha * k);
                }
            }
            let mut painted = Vec::new();
            for placed in placed {
                if paint_tile(window, placed.rect, placed.color, placed.alpha)
                    && let Some(index) = placed.index
                {
                    painted.push(PaintedTile { index, rect: placed.rect.bounds(), alpha: placed.alpha, clip, color: placed.color });
                }
            }
            painted
        })
    }
}

/// Paint one tile, rounded unless it's tiny. Returns whether anything was drawn.
fn paint_tile(window: &mut Window, rect: Rect, color: Hsla, alpha: f32) -> bool {
    if alpha < 0.01 || !rect.visible() {
        return false;
    }
    let radius = if rect.w() >= 6.0 && rect.h() >= 6.0 { CORNER } else { 0.0 };
    window.paint_quad(fill(rect.bounds(), faded(color, alpha)).corner_radii(px(radius)));
    true
}

fn faded(color: Hsla, alpha: f32) -> Hsla {
    let mut color = color;
    color.alpha *= alpha;
    color
}

/// Paint a shaped line at `origin`, clipped to `clip`.
fn paint_line(line: &gpui::ShapedLine, origin: Point<Pixels>, line_height: f32, clip: Rect, window: &mut Window, cx: &mut App) {
    window.with_content_mask(Some(ContentMask { bounds: clip.bounds() }), |window| {
        let _ = line.paint(origin, px(line_height), TextAlign::Left, None, window, cx);
    });
}

/// Name and size on one line, the size quieter, fitted into `max_width`: the size goes
/// first when there isn't room for both.
#[allow(clippy::too_many_arguments)]
fn paint_name_and_size(name: &SharedString, size: &SharedString, font: &Font, ink: Hsla, origin: Point<Pixels>, max_width: f32, line_height: f32, clip: Rect, window: &mut Window, cx: &mut App) {
    const SPACE: f32 = 6.0;
    let quiet = faded(ink, 0.72);
    let size_line = (!size.is_empty()).then(|| fit_line(window, size, font, 12.0, quiet, f32::INFINITY)).flatten();
    let size_width = size_line.as_ref().map(|l| f32::from(l.width()) + SPACE).unwrap_or(0.0);
    let (name_line, with_size) = match fit_line(window, name, font, 12.0, ink, max_width - size_width) {
        Some(line) if size_line.is_some() && (f32::from(line.width()) >= 24.0 || line.text == *name) => (Some(line), true),
        _ => (fit_line(window, name, font, 12.0, ink, max_width), false),
    };
    let Some(name_line) = name_line else { return };
    paint_line(&name_line, origin, line_height, clip, window, cx);
    if let (true, Some(size_line)) = (with_size, size_line) {
        let at = point(origin.x + name_line.width() + px(SPACE), origin.y);
        paint_line(&size_line, at, line_height, clip, window, cx);
    }
}

/// Label the tiles `paint` drew: each one big enough gets its name, with its size below when
/// it's tall enough. `label` gives a tile's name and size by its index in the latest layout.
/// Labels are cut short with "…" to fit, and clipped to their tiles.
pub fn paint_labels(painted: &[PaintedTile], label: impl Fn(usize) -> Option<(SharedString, SharedString)>, window: &mut Window, cx: &mut App) {
    const PAD: f32 = 6.0;
    let regular = window.text_style().font();
    let medium = Font { weight: FontWeight::MEDIUM, ..regular.clone() };
    for tile in painted {
        let rect = Rect::of(tile.rect);
        if tile.alpha < 0.05 || rect.w() < 40.0 || rect.h() < 16.0 {
            continue;
        }
        let Some((name, size)) = label(tile.index) else { continue };
        let ink = label_color(tile.color, tile.alpha);
        let max_width = rect.w() - 2.0 * PAD;
        // Clipped to the tile, within the tile area (clamping to it is intersecting with it).
        let clip = rect.clamped(Rect::of(tile.clip));
        if let Some(line) = fit_line(window, &name, &medium, 12.0, ink, max_width) {
            paint_line(&line, point(px(rect.x0 + PAD), px(rect.y0 + 1.0)), 16.0, clip, window, cx);
        }
        if rect.h() >= 32.0
            && let Some(line) = fit_line(window, &size, &regular, 11.0, faded(ink, 0.8), max_width)
        {
            paint_line(&line, point(px(rect.x0 + PAD), px(rect.y0 + 16.0)), 14.0, clip, window, cx);
        }
    }
}

/// The focus folder's bar across the top of the chart (see `content_area`), in `background`
/// (the app lightens it while hovered), with the folder's name and size. Clicking it goes up a
/// level (`Hit::Center`); the app's label strip says where to.
pub fn paint_focus_bar(bar: Bounds<Pixels>, title: &SharedString, subtitle: &SharedString, background: Hsla, window: &mut Window, cx: &mut App) {
    let rect = Rect::of(bar);
    if !rect.visible() {
        return;
    }
    window.paint_quad(fill(bar, background).corner_radii(px(4.0)));
    let medium = Font { weight: FontWeight::MEDIUM, ..window.text_style().font() };
    let origin = point(px(rect.x0 + 8.0), px(rect.y0));
    paint_name_and_size(title, subtitle, &medium, label_color(background, 1.0), origin, rect.w() - 16.0, rect.h(), rect, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Node;
    use crate::sunburst::Target;
    use std::path::PathBuf;
    use std::time::Duration;

    /// root ── crate (60) ── src (59.9) ── win (59.9) ── x (30), y (29.9)
    ///     │                └ Cargo.toml (0.1)
    ///     ├ docs (39) ── guide (38.4), notes (0.6)
    ///     └ empty (1)
    fn chains() -> Tree {
        let mut nodes = Vec::new();
        let mut add = |name: &str, size: u64, kind: Kind, parent: Option<usize>| {
            nodes.push(Node { name: name.into(), size, kind, parent, children: Vec::new(), items: 1 });
            let ix = nodes.len() - 1;
            if let Some(p) = parent {
                nodes[p].children.push(ix);
            }
            ix
        };
        let root = add("root", 1000, Kind::Dir, None);
        let krate = add("crate", 600, Kind::Dir, Some(root));
        let src = add("src", 599, Kind::Dir, Some(krate));
        add("Cargo.toml", 1, Kind::File, Some(krate));
        let win = add("win", 599, Kind::Dir, Some(src));
        add("x", 300, Kind::Dir, Some(win));
        add("y", 299, Kind::Dir, Some(win));
        let docs = add("docs", 390, Kind::Dir, Some(root));
        add("guide", 384, Kind::Dir, Some(docs));
        add("notes", 6, Kind::File, Some(docs));
        add("empty", 10, Kind::Dir, Some(root));
        Tree { root_path: PathBuf::from("/"), nodes, errors: 0, cloud_only: 0 }
    }

    /// Opening a folder one folder fills goes on into it, as far as the chain goes; a folder
    /// with a real second box (here 1.5%), or nothing inside, opens as it is.
    #[test]
    fn opening_goes_through_folders_one_folder_fills() {
        let tree = chains();
        let name = |ix: usize| tree.nodes[ix].name.to_string();
        assert_eq!(name(opened(&tree, 1)), "win", "crate → src → win");
        assert_eq!(name(opened(&tree, 7)), "docs", "guide is only 98.5% of docs");
        assert_eq!(name(opened(&tree, 10)), "empty");
        assert_eq!(name(opened(&tree, 4)), "win", "already there");
    }

    /// Going up skips the same chains on the way back, to the first folder showing more than
    /// one box; it stops at the top whatever the top holds.
    #[test]
    fn going_up_skips_folders_one_folder_fills() {
        let tree = chains();
        let name = |ix: Option<usize>| ix.map(|ix| tree.nodes[ix].name.to_string());
        assert_eq!(name(enclosing(&tree, 4)), Some("root".into()), "win → past src and crate → root");
        assert_eq!(name(enclosing(&tree, 5)), Some("win".into()), "x → win, which holds two boxes");
        assert_eq!(name(enclosing(&tree, 8)), Some("docs".into()));
        assert_eq!(name(enclosing(&tree, 0)), None, "nothing above the top");
    }

    fn seg(start: f32, end: f32, depth: usize) -> Segment {
        Segment { target: Target::Node(0), depth, start, end, kind: Kind::Dir }
    }

    fn key(path: &str) -> Key {
        Key::Node(path.split('/').map(SharedString::from).collect())
    }

    /// The root's key, as the app makes it: no names.
    fn root_key() -> Key {
        Key::Node(Vec::new())
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn area() -> Bounds<Pixels> {
        rect(0.0, 0.0, 800.0, 600.0).bounds()
    }

    const EPS: f32 = 1e-3;

    fn inside(r: Rect, of: Rect) -> bool {
        r.x0 >= of.x0 - EPS && r.y0 >= of.y0 - EPS && r.x1 <= of.x1 + EPS && r.y1 <= of.y1 + EPS
    }

    fn overlap(a: Rect, b: Rect) -> f32 {
        (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0) * (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0)
    }

    fn sane(r: Rect) -> bool {
        [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) && r.w() >= 0.0 && r.h() >= 0.0
    }

    /// The tile's area as laid out, before the gap round it.
    fn slot_area(tile: Option<Bounds<Pixels>>) -> f32 {
        let r = Rect::of(tile.unwrap());
        (r.w() + 2.0 * GAP) * (r.h() + 2.0 * GAP)
    }

    /// Every top-level segment has a sane tile inside the area, tiles never overlap, and
    /// anything deeper has none.
    fn check_layout(segments: &[Segment], area: Bounds<Pixels>) -> Vec<Option<Bounds<Pixels>>> {
        let tiles = layout(segments, area);
        assert_eq!(tiles.len(), segments.len());
        for (i, tile) in tiles.iter().enumerate() {
            let Some(tile) = tile else {
                assert!(segments[i].depth > 1, "top-level segment {i} has a tile");
                continue;
            };
            assert_eq!(segments[i].depth, 1, "only the top level is shown, not {i}");
            let r = Rect::of(*tile);
            assert!(sane(r), "tile {i} is {r:?}");
            assert!(inside(r, Rect::of(area)), "tile {i} {r:?} inside the area");
            for (j, other) in tiles.iter().enumerate().skip(i + 1) {
                if let Some(other) = other {
                    let o = overlap(r, Rect::of(*other));
                    assert!(o <= EPS, "tiles {i} and {j} overlap by {o}");
                }
            }
        }
        tiles
    }

    #[test]
    fn slots_tile_the_rect_in_proportion() {
        let weights = [6.0, 4.0, 3.0, 2.0, 1.0, 1.0, 0.5];
        let within = rect(10.0, 20.0, 310.0, 220.0);
        let slots = squarify(&weights, 0.0, within);
        let total: f64 = weights.iter().sum();
        let whole = within.w() * within.h();
        let mut covered = 0.0;
        for (i, (slot, w)) in slots.iter().zip(weights).enumerate() {
            let share = slot.w() * slot.h() / whole;
            assert!((share as f64 - w / total).abs() <= 0.01 * w / total, "slot {i} has {share}, wants {}", w / total);
            assert!(inside(*slot, within));
            for other in &slots[i + 1..] {
                assert!(overlap(*slot, *other) <= EPS);
            }
            covered += slot.w() * slot.h();
        }
        assert!((covered - whole).abs() < 0.01, "the slots cover it all");
        // Squarified: nothing absurdly thin.
        assert!(slots.iter().all(|s| s.w().max(s.h()) / s.w().min(s.h()) < 4.0), "{slots:?}");
    }

    #[test]
    fn zero_weights_and_a_single_tile() {
        let within = rect(0.0, 0.0, 100.0, 50.0);
        let slots = squarify(&[0.0, 1.0, 0.0], 0.0, within);
        assert!(!slots[0].visible() && !slots[2].visible());
        assert_eq!(slots[1], within);
        assert_eq!(squarify(&[2.5], 0.0, within), [within]);
        assert!(squarify(&[0.0, 0.0], 0.0, within).iter().all(|s| !s.visible() && sane(*s)));
        assert!(squarify(&[1.0, 1.0], 0.0, rect(5.0, 5.0, 5.0, 5.0)).iter().all(|s| sane(*s)));

        // A folder holding just one thing: it fills the area, less the gap.
        let tiles = check_layout(&[seg(0.0, 1.0, 1)], area());
        assert_eq!(Rect::of(tiles[0].unwrap()), Rect::of(area()).gap(GAP));
    }

    #[test]
    fn one_level_fills_the_area_in_proportion() {
        let segments = [seg(0.0, 0.5, 1), seg(0.5, 0.8, 1), seg(0.8, 1.0, 1)];
        let tiles = check_layout(&segments, area());
        let whole = 800.0 * 600.0;
        for (i, s) in segments.iter().enumerate() {
            let share = slot_area(tiles[i]) / whole;
            assert!((share - (s.end - s.start)).abs() < 0.005, "tile {i} covers {share}");
        }
        // Deterministic.
        assert_eq!(layout(&segments, area()), tiles);
    }

    /// Segments deeper than the top level, as the sunburst's full layout has, are left out:
    /// the top level is laid out just as it would be alone.
    #[test]
    fn deeper_segments_get_no_tile() {
        let segments = [seg(0.0, 0.6, 1), seg(0.0, 0.3, 2), seg(0.0, 0.2, 3), seg(0.3, 0.6, 2), seg(0.6, 1.0, 1)];
        let tiles = check_layout(&segments, area());
        assert!(tiles[1].is_none() && tiles[2].is_none() && tiles[3].is_none());
        let alone = layout(&[seg(0.0, 0.6, 1), seg(0.6, 1.0, 1)], area());
        assert_eq!((tiles[0], tiles[4]), (alone[0], alone[1]));
    }

    #[test]
    fn random_layouts_are_sound() {
        // A small LCG, so the "random" folders are the same every run.
        let mut seed = 0x2545_f491_u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as f32 / (1u64 << 31) as f32
        };
        let areas = [area(), rect(0.0, 0.0, 2000.0, 3.0).bounds(), rect(5.0, 5.0, 8.0, 1505.0).bounds(), rect(0.0, 0.0, 0.0, 0.0).bounds()];
        for (k, area) in areas.into_iter().enumerate() {
            for _ in 0..20 {
                // Up to 40 things of very different sizes, some with (ignored) contents.
                let n = (next() * 40.0) as usize;
                let weights: Vec<f32> = (0..n).map(|_| next().powi(3)).collect();
                let total: f32 = weights.iter().sum::<f32>().max(1e-6);
                let mut segments = Vec::new();
                let mut at = 0.0;
                for w in weights {
                    let width = w / total;
                    segments.push(seg(at, at + width, 1));
                    if next() < 0.3 {
                        segments.push(seg(at, at + width / 2.0, 2));
                    }
                    at += width;
                }
                let tiles = check_layout(&segments, area);
                if k == 0 {
                    let covered: f32 = tiles.iter().flatten().map(|t| Rect::of(*t).w() * Rect::of(*t).h()).sum();
                    assert!(segments.is_empty() || covered > 0.9 * 800.0 * 600.0, "the tiles fill the area");
                }
            }
        }
    }

    #[test]
    fn what_a_folder_doesnt_show_is_left_empty() {
        // A weight of 3 in a rect worth 4 covers three quarters of it, leaving the rest.
        let within = rect(0.0, 0.0, 100.0, 100.0);
        let slots = squarify(&[2.0, 1.0], 4.0, within);
        let share = |r: Rect| r.w() * r.h() / (100.0 * 100.0);
        assert!((share(slots[0]) - 0.5).abs() < 0.005 && (share(slots[1]) - 0.25).abs() < 0.005, "{slots:?}");
        // Shares that only miss by rounding still fill it.
        let slots = squarify(&[1.0, 1.0], 2.0005, within);
        assert!((share(slots[0]) + share(slots[1]) - 1.0).abs() < 1e-4);

        // A folder showing 0.45 and 0.3 of itself (the rest too small to show): its tiles
        // cover three quarters of the area, not all of it.
        let tiles = check_layout(&[seg(0.0, 0.45, 1), seg(0.45, 0.75, 1)], area());
        let covered = (slot_area(tiles[0]) + slot_area(tiles[1])) / (800.0 * 600.0);
        assert!((covered - 0.75).abs() < 0.0075, "covers {covered}");
    }

    #[test]
    fn hit_test_finds_the_tile_under_the_pointer() {
        let segments = [seg(0.0, 0.6, 1), seg(0.0, 0.4, 2), seg(0.6, 1.0, 1)];
        let (bar, tiles_area) = content_area(rect(0.0, 0.0, 860.0, 700.0).bounds());
        let tiles = layout(&segments, tiles_area);
        let middle = |r: Bounds<Pixels>| {
            let (x, y) = Rect::of(r).center();
            point(px(x), px(y))
        };
        // A deeper segment passed in is never hit: the middle of the folder's tile is the folder.
        assert_eq!(hit_test(&tiles, bar, middle(tiles[0].unwrap())), Some(Hit::Segment(0)));
        assert_eq!(hit_test(&tiles, bar, middle(tiles[2].unwrap())), Some(Hit::Segment(2)));
        assert_eq!(hit_test(&tiles, bar, middle(bar)), Some(Hit::Center));
        assert_eq!(hit_test(&tiles, bar, point(px(4.0), px(4.0))), None);
        assert_eq!(hit_test(&tiles, bar, point(px(2000.0), px(300.0))), None);
    }

    /// Step until settled, checking `each` on every frame. Returns the frame count.
    fn run(motion: &mut TreemapMotion, t: &mut Instant, mut each: impl FnMut(&TreemapMotion)) -> usize {
        let mut frames = 0;
        while motion.step(*t) && frames < 600 {
            each(motion);
            *t += Duration::from_millis(16);
            frames += 1;
        }
        frames
    }

    fn now(motion: &TreemapMotion, path: &str) -> Rect {
        motion.tiles[&key(path)].rect()
    }

    fn close(a: Rect, b: Rect) -> bool {
        [(a.x0, b.x0), (a.y0, b.y0), (a.x1, b.x1), (a.y1, b.y1)].iter().all(|(p, q)| (p - q).abs() < 0.05)
    }

    fn at_targets(motion: &TreemapMotion) -> bool {
        motion.keys.iter().zip(&motion.layout).all(|(k, tile)| tile.is_none_or(|tile| close(motion.tiles[k].rect(), Rect::of(tile))))
    }

    fn root() -> (Vec<Key>, Vec<Segment>) {
        // a (60%), b (40%)
        (vec![key("a"), key("b")], vec![seg(0.0, 0.6, 1), seg(0.6, 1.0, 1)])
    }

    /// Inside "a": x (half of it), y (half).
    fn in_a() -> (Vec<Key>, Vec<Segment>) {
        (vec![key("a/x"), key("a/y")], vec![seg(0.0, 0.5, 1), seg(0.5, 1.0, 1)])
    }

    #[test]
    fn the_first_layout_is_revealed_biggest_first_and_settles() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        // Listed smallest first, so it's the sizes that set the order, not the listing.
        let keys = [key("small"), key("big"), key("middle")];
        motion.retarget(1, &keys, &[seg(0.0, 0.1, 1), seg(0.1, 0.7, 1), seg(0.7, 1.0, 1)], area(), 1.0);
        let mut seen_stagger = false;
        let frames = run(&mut motion, &mut t, |m| {
            let placed = m.placed();
            let alpha = |i: usize| placed.iter().find(|p| p.index == Some(i)).unwrap().alpha;
            seen_stagger |= alpha(1) > 0.2 && alpha(0) < alpha(1) - 0.1;
            assert!(alpha(0) <= alpha(2) + 1e-4 && alpha(2) <= alpha(1) + 1e-4, "the biggest comes in first");
        });
        assert!(seen_stagger, "the smaller ones follow on");
        assert!(frames <= 60, "revealed within ~0.9 s, took {frames} frames");
        assert!(at_targets(&motion) && motion.reveal.is_none());
    }

    #[test]
    fn a_grown_layout_and_a_departed_tile_settle() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        motion.retarget(1, &[key("a"), key("b"), key("c")], &[seg(0.0, 0.5, 1), seg(0.5, 0.8, 1), seg(0.8, 1.0, 1)], area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        // "b" leaves, "d" arrives and "a" grows.
        let keys = [key("a"), key("c"), key("d")];
        motion.retarget(2, &keys, &[seg(0.0, 0.6, 1), seg(0.6, 0.8, 1), seg(0.8, 1.0, 1)], area(), 1.0);
        let gone = motion.tiles[&key("b")].target();
        assert!(gone.w() == 0.0 && gone.h() == 0.0, "the departed tile shrinks to a point");
        let d = now(&motion, "d");
        assert!(d.w() == 0.0 && d.h() == 0.0 && motion.tiles[&key("d")].alpha.x == 0.0, "the new tile grows from its centre");
        // On its way out, it's beneath everything that's staying.
        t += Duration::from_millis(16);
        motion.step(t);
        assert_eq!(motion.placed()[0].index, None);
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75, "settles within ~1 s, took {frames} frames");
        assert!(!motion.tiles.contains_key(&key("b")), "the departed tile is removed");
        assert!(at_targets(&motion));
    }

    #[test]
    fn zooming_in_and_out_moves_the_chart_as_one() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        let old_a = Rect::of(motion.layout[0].unwrap());

        // Into "a": it opens out to fill the area, its contents fading in over it, and "b" is
        // pushed off past the edge.
        motion.zoom(root_key(), key("a"));
        let (keys, segments) = in_a();
        motion.retarget(2, &keys, &segments, area(), 1.0);
        assert!(!motion.folding(), "zoomed, not cross-faded");
        assert!(inside(now(&motion, "a/x"), old_a) && inside(now(&motion, "a/y"), old_a), "its contents start inside it");
        let a = motion.tiles[&key("a")].target();
        assert!(close(a, Rect::of(area())), "a opens out to fill the area: {a:?}");
        let b = motion.tiles[&key("b")].target();
        assert!(b.x0 >= 800.0 - EPS || b.y0 >= 600.0 - EPS, "b flies off past the edge: {b:?}");
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75, "took {frames} frames");
        assert!(!motion.tiles.contains_key(&key("a")) && !motion.tiles.contains_key(&key("b")));
        assert!(at_targets(&motion));

        // And back out: the chart shrinks into "a", which fades in over it, and "b" comes
        // back in from outside.
        motion.zoom(key("a"), root_key());
        let (keys, segments) = root();
        motion.retarget(3, &keys, &segments, area(), 1.0);
        let (a, b) = (now(&motion, "a"), now(&motion, "b"));
        assert!(a.x0 <= 0.0 + EPS && a.x1 >= 800.0 - EPS, "a starts out filling the area: {a:?}");
        assert!(b.x0 >= 800.0 - EPS || b.y0 >= 600.0 - EPS, "b starts outside: {b:?}");
        let x = motion.tiles[&key("a/x")].target();
        assert!(inside(x, Rect::of(motion.layout[0].unwrap())), "a's contents shrink into it: {x:?}");
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75, "took {frames} frames");
        assert!(!motion.tiles.contains_key(&key("a/x")));
        assert!(at_targets(&motion));
    }

    /// How opaque the chart is at a point: every tile over it, layered.
    fn coverage(motion: &TreemapMotion, x: f32, y: f32) -> f32 {
        1.0 - motion.placed().iter().filter(|p| p.rect.contains(x, y)).map(|p| 1.0 - p.alpha).product::<f32>()
    }

    /// Zooming in or out never lets the backdrop show through: what leaves stays opaque
    /// beneath what arrives until that has all but faded in.
    #[test]
    fn zooming_never_shows_the_backdrop_through_the_folder() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});

        motion.zoom(root_key(), key("a"));
        let (keys, segments) = in_a();
        motion.retarget(2, &keys, &segments, area(), 1.0);
        let (x, y) = Rect::of(motion.layout[0].unwrap()).center();
        let mut worst = f32::INFINITY;
        let frames = run(&mut motion, &mut t, |m| worst = worst.min(coverage(m, x, y)));
        assert!(worst >= HOLD_UNTIL - 0.01, "zooming in, the chart is only {worst} opaque at its thinnest");
        assert!(frames < 75 && !motion.tiles.contains_key(&key("a")), "in took {frames} frames");

        motion.zoom(key("a"), root_key());
        let (keys, segments) = root();
        motion.retarget(3, &keys, &segments, area(), 1.0);
        let (x, y) = motion.tiles[&key("a/x")].target().center();
        let mut worst = f32::INFINITY;
        let frames = run(&mut motion, &mut t, |m| worst = worst.min(coverage(m, x, y)));
        assert!(worst >= HOLD_UNTIL - 0.01, "zooming out, the chart is only {worst} opaque at its thinnest");
        assert!(frames < 75 && !motion.tiles.contains_key(&key("a/x")), "out took {frames} frames");
    }

    /// Jumping more than one level (along the breadcrumbs, say) zooms by the folder on the
    /// way that's on screen, rather than cross-fading.
    #[test]
    fn a_jump_of_several_levels_zooms_by_the_folder_on_the_way() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        let old_a = Rect::of(motion.layout[0].unwrap());

        motion.zoom(root_key(), key("a/x/p"));
        motion.retarget(2, &[key("a/x/p/1"), key("a/x/p/2")], &[seg(0.0, 0.7, 1), seg(0.7, 1.0, 1)], area(), 1.0);
        assert!(!motion.folding(), "zoomed by a, not cross-faded");
        assert!(inside(now(&motion, "a/x/p/1"), old_a), "what's new starts inside a");
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75 && at_targets(&motion), "in took {frames} frames");

        // And straight back up to the top: the chart shrinks into a.
        motion.zoom(key("a/x/p"), root_key());
        motion.retarget(3, &keys, &segments, area(), 1.0);
        assert!(!motion.folding(), "zoomed out by a, not cross-faded");
        assert!(inside(motion.tiles[&key("a/x/p/1")].target(), Rect::of(motion.layout[0].unwrap())));
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75 && at_targets(&motion), "out took {frames} frames");
    }

    #[test]
    fn a_lateral_jump_cross_fades() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = in_a();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        // From inside a/x to inside b/y: neither is on screen in the other's chart.
        motion.zoom(key("a/x"), key("b/y"));
        motion.retarget(2, &[key("b/y/1")], &[seg(0.0, 1.0, 1)], area(), 1.0);
        assert!(motion.folding() && motion.reveal.is_some());
        run(&mut motion, &mut t, |_| {});
        assert!(!motion.folding() && at_targets(&motion));
    }

    #[test]
    fn the_scan_fades_away_as_the_results_are_revealed() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 0.5);
        run(&mut motion, &mut t, |_| {});
        motion.fold();
        assert!(motion.folding() && motion.fade.as_ref().unwrap().ghosts.len() == 2);
        let ghost = motion.fade.as_ref().unwrap().ghosts[0].rect;
        assert!(ghost.x1 <= 400.0 + EPS, "the ghosts are as drawn, squeezed by the fraction");
        let keys = [key("b"), key("a")];
        let layout = [seg(0.0, 0.4, 1), seg(0.4, 1.0, 1)];
        let mut frames = 0;
        loop {
            motion.retarget(2, &keys, &layout, area(), 1.0);
            if !motion.step(t) || frames > 600 {
                break;
            }
            t += Duration::from_millis(16);
            frames += 1;
        }
        assert!(!motion.folding() && at_targets(&motion));
        assert!((motion.fraction() - 1.0).abs() < 1e-3);
        assert!(frames < 90, "done within ~1.5 s, took {frames} frames");
    }

    #[test]
    fn folding_into_an_empty_chart_still_ends() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        motion.retarget(1, &[key("a")], &[seg(0.0, 1.0, 1)], area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.fold();
        let mut frames = 0;
        loop {
            motion.retarget(2, &[], &[], area(), 1.0);
            if !motion.step(t) || frames > 600 {
                break;
            }
            t += Duration::from_millis(16);
            frames += 1;
        }
        assert!(!motion.folding() && frames < 60, "the fold ends, after {frames} frames");
    }

    #[test]
    fn resizing_carries_the_chart_along() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        let bigger = rect(0.0, 0.0, 1600.0, 1200.0).bounds();
        motion.retarget(1, &keys, &segments, bigger, 1.0);
        // Twice the size, carried along at once: only the fixed-size gaps are left to settle.
        let off = motion.keys.iter().enumerate().map(|(i, k)| {
            let (r, t) = (motion.tiles[k].rect(), Rect::of(motion.layout[i].unwrap()));
            [r.x0 - t.x0, r.y0 - t.y0, r.x1 - t.x1, r.y1 - t.y1].iter().fold(0.0f32, |m, d| m.max(d.abs()))
        });
        assert!(off.fold(0.0, f32::max) < 4.0);
        assert_eq!(motion.tiles(), layout(&segments, bigger).as_slice());
        run(&mut motion, &mut t, |_| {});
        assert!(at_targets(&motion));
    }

    #[test]
    fn a_pending_zoom_waits_for_the_next_layout() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.zoom(root_key(), key("a"));
        // The window is resized before the zoomed layout arrives.
        let wider = rect(0.0, 0.0, 1000.0, 600.0).bounds();
        motion.retarget(1, &keys, &segments, wider, 1.0);
        assert!(motion.zoom.is_some(), "a resize doesn't use the zoom up");
        let old_a = motion.tiles[&key("a")].target();
        let (keys, segments) = in_a();
        motion.retarget(2, &keys, &segments, wider, 1.0);
        assert!(motion.zoom.is_none() && !motion.folding());
        assert!(inside(now(&motion, "a/x"), old_a), "the camera was applied");
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75 && at_targets(&motion), "took {frames} frames");
    }

    #[test]
    fn retargeting_mid_flight_keeps_position_and_speed() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.retarget(2, &keys, &[seg(0.0, 0.3, 1), seg(0.3, 1.0, 1)], area(), 1.0);
        for _ in 0..6 {
            motion.step(t);
            t += Duration::from_millis(16);
        }
        let before: Vec<(Rect, [f32; 4])> = keys.iter().map(|k| (motion.tiles[k].rect(), motion.tiles[k].edges.map(|e| e.v))).collect();
        assert!(before.iter().any(|(_, v)| v.iter().any(|v| v.abs() > 1.0)), "something is moving");
        motion.retarget(3, &keys, &[seg(0.0, 0.5, 1), seg(0.5, 1.0, 1)], area(), 1.0);
        for (k, (rect, v)) in keys.iter().zip(before) {
            assert_eq!(motion.tiles[k].rect(), rect);
            assert_eq!(motion.tiles[k].edges.map(|e| e.v), v);
        }
        run(&mut motion, &mut t, |_| {});
        assert!(at_targets(&motion));
    }

    /// Segments too deep to show, passed anyway, never get a tile in motion: they're never
    /// painted, and a tile of the same key on its way out isn't mistaken for one.
    #[test]
    fn deeper_segments_are_never_placed() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let keys = [key("a"), key("a/x"), key("b")];
        let segments = [seg(0.0, 0.6, 1), seg(0.0, 0.3, 2), seg(0.6, 1.0, 1)];
        motion.retarget(1, &keys, &segments, area(), 1.0);
        let frames = run(&mut motion, &mut t, |m| assert!(m.placed().iter().all(|p| p.index != Some(1))));
        assert!(frames <= 60 && at_targets(&motion));
        assert!(!motion.tiles.contains_key(&key("a/x")) && motion.tiles().len() == 3 && motion.tiles()[1].is_none());

        // "a/x" was a top-level tile (inside "a"); going up, it's deeper and on its way out.
        let (inside_keys, inside_segments) = in_a();
        motion.zoom(root_key(), key("a"));
        motion.retarget(2, &inside_keys, &inside_segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.zoom(key("a"), root_key());
        motion.retarget(3, &keys, &segments, area(), 1.0);
        assert!(motion.tiles[&key("a/x")].current.is_none(), "on its way out, not a tile of the layout");
        run(&mut motion, &mut t, |m| assert!(m.placed().iter().all(|p| p.index != Some(1))));
        assert!(!motion.tiles.contains_key(&key("a/x")) && at_targets(&motion));
    }

    #[test]
    fn zooming_into_a_sliver_stays_near_the_chart() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        // A folder of half a percent, laid out as a thin column down the side.
        let keys = [key("big"), key("thin")];
        let segments = [seg(0.0, 0.995, 1), seg(0.995, 1.0, 1)];
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        let thin = Rect::of(motion.layout[1].unwrap());
        assert!(thin.w() < 10.0 && thin.h() > 500.0, "{thin:?}");
        let bounds = Rect::of(area()).surroundings();
        let near = |m: &TreemapMotion| {
            for p in m.placed().iter().filter(|p| p.alpha >= 0.01 && p.rect.visible()) {
                assert!(inside(p.rect, bounds), "{:?} strays from the chart", p.rect);
            }
        };

        motion.zoom(root_key(), key("thin"));
        motion.retarget(2, &[key("thin/a"), key("thin/b")], &[seg(0.0, 0.6, 1), seg(0.6, 1.0, 1)], area(), 1.0);
        assert!(!motion.folding(), "zoomed, not cross-faded");
        near(&motion);
        let frames = run(&mut motion, &mut t, near);
        assert!(frames < 75 && at_targets(&motion), "in took {frames} frames");

        motion.zoom(key("thin"), root_key());
        motion.retarget(3, &keys, &segments, area(), 1.0);
        near(&motion);
        let frames = run(&mut motion, &mut t, near);
        assert!(frames < 75 && at_targets(&motion), "out took {frames} frames");
    }

    #[test]
    fn new_tiles_hues_move_with_the_camera() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        // Into "a" (0 to 0.6 turns): what's new starts at its turns within a's.
        motion.zoom(root_key(), key("a"));
        let (inside_keys, inside_segments) = in_a();
        motion.retarget(2, &inside_keys, &inside_segments, area(), 1.0);
        let x = &motion.tiles[&key("a/x")].turns;
        assert!(x[0].x.abs() < 1e-5 && (x[1].x - 0.3).abs() < 1e-5 && (x[1].target - 0.5).abs() < 1e-5);
        run(&mut motion, &mut t, |_| {});
        // Back out: "b" comes in from past the end of the turns "a" covered.
        motion.zoom(key("a"), root_key());
        motion.retarget(3, &keys, &segments, area(), 1.0);
        let b = &motion.tiles[&key("b")].turns;
        assert!((b[0].x - 1.0).abs() < 1e-5 && (b[1].x - 1.0).abs() < 1e-5 && (b[0].target - 0.6).abs() < 1e-5);
    }

    #[test]
    fn folding_lasts_until_the_results_are_revealed() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        assert!(!motion.folding(), "the first reveal isn't a fold");
        motion.fold();
        let mut revealing_after_fade = false;
        loop {
            motion.retarget(2, &keys, &segments, area(), 1.0);
            if !motion.step(t) {
                break;
            }
            if motion.reveal.is_some() {
                assert!(motion.folding(), "still folding while the results are revealed");
                revealing_after_fade |= motion.fade.is_none();
            }
            t += Duration::from_millis(16);
        }
        assert!(revealing_after_fade && !motion.folding());
    }
}
