//! Treemap: the same segments as the sunburst, drawn as nested rectangles whose areas are
//! their sizes. Each folder's contents are laid out inside it, under a header strip naming
//! it when there's room, so the chart reads as boxes within boxes.
//!
//! It takes the sunburst's `Vec<Segment>` unchanged (a segment's share of the focus folder,
//! `end - start`, is its weight), so colours, keys, hover, categories and the legend work
//! the same whichever chart is showing.
//!
//! Motion follows `motion.rs`: every edge of every tile follows the same critically damped
//! spring, and tiles are matched across layouts by `Key`, so the picture morphs as one piece.
//! - The first layout is revealed by depth: the top level grows in first, each level inside
//!   following a moment later.
//! - A folder's new contents slide out from under it, moving as it moves.
//! - Tiles that are gone shrink away where they were, fading.
//! - Changing the folder in focus moves a camera: one affine map for the whole chart, so the
//!   folder opens out to fill the chart (or, going up, the chart shrinks back into it).
//! - When the scan finishes, its chart fades away while the results are revealed in its place
//!   (`fold`); the scan's tiles are in first-seen order and the results' largest first, so
//!   morphing between them would only be a shuffle.

#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Instant;

use gpui::{App, Bounds, ContentMask, Font, FontWeight, Hsla, Pixels, Point, SharedString, TextAlign, TextRun, Window, fill, hsla, point, px, size};
use palette::IntoColor;

use crate::motion::{Key, Spring, smootherstep};
use crate::sunburst::{Hit, Segment};

/// Room above the chart for the app's hover label strip.
const TOP_MARGIN: f32 = 52.0;
const SIDE_MARGIN: f32 = 16.0;
/// The focus folder's bar across the top of the chart, and the gap under it.
const FOCUS_BAR: f32 = 22.0;
const FOCUS_GAP: f32 = 3.0;
/// A folder at least this big gets a header strip naming it, with its contents below.
const HEADER: f32 = 18.0;
const HEADER_MIN_WIDTH: f32 = 48.0;
const HEADER_MIN_HEIGHT: f32 = 40.0;
/// Frame of folder colour left round a folder's contents (under a header).
const FRAME: f32 = 2.0;
/// Below this a folder is too small to show anything inside it.
const NEST_MIN: f32 = 12.0;
/// Each tile is inset by this inside its slot, so siblings stand apart.
const GAP: f32 = 1.0;
const CORNER: f32 = 2.0;
/// Revealing the first layout: each level grows in over `REVEAL_EACH`, starting
/// `REVEAL_STAGGER` after the one outside it, from this share of its size.
const REVEAL_EACH: f32 = 0.4;
const REVEAL_STAGGER: f32 = 0.1;
const REVEAL_SECONDS: f32 = 0.9;
const REVEAL_FROM: f32 = 0.6;
/// How long the chart as drawn takes to fade away in a fold.
const FADE_SECONDS: f32 = 0.35;

/// Where a segment goes in the treemap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub rect: Bounds<Pixels>,
    /// Where its contents are laid out, when it has any and there's room for them.
    pub inner: Option<Bounds<Pixels>>,
    /// Whether it has a header strip (the top `HEADER` px of `rect`) naming it.
    pub header: bool,
}

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

/// Lay the segments out as a nested, squarified treemap filling `area`, one tile per
/// segment. Each folder's children are laid out in the order given (the results are
/// largest first; the scan's chart is in first-seen order, which keeps its tiles fairly
/// stable as it grows), with areas in proportion to their share.
pub fn layout(segments: &[Segment], area: Bounds<Pixels>) -> Vec<Tile> {
    let area = Rect::of(area).valid();
    let parents = parents(segments);
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); segments.len()];
    let mut top = Vec::new();
    for (i, parent) in parents.iter().enumerate() {
        match parent {
            Some(p) => children[*p].push(i),
            None => top.push(i),
        }
    }
    let (cx, cy) = area.center();
    let mut placed = vec![(Rect::point(cx, cy), None::<Rect>, false); segments.len()];
    let place = |kids: &[usize], within: Rect, placed: &mut Vec<(Rect, Option<Rect>, bool)>| {
        let weights: Vec<f64> = kids.iter().map(|&k| (segments[k].end - segments[k].start).max(0.0) as f64).collect();
        for (&k, slot) in kids.iter().zip(squarify(&weights, within)) {
            let rect = slot.gap(GAP);
            let (inner, header) = if children[k].is_empty() {
                (None, false)
            } else if rect.w() >= HEADER_MIN_WIDTH && rect.h() >= HEADER_MIN_HEIGHT {
                (Some(Rect { x0: rect.x0 + FRAME, y0: rect.y0 + HEADER, x1: rect.x1 - FRAME, y1: rect.y1 - FRAME }), true)
            } else if rect.w() >= NEST_MIN && rect.h() >= NEST_MIN {
                (Some(rect.gap(GAP)), false)
            } else {
                (None, false)
            };
            placed[k] = (rect, inner, header);
        }
    };
    place(&top, area, &mut placed);
    // Parents come before their children, so each folder is placed by the time we get to it.
    for i in 0..segments.len() {
        match placed[i] {
            (_, Some(inner), _) => place(&children[i], inner, &mut placed),
            (rect, None, _) => {
                // No room: its contents collapse to its centre and aren't drawn.
                let (cx, cy) = rect.center();
                for &k in &children[i] {
                    placed[k] = (Rect::point(cx, cy), None, false);
                }
            }
        }
    }
    placed.into_iter().map(|(rect, inner, header)| Tile { rect: rect.bounds(), inner: inner.map(Rect::bounds), header }).collect()
}

/// Each segment's parent, by index. Layouts are depth-first, so it's the nearest segment
/// before it that's one level up.
fn parents(segments: &[Segment]) -> Vec<Option<usize>> {
    let mut stack: Vec<usize> = Vec::new();
    segments
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            while stack.last().is_some_and(|&s| segments[s].depth >= segment.depth) {
                stack.pop();
            }
            let parent = stack.last().copied();
            stack.push(i);
            parent
        })
        .collect()
}

/// Squarified treemap (Bruls, Huizing and van Wijk): split `rect` into one slot per weight,
/// in order, with areas in proportion to the weights, keeping slots as square as it can.
/// Rows are laid along the shorter side and grown while that improves their worst aspect
/// ratio. Zero weights get an empty slot at the centre. Linear in the number of weights.
fn squarify(weights: &[f64], rect: Rect) -> Vec<Rect> {
    let (cx, cy) = rect.center();
    let mut out = vec![Rect::point(cx, cy); weights.len()];
    let items: Vec<usize> = (0..weights.len()).filter(|&i| weights[i] > 0.0 && weights[i].is_finite()).collect();
    let total: f64 = items.iter().map(|&i| weights[i]).sum();
    let (width, height) = (rect.w() as f64, rect.h() as f64);
    if total <= 0.0 || width <= 0.0 || height <= 0.0 {
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
    out
}

/// The deepest tile under `position` (tiles are depth-first and siblings never overlap, so
/// that's the last one containing it), or the focus bar as `Hit::Center`.
pub fn hit_test(tiles: &[Tile], focus_bar: Bounds<Pixels>, position: Point<Pixels>) -> Option<Hit> {
    let (x, y) = (f32::from(position.x), f32::from(position.y));
    if Rect::of(focus_bar).contains(x, y) {
        return Some(Hit::Center);
    }
    tiles.iter().rposition(|tile| Rect::of(tile.rect).contains(x, y)).map(Hit::Segment)
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

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    fn visible(&self) -> bool {
        self.w() >= 0.5 && self.h() >= 0.5
    }
}

/// A map of the plane that scales and shifts each axis: x → x × sx + ox, y → y × sy + oy.
/// The camera, and the way a revealing folder carries its contents along.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    sx: f32,
    ox: f32,
    sy: f32,
    oy: f32,
}

impl Affine {
    const IDENTITY: Self = Self { sx: 1.0, ox: 0.0, sy: 1.0, oy: 0.0 };

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

/// One tile on its way to (or away from) its place in the latest layout.
struct TileMotion {
    /// Left, top, right and bottom.
    edges: [Spring; 4],
    /// Its start and end in turns, so hues taken from them follow the motion.
    turns: [Spring; 2],
    alpha: Spring,
    /// Where its contents go in the latest layout it was part of (the camera zooms into it).
    inner: Option<Rect>,
    depth: usize,
    /// Index into the latest layout; `None` once the tile has left it.
    current: Option<usize>,
    /// Colour last drawn with, for tiles that are on their way out.
    color: Hsla,
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
        self.inner = self.inner.map(|r| map.apply(r));
    }
}

fn still(r: Rect) -> [Spring; 4] {
    [Spring::still(r.x0, r.x0), Spring::still(r.y0, r.y0), Spring::still(r.x1, r.x1), Spring::still(r.y1, r.y1)]
}

/// Springs starting at `from`, headed for `to`.
fn moving(from: Rect, to: Rect) -> [Spring; 4] {
    [Spring::still(from.x0, to.x0), Spring::still(from.y0, to.y0), Spring::still(from.x1, to.x1), Spring::still(from.y1, to.y1)]
}

/// Springs for `target`, which lies in `source` (the parent's target inner rect), starting
/// where it falls when `source` is stretched over the parent as drawn and moving as the
/// parent moves: the 2-D version of `along` in `motion.rs`.
fn along(parent: &[Spring; 4], source: Rect, target: Rect) -> [Spring; 4] {
    let edge = |at: f32, lo: f32, size: f32, a: &Spring, b: &Spring| {
        let t = if size > 1e-3 { (at - lo) / size } else { 0.5 };
        Spring { x: a.x + (b.x - a.x) * t, v: a.v + (b.v - a.v) * t, target: at }
    };
    let x = |at: f32| edge(at, source.x0, source.w(), &parent[0], &parent[2]);
    let y = |at: f32| edge(at, source.y0, source.h(), &parent[1], &parent[3]);
    [x(target.x0), y(target.y0), x(target.x1), y(target.y1)]
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
    header: bool,
    leaf: bool,
}

/// A tile of the latest layout as `TreemapMotion::paint` drew it, for its label.
#[derive(Clone, Copy, Debug)]
pub struct PaintedTile {
    /// Index into the latest layout's segments.
    pub index: usize,
    pub rect: Bounds<Pixels>,
    pub alpha: f32,
    /// Whether it has a header strip naming it (its contents are drawn below that).
    pub header: bool,
    /// Whether nothing is drawn inside it, so a label on it would be seen.
    pub leaf: bool,
}

/// Motion for the treemap; see the module docs. The app calls `retarget` and `step` each
/// frame, then `paint`, then `paint_labels` with what was painted, and hit tests against
/// `tiles()`. `paint_focus_bar` draws the focus folder's bar, separately, since the app
/// knows its title and whether it's hovered.
pub struct TreemapMotion {
    tiles: HashMap<Key, TileMotion>,
    /// The latest layout, with its keys and each segment's parent and depth.
    layout: Vec<Tile>,
    keys: Vec<Key>,
    parents: Vec<Option<usize>>,
    depths: Vec<usize>,
    /// The area the tiles fill.
    area: Option<Rect>,
    last_frame: Option<Instant>,
    /// Which layout the targets come from, to retarget only on change.
    layout_id: usize,
    /// Share of the area the chart covers, across (volume scans fill it as they go).
    fraction: Spring,
    /// Applied to the next layout: the focus changed, from one folder to another (`None`:
    /// the root).
    zoom: Option<(Option<Key>, Option<Key>)>,
    /// Seconds into revealing the first layout; `None` once done (or not started).
    reveal: Option<f32>,
    revealed: bool,
    fade: Option<Fade>,
}

impl Default for TreemapMotion {
    fn default() -> Self {
        Self {
            tiles: HashMap::new(),
            layout: Vec::new(),
            keys: Vec::new(),
            parents: Vec::new(),
            depths: Vec::new(),
            area: None,
            last_frame: None,
            layout_id: 0,
            fraction: Spring::still(1.0, 1.0),
            zoom: None,
            reveal: None,
            revealed: false,
            fade: None,
        }
    }
}

impl TreemapMotion {
    /// The next layout is centred on a different folder: `from` is the old focus and `to`
    /// the new one (`None`: the root). Move there as a camera would.
    pub fn zoom(&mut self, from: Option<Key>, to: Option<Key>) {
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
        self.tiles.clear();
        self.layout.clear();
        self.keys.clear();
        self.parents.clear();
        self.depths.clear();
        self.zoom = None;
        self.reveal = None;
        self.layout_id = 0;
    }

    /// Whether the chart is folding: what's drawn isn't where things are, so it shouldn't
    /// respond to the pointer.
    pub fn folding(&self) -> bool {
        self.fade.is_some()
    }

    pub fn fraction(&self) -> f32 {
        self.fraction.x
    }

    /// The latest layout's tiles, where they're headed, for hit testing. (Not squeezed by
    /// `fraction`: only the scan's chart, which isn't interactive, covers less than all.)
    pub fn tiles(&self) -> &[Tile] {
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

        // The camera: one map from the old chart to the new one.
        let mut camera = None;
        if let Some((from, to)) = self.zoom.take().filter(|_| new_layout) {
            // Zooming in: the folder's contents (in the old chart) open out to fill the area.
            let into = to.as_ref().and_then(|k| self.tiles.get(k)).filter(|m| m.current.is_some()).map(|m| m.inner.filter(Rect::visible).unwrap_or(m.target()));
            camera = into.and_then(|r| Affine::between(r, area_rect));
            if camera.is_none() {
                // Zooming out: the whole old chart shrinks into the folder it showed.
                let out_of = from.as_ref().and_then(|k| keys.iter().position(|key| key == k)).map(|i| {
                    let tile = &layout[i];
                    tile.inner.map(Rect::of).filter(Rect::visible).unwrap_or(Rect::of(tile.rect))
                });
                camera = out_of.and_then(|r| Affine::between(area_rect, r));
            }
            if camera.is_none() {
                // Neither folder is in the other's chart: cross-fade.
                self.fold();
            }
        }
        self.layout_id = layout_id;
        // In place straight away: the very first layout, and the one after a fold, both
        // revealed by depth.
        let first = !self.revealed || (self.fade.is_some() && self.tiles.is_empty());
        if first && new_layout && !segments.is_empty() {
            self.reveal = Some(0.0);
        }
        self.revealed |= !segments.is_empty();
        if let Some(fade) = &mut self.fade {
            fade.opened = true;
        }
        let parents = parents(segments);

        for motion in self.tiles.values_mut() {
            motion.current = None;
        }
        for (i, (key, segment)) in keys.iter().zip(segments).enumerate() {
            let tile = layout[i];
            let (target, inner) = (Rect::of(tile.rect), tile.inner.map(Rect::of));
            if let Some(motion) = self.tiles.get_mut(key) {
                motion.aim(target);
                motion.turns[0].target = segment.start;
                motion.turns[1].target = segment.end;
                motion.alpha.target = 1.0;
                motion.inner = inner;
                motion.depth = segment.depth;
                motion.current = Some(i);
                continue;
            }
            let (edges, alpha) = if first {
                // The reveal brings the whole first chart in.
                (still(target), Spring::still(1.0, 1.0))
            } else if let Some(start) = camera.and_then(|c| c.inverse()).map(|c| c.apply(target)) {
                // Where it would have been in the old chart, had it been drawn.
                (moving(start, target), Spring::still(0.0, 1.0))
            } else if let Some(p) = parents[i].filter(|p| self.tiles.contains_key(&keys[*p])) {
                // Out from under its folder, moving as the folder moves. (A folder that's new
                // itself is growing from its centre, so its contents grow along with it.)
                let source = layout[p].inner.map(Rect::of).filter(Rect::visible).unwrap_or(Rect::of(layout[p].rect));
                (along(&self.tiles[&keys[p]].edges, source, target), Spring::still(0.0, 1.0))
            } else {
                // Grow from its centre.
                let (cx, cy) = target.center();
                (moving(Rect::point(cx, cy), target), Spring::still(0.0, 1.0))
            };
            self.tiles.insert(
                key.clone(),
                TileMotion {
                    edges,
                    turns: [Spring::still(segment.start, segment.start), Spring::still(segment.end, segment.end)],
                    alpha,
                    inner,
                    depth: segment.depth,
                    current: Some(i),
                    color: hsla(0., 0., 0., 0.),
                },
            );
        }
        // Tiles that are gone: carried off by the camera, or shrinking away where they were.
        for motion in self.tiles.values_mut().filter(|m| m.current.is_none()) {
            let target = motion.target();
            let to = match camera {
                Some(camera) => camera.apply(target),
                None => {
                    let (cx, cy) = target.center();
                    Rect::point(cx, cy)
                }
            };
            motion.aim(to);
            motion.alpha.target = 0.0;
        }
        self.depths = segments.iter().map(|s| s.depth).collect();
        self.keys = keys.to_vec();
        self.parents = parents;
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
        self.tiles.retain(|_, motion| {
            motion.step(dt);
            let settled = motion.settled();
            moving |= !settled;
            // Departed tiles go once they've shrunk away, faded out, or come to rest.
            motion.current.is_some() || (!settled && motion.rect().visible() && motion.alpha.x >= 0.01)
        });
        moving
    }

    /// Every tile where it is right now, in painting order: tiles on their way out first
    /// (underneath), shallowest first, then the latest layout depth-first, so folders are
    /// beneath their contents.
    fn placed(&self) -> Vec<Placed> {
        let Some(area) = self.area else { return Vec::new() };
        let fraction = self.fraction.x;
        // Volume scans fill the area from the left as they go.
        let squeeze = |r: Rect| Rect { x0: area.x0 + (r.x0 - area.x0) * fraction, x1: area.x0 + (r.x1 - area.x0) * fraction, ..r };
        let mut gone: Vec<&TileMotion> = self.tiles.values().filter(|m| m.current.is_none()).collect();
        gone.sort_by_key(|m| m.depth);
        let mut out: Vec<Placed> = gone
            .into_iter()
            .map(|m| Placed { index: None, rect: squeeze(m.rect()), alpha: m.alpha.x.clamp(0.0, 1.0), color: m.color, header: false, leaf: false })
            .collect();
        // While revealing, each folder carries its contents along as it grows.
        let mut carried = vec![Affine::IDENTITY; self.keys.len()];
        for (i, key) in self.keys.iter().enumerate() {
            let Some(motion) = self.tiles.get(key) else { continue };
            let (mut rect, mut alpha) = (motion.rect(), motion.alpha.x.clamp(0.0, 1.0));
            if let Some(t) = self.reveal {
                let k = smootherstep((t - (self.depths[i] as f32 - 1.0) * REVEAL_STAGGER) / REVEAL_EACH);
                let outer = self.parents[i].map(|p| carried[p]).unwrap_or(Affine::IDENTITY);
                let grown = outer.apply(rect).scaled(REVEAL_FROM + (1.0 - REVEAL_FROM) * k);
                carried[i] = Affine::between(rect, grown).unwrap_or(outer);
                (rect, alpha) = (grown, alpha * k);
            }
            let tile = &self.layout[i];
            out.push(Placed { index: Some(i), rect: squeeze(rect), alpha, color: motion.color, header: tile.header, leaf: tile.inner.is_none() });
        }
        out
    }

    /// Paint every tile where it is right now, and say what was painted (for labels).
    /// `color` gets the tile's index in the latest layout and its current start and end in
    /// turns (so hues taken from them follow the motion, as on the sunburst).
    pub fn paint(&mut self, window: &mut Window, mut color: impl FnMut(usize, f32, f32) -> Hsla) -> Vec<PaintedTile> {
        for (i, key) in self.keys.iter().enumerate() {
            if let Some(motion) = self.tiles.get_mut(key) {
                motion.color = color(i, motion.turns[0].x, motion.turns[1].x);
            }
        }
        if let Some(fade) = &self.fade {
            let k = 1.0 - smootherstep(fade.t / FADE_SECONDS);
            for ghost in &fade.ghosts {
                paint_tile(window, ghost.rect, ghost.color, ghost.alpha * k);
            }
        }
        let mut painted = Vec::new();
        for placed in self.placed() {
            if paint_tile(window, placed.rect, placed.color, placed.alpha)
                && let Some(index) = placed.index
            {
                painted.push(PaintedTile { index, rect: placed.rect.bounds(), alpha: placed.alpha, header: placed.header, leaf: placed.leaf });
            }
        }
        painted
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

/// Text colour that reads on a tile of colour `bar`: near-white on dark tiles, near-black
/// on light ones.
fn ink(bar: Hsla, alpha: f32) -> Hsla {
    if bar.lightness > 0.62 { hsla(0.0, 0.0, 0.08, 0.85 * alpha) } else { hsla(0.0, 0.0, 1.0, 0.92 * alpha) }
}

/// `text` shaped to fit `max_width`, cut short with "…" if need be (binary searching how
/// many characters fit); `None` if not even one character does.
fn fit(window: &Window, text: &SharedString, font: &Font, font_size: f32, color: Hsla, max_width: f32) -> Option<gpui::ShapedLine> {
    let shape = |text: SharedString| {
        let run = TextRun { len: text.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None, letter_spacing: None };
        window.text_system().shape_line(text, px(font_size), &[run], None)
    };
    if max_width <= 0.0 || text.is_empty() {
        return None;
    }
    let full = shape(text.clone());
    if f32::from(full.width()) <= max_width {
        return Some(full);
    }
    let cuts: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
    let cut = |n: usize| SharedString::from(format!("{}…", text[..cuts[n]].trim_end()));
    // `lo` characters fit (0: none known to); all of them don't.
    let (mut lo, mut hi) = (0, cuts.len());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if f32::from(shape(cut(mid)).width()) <= max_width {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo > 0).then(|| shape(cut(lo)))
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
    let size_line = (!size.is_empty()).then(|| fit(window, size, font, 12.0, quiet, f32::INFINITY)).flatten();
    let size_width = size_line.as_ref().map(|l| f32::from(l.width()) + SPACE).unwrap_or(0.0);
    let (name_line, with_size) = match fit(window, name, font, 12.0, ink, max_width - size_width) {
        Some(line) if size_line.is_some() && (f32::from(line.width()) >= 24.0 || line.text == *name) => (Some(line), true),
        _ => (fit(window, name, font, 12.0, ink, max_width), false),
    };
    let Some(name_line) = name_line else { return };
    paint_line(&name_line, origin, line_height, clip, window, cx);
    if let (true, Some(size_line)) = (with_size, size_line) {
        let at = point(origin.x + name_line.width() + px(SPACE), origin.y);
        paint_line(&size_line, at, line_height, clip, window, cx);
    }
}

/// Label the tiles `paint` drew: a folder's header strip gets "Name  size" on one line, and
/// a tile with nothing drawn inside it gets its name, with its size below when there's room.
/// `label` gives a tile's name, size and colour by its index in the latest layout. Labels
/// are cut short with "…" to fit, and clipped to their tiles.
pub fn paint_labels(painted: &[PaintedTile], label: impl Fn(usize) -> Option<(SharedString, SharedString, Hsla)>, window: &mut Window, cx: &mut App) {
    const PAD: f32 = 6.0;
    let regular = window.text_style().font();
    let medium = Font { weight: FontWeight::MEDIUM, ..regular.clone() };
    for tile in painted {
        let rect = Rect::of(tile.rect);
        if tile.alpha < 0.05 {
            continue;
        }
        if tile.header {
            if rect.w() < 40.0 || rect.h() < HEADER {
                continue;
            }
            let Some((name, size, bar)) = label(tile.index) else { continue };
            let strip = Rect { y1: rect.y0 + HEADER, ..rect };
            let origin = point(px(rect.x0 + PAD), px(rect.y0));
            paint_name_and_size(&name, &size, &medium, ink(bar, tile.alpha), origin, rect.w() - 2.0 * PAD, HEADER, strip, window, cx);
        } else if tile.leaf {
            if rect.w() < 40.0 || rect.h() < 16.0 {
                continue;
            }
            let Some((name, size, bar)) = label(tile.index) else { continue };
            let ink = ink(bar, tile.alpha);
            let max_width = rect.w() - 2.0 * PAD;
            if let Some(line) = fit(window, &name, &medium, 12.0, ink, max_width) {
                paint_line(&line, point(px(rect.x0 + PAD), px(rect.y0 + 1.0)), 16.0, rect, window, cx);
            }
            if rect.h() >= 32.0
                && let Some(line) = fit(window, &size, &regular, 11.0, faded(ink, 0.8), max_width)
            {
                paint_line(&line, point(px(rect.x0 + PAD), px(rect.y0 + 16.0)), 14.0, rect, window, cx);
            }
        }
    }
}

/// The focus folder's bar across the top of the chart (see `content_area`): its name and
/// size, or, hovered, where clicking goes. Clicking it goes up a level (`Hit::Center`).
pub fn paint_focus_bar(bar: Bounds<Pixels>, title: &SharedString, subtitle: &SharedString, hovered: bool, window: &mut Window, cx: &mut App) {
    let rect = Rect::of(bar);
    if !rect.visible() {
        return;
    }
    let background: Hsla = gpui::rgb(if hovered { 0x3a3d44 } else { 0x2b2d33 }).into_color();
    window.paint_quad(fill(bar, background).corner_radii(px(4.0)));
    let medium = Font { weight: FontWeight::MEDIUM, ..window.text_style().font() };
    let origin = point(px(rect.x0 + 8.0), px(rect.y0));
    paint_name_and_size(title, subtitle, &medium, ink(background, 1.0), origin, rect.w() - 16.0, rect.h(), rect, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Kind;
    use crate::sunburst::Target;
    use std::time::Duration;

    fn seg(start: f32, end: f32, depth: usize) -> Segment {
        Segment { target: Target::Node(0), depth, start, end, kind: Kind::Dir }
    }

    fn key(path: &str) -> Key {
        Key::Node(path.split('/').map(SharedString::from).collect())
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

    /// Every tile is sane, sits inside its parent's inner rect (or the area), and siblings
    /// never overlap.
    fn check_layout(segments: &[Segment], area: Bounds<Pixels>) -> Vec<Tile> {
        let tiles = layout(segments, area);
        assert_eq!(tiles.len(), segments.len());
        let parents = parents(segments);
        for (i, tile) in tiles.iter().enumerate() {
            let r = Rect::of(tile.rect);
            assert!(sane(r), "tile {i} is {r:?}");
            if let Some(inner) = tile.inner {
                assert!(inside(Rect::of(inner), r) && sane(Rect::of(inner)), "inner of {i} sits inside it");
            }
            let within = match parents[i] {
                Some(p) => match tiles[p].inner {
                    Some(inner) => Rect::of(inner),
                    None => {
                        assert!(!r.visible(), "a folder without room shows nothing inside: {i} {r:?}");
                        continue;
                    }
                },
                None => Rect::of(area),
            };
            assert!(inside(r, within), "tile {i} {r:?} inside {within:?}");
            for j in (i + 1)..tiles.len() {
                if parents[j] == parents[i] {
                    let o = overlap(r, Rect::of(tiles[j].rect));
                    assert!(o <= EPS, "siblings {i} and {j} overlap by {o}");
                }
            }
        }
        tiles
    }

    #[test]
    fn slots_tile_the_rect_in_proportion() {
        let weights = [6.0, 4.0, 3.0, 2.0, 1.0, 1.0, 0.5];
        let within = rect(10.0, 20.0, 310.0, 220.0);
        let slots = squarify(&weights, within);
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
    fn zero_weights_and_a_single_child() {
        let within = rect(0.0, 0.0, 100.0, 50.0);
        let slots = squarify(&[0.0, 1.0, 0.0], within);
        assert!(!slots[0].visible() && !slots[2].visible());
        assert_eq!(slots[1], within);
        assert_eq!(squarify(&[2.5], within), [within]);
        assert!(squarify(&[0.0, 0.0], within).iter().all(|s| !s.visible() && sane(*s)));
        assert!(squarify(&[1.0, 1.0], rect(5.0, 5.0, 5.0, 5.0)).iter().all(|s| sane(*s)));

        // A folder with one child: it fills the folder's inner rect, less the gap.
        let tiles = check_layout(&[seg(0.0, 1.0, 1), seg(0.0, 1.0, 2)], area());
        let inner = Rect::of(tiles[0].inner.unwrap());
        assert!(tiles[0].header);
        assert_eq!(Rect::of(tiles[1].rect), inner.gap(GAP));
    }

    #[test]
    fn nested_layout_stays_inside_its_folders() {
        let segments = [
            seg(0.0, 0.5, 1),
            seg(0.0, 0.3, 2),
            seg(0.0, 0.2, 3),
            seg(0.2, 0.3, 3),
            seg(0.3, 0.5, 2),
            seg(0.5, 0.8, 1),
            seg(0.8, 1.0, 1),
            seg(0.8, 0.801, 2),
            seg(0.801, 1.0, 2),
        ];
        let tiles = check_layout(&segments, area());
        assert!(tiles[0].header && tiles[0].inner.is_some() && tiles[5].inner.is_none());
        // Top-level areas are in proportion, before the gaps.
        let a = |i: usize| (Rect::of(tiles[i].rect).w() + 2.0 * GAP) * (Rect::of(tiles[i].rect).h() + 2.0 * GAP);
        assert!((a(0) / (800.0 * 600.0) - 0.5).abs() < 0.005 && (a(5) / a(6) - 1.5).abs() < 0.015);
        // Deterministic.
        assert_eq!(layout(&segments, area()), tiles);
    }

    #[test]
    fn random_layouts_are_sound() {
        // A small LCG, so the "random" trees are the same every run.
        let mut seed = 0x2545_f491_u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as f32 / (1u64 << 31) as f32
        };
        fn grow(start: f32, end: f32, depth: usize, next: &mut impl FnMut() -> f32, out: &mut Vec<Segment>) {
            let n = (next() * 8.0) as usize;
            let weights: Vec<f32> = (0..n).map(|_| next().powi(3)).collect();
            let total: f32 = weights.iter().sum::<f32>().max(1e-6);
            let mut at = start;
            for w in weights {
                let width = (end - start) * w / total;
                out.push(seg(at, at + width, depth));
                if depth < 6 && next() < 0.5 {
                    grow(at, at + width, depth + 1, next, out);
                }
                at += width;
            }
        }
        for (k, area) in [area(), rect(0.0, 0.0, 2000.0, 3.0).bounds(), rect(5.0, 5.0, 8.0, 1505.0).bounds(), rect(0.0, 0.0, 0.0, 0.0).bounds()].into_iter().enumerate() {
            for _ in 0..20 {
                let mut segments = Vec::new();
                grow(0.0, 1.0, 1, &mut next, &mut segments);
                let tiles = check_layout(&segments, area);
                if k == 0 {
                    let top: f32 = tiles.iter().zip(&segments).filter(|(_, s)| s.depth == 1).map(|(t, _)| Rect::of(t.rect).w() * Rect::of(t.rect).h()).sum();
                    assert!(segments.is_empty() || top > 0.9 * 800.0 * 600.0, "the top level fills the area");
                }
            }
        }
    }

    #[test]
    fn hit_test_finds_the_deepest_tile() {
        let segments = [seg(0.0, 0.6, 1), seg(0.0, 0.4, 2), seg(0.4, 0.6, 2), seg(0.6, 1.0, 1)];
        let (bar, tiles_area) = content_area(rect(0.0, 0.0, 860.0, 700.0).bounds());
        let tiles = layout(&segments, tiles_area);
        let middle = |r: Bounds<Pixels>| {
            let (x, y) = Rect::of(r).center();
            point(px(x), px(y))
        };
        assert_eq!(hit_test(&tiles, bar, middle(tiles[1].rect)), Some(Hit::Segment(1)));
        assert_eq!(hit_test(&tiles, bar, middle(tiles[3].rect)), Some(Hit::Segment(3)));
        // The folder's header is the folder.
        let header = Rect::of(tiles[0].rect);
        assert_eq!(hit_test(&tiles, bar, point(px(header.x0 + 10.0), px(header.y0 + 5.0))), Some(Hit::Segment(0)));
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
        motion.keys.iter().enumerate().all(|(i, k)| close(motion.tiles[k].rect(), Rect::of(motion.layout[i].rect)))
    }

    fn root() -> (Vec<Key>, Vec<Segment>) {
        // a (60%) ── a/x, a/y;  b (40%)
        (vec![key("a"), key("a/x"), key("a/y"), key("b")], vec![seg(0.0, 0.6, 1), seg(0.0, 0.3, 2), seg(0.3, 0.6, 2), seg(0.6, 1.0, 1)])
    }

    #[test]
    fn the_first_layout_is_revealed_by_depth_and_settles() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        let mut seen_stagger = false;
        let frames = run(&mut motion, &mut t, |m| {
            let placed = m.placed();
            let alpha = |i: usize| placed.iter().find(|p| p.index == Some(i)).unwrap().alpha;
            seen_stagger |= alpha(0) > 0.2 && alpha(1) < alpha(0) - 0.1;
            assert!(alpha(1) <= alpha(0) + 1e-4, "the outside comes in first");
        });
        assert!(seen_stagger, "deeper levels follow on");
        assert!(frames <= 60, "revealed within ~0.9 s, took {frames} frames");
        assert!(at_targets(&motion) && motion.reveal.is_none());
    }

    #[test]
    fn a_grown_layout_and_a_departed_tile_settle() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        // "a/y" leaves, "c" arrives and "b" shrinks.
        let keys = [key("a"), key("a/x"), key("b"), key("c")];
        motion.retarget(2, &keys, &[seg(0.0, 0.6, 1), seg(0.0, 0.6, 2), seg(0.6, 0.8, 1), seg(0.8, 1.0, 1)], area(), 1.0);
        let gone = motion.tiles[&key("a/y")].target();
        assert!(gone.w() == 0.0 && gone.h() == 0.0, "the departed tile shrinks to a point");
        let c = now(&motion, "c");
        assert!(c.w() == 0.0 && c.h() == 0.0 && motion.tiles[&key("c")].alpha.x == 0.0, "the new top-level tile grows from its centre");
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75, "settles within ~1 s, took {frames} frames");
        assert!(!motion.tiles.contains_key(&key("a/y")), "the departed tile is removed");
        assert!(at_targets(&motion));
    }

    #[test]
    fn a_folders_new_contents_start_inside_it() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        motion.retarget(1, &[key("a"), key("b")], &[seg(0.0, 0.5, 1), seg(0.5, 1.0, 1)], area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        // "a" grows and its contents appear.
        let keys = [key("a"), key("a/x"), key("a/y"), key("b")];
        motion.retarget(2, &keys, &[seg(0.0, 0.7, 1), seg(0.0, 0.4, 2), seg(0.4, 0.7, 2), seg(0.7, 1.0, 1)], area(), 1.0);
        let a = now(&motion, "a");
        for child in ["a/x", "a/y"] {
            assert!(inside(now(&motion, child), a), "{child} starts inside a as drawn: {:?} {a:?}", now(&motion, child));
        }
        run(&mut motion, &mut t, |m| {
            for child in ["a/x", "a/y"] {
                assert!(inside(now(m, child), now(m, "a")), "{child} stays inside a as it moves");
            }
        });
        assert!(at_targets(&motion));
    }

    #[test]
    fn zooming_in_and_out_moves_the_chart_as_one() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        let old_a = motion.tiles[&key("a")].inner.unwrap();

        // Into "a": its contents fill the area, "b" is pushed off past the edge.
        motion.zoom(None, Some(key("a")));
        let keys = [key("a/x"), key("a/x/p"), key("a/y")];
        motion.retarget(2, &keys, &[seg(0.0, 0.5, 1), seg(0.0, 0.5, 2), seg(0.5, 1.0, 1)], area(), 1.0);
        assert!(inside(now(&motion, "a/x/p"), old_a), "new tiles start inside the folder zoomed into");
        let b = motion.tiles[&key("b")].target();
        assert!(b.x0 >= 800.0 - EPS || b.y0 >= 600.0 - EPS, "b flies off past the edge: {b:?}");
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75, "took {frames} frames");
        assert!(!motion.tiles.contains_key(&key("a")) && !motion.tiles.contains_key(&key("b")));
        assert!(at_targets(&motion));

        // And back out: the chart shrinks into "a", and "b" comes back in from outside.
        motion.zoom(Some(key("a")), None);
        let (keys, segments) = root();
        motion.retarget(3, &keys, &segments, area(), 1.0);
        let (a, b) = (now(&motion, "a"), now(&motion, "b"));
        assert!(a.x0 <= 0.0 + EPS && a.x1 >= 800.0 - EPS, "a starts out filling the area: {a:?}");
        assert!(b.x0 >= 800.0 - EPS || b.y0 >= 600.0 - EPS, "b starts outside: {b:?}");
        let frames = run(&mut motion, &mut t, |_| {});
        assert!(frames < 75, "took {frames} frames");
        assert!(!motion.tiles.contains_key(&key("a/x/p")));
        assert!(at_targets(&motion));
    }

    #[test]
    fn a_lateral_jump_cross_fades() {
        let mut motion = TreemapMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = root();
        motion.retarget(1, &keys, &segments, area(), 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.zoom(Some(key("q")), Some(key("r")));
        motion.retarget(2, &[key("r/1")], &[seg(0.0, 1.0, 1)], area(), 1.0);
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
        assert!(motion.folding() && motion.fade.as_ref().unwrap().ghosts.len() == 4);
        let ghost = motion.fade.as_ref().unwrap().ghosts[0].rect;
        assert!(ghost.x1 <= 400.0 + EPS, "the ghosts are as drawn, squeezed by the fraction");
        let keys = [key("b"), key("a"), key("a/y"), key("a/x")];
        let layout = [seg(0.0, 0.4, 1), seg(0.4, 1.0, 1), seg(0.4, 0.7, 2), seg(0.7, 1.0, 2)];
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
        // Twice the size, carried along at once: only the fixed-size gaps and headers are
        // left to settle.
        let off = motion.keys.iter().enumerate().map(|(i, k)| {
            let (r, t) = (motion.tiles[k].rect(), Rect::of(motion.layout[i].rect));
            [r.x0 - t.x0, r.y0 - t.y0, r.x1 - t.x1, r.y1 - t.y1].iter().fold(0.0f32, |m, d| m.max(d.abs()))
        });
        assert!(off.fold(0.0, f32::max) < 24.0);
        assert_eq!(motion.tiles(), layout(&segments, bigger).as_slice());
        run(&mut motion, &mut t, |_| {});
        assert!(at_targets(&motion));
    }
}
