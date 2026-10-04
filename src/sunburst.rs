//! Sunburst and icicle layout, painting and hit testing.
//!
//! Angles are measured in turns (0..1), clockwise from 12 o'clock. The icicle is the same
//! chart unrolled and hung from the top: rings become rows falling from top to bottom, and
//! turns run left to right, so everything that thinks in rings and turns (the layout, the
//! motion) works for both.

use std::f32::consts::TAU;

use gpui::{
    App, Bounds, ContentMask, Font, FontWeight, Hsla, PathBuilder, Pixels, Point, SharedString, ShapedLine, TextAlign, TextRun, Window, hsla,
    point, px, size,
};

use crate::scan::{Kind, Tree};

pub const MAX_DEPTH: usize = 6;
/// Children thinner than this are merged into a single "smaller objects" segment.
const MIN_TURNS: f32 = 0.004;
const RING_FALLOFF: f32 = 0.84;
const GAP_PX: f32 = 1.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Node(usize),
    /// Aggregated tiny children of `parent`, a run of them starting at `first`. (A folder
    /// can have several runs when its children aren't sorted by size, as mid-scan.)
    Small { parent: usize, first: usize },
}

#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub target: Target,
    /// 1-based ring index.
    pub depth: usize,
    pub start: f32,
    pub end: f32,
    pub kind: Kind,
}

impl Segment {
    pub fn hue(&self) -> f32 {
        ((self.start + self.end) / 2.0).rem_euclid(1.0)
    }
}

pub fn layout(tree: &Tree, focus: usize) -> Vec<Segment> {
    layout_to(tree, focus, MAX_DEPTH)
}

/// `layout`, only `max_depth` rings deep: the treemap shows just the focus folder's own
/// contents, so it has no use for anything further in (and laying it out would be wasted).
/// The segments that are there are exactly `layout`'s, so they're coloured and keyed alike.
pub fn layout_to(tree: &Tree, focus: usize, max_depth: usize) -> Vec<Segment> {
    let mut out = Vec::new();
    if max_depth > 0 {
        layout_children(tree, focus, 1, max_depth, 0.0, 1.0, &mut out);
    }
    out
}

fn layout_children(tree: &Tree, ix: usize, depth: usize, max_depth: usize, start: f32, end: f32, out: &mut Vec<Segment>) {
    let node = &tree.nodes[ix];
    if node.size == 0 {
        return;
    }
    let span = end - start;
    let total = node.size as f64;
    let mut angle = start;
    // Thin children are merged into one "smaller objects" sliver. Children are sorted
    // largest first, except slices pinned to the end (e.g. "Not scanned yet"), so keep
    // going after the thin ones: anything large after them still gets its own segment.
    let mut small: Option<(f32, f32, usize)> = None;
    let flush = |small: &mut Option<(f32, f32, usize)>, out: &mut Vec<Segment>| {
        if let Some((from, width, first)) = small.take() {
            if width >= MIN_TURNS / 2.0 {
                out.push(Segment { target: Target::Small { parent: ix, first }, depth, start: from, end: from + width, kind: Kind::File });
            }
        }
    };
    for &child_ix in &node.children {
        let child = &tree.nodes[child_ix];
        let width = (span as f64 * child.size as f64 / total) as f32;
        if width < MIN_TURNS {
            let run = small.get_or_insert((angle, 0.0, child_ix));
            run.1 += width;
            angle += width;
            continue;
        }
        flush(&mut small, out);
        out.push(Segment {
            target: Target::Node(child_ix),
            depth,
            start: angle,
            end: angle + width,
            kind: child.kind,
        });
        if child.kind == Kind::Dir && depth < max_depth {
            layout_children(tree, child_ix, depth + 1, max_depth, angle, angle + width, out);
        }
        angle += width;
    }
    flush(&mut small, out);
}

/// Where `node` sits in the chart centred on `ancestor`: its angles, and its ring
/// (0 for `ancestor` itself, which is the centre). Matches `layout`, ignoring merging.
pub fn frame_of(tree: &Tree, ancestor: usize, node: usize) -> Option<(f32, f32, f32)> {
    let mut chain = vec![node];
    while *chain.last()? != ancestor {
        chain.push(tree.nodes[*chain.last()?].parent?);
    }
    let (mut start, mut end) = (0.0f64, 1.0f64);
    for pair in chain.windows(2).rev() {
        let (child, parent) = (pair[0], pair[1]);
        let total = tree.nodes[parent].size as f64;
        if total <= 0.0 {
            return None;
        }
        let span = end - start;
        let before: u64 = tree.nodes[parent].children.iter().take_while(|&&c| c != child).map(|&c| tree.nodes[c].size).sum();
        start += span * before as f64 / total;
        end = start + span * tree.nodes[child].size as f64 / total;
    }
    Some((start as f32, end as f32, (chain.len() - 1) as f32))
}

pub fn base_color(segment: &Segment) -> Hsla {
    let d = (segment.depth - 1) as f32;
    match (segment.target, segment.kind) {
        (Target::Small { .. }, _) => hsla(0.0, 0.0, 0.38, 1.0),
        (_, Kind::File) => hsla(segment.hue(), 0.10, 0.50 + d * 0.02, 1.0),
        (_, Kind::Other) => hsla(0.0, 0.0, 0.45, 1.0),
        (_, Kind::Dir) => hsla(segment.hue(), 0.70 - d * 0.06, 0.52 + d * 0.035, 1.0),
    }
}

/// Blend from `from` to `to` (saturation and lightness; the hue is `to`'s).
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    hsla(
        hue_turns(&to),
        from.saturation + (to.saturation - from.saturation) * t,
        from.lightness + (to.lightness - from.lightness) * t,
        to.alpha,
    )
}

fn hue_turns(color: &Hsla) -> f32 {
    color.hue.into_positive_degrees() / 360.0
}

pub fn highlight(color: Hsla) -> Hsla {
    hsla(hue_turns(&color), (color.saturation + 0.1).min(1.0), (color.lightness + 0.14).min(0.9), 1.0)
}

pub fn dim(color: Hsla) -> Hsla {
    hsla(hue_turns(&color), color.saturation * 0.8, color.lightness * 0.82, 1.0)
}

/// Highlight for colours that are already vivid (colouring by kind): lighter, without
/// pushing saturation, which turns a pure green neon.
pub fn lift(color: Hsla) -> Hsla {
    hsla(hue_turns(&color), color.saturation.min(0.8), (color.lightness + 0.1).min(0.85), 1.0)
}

/// Much quieter than `dim`, so one highlighted group stands out from everything else.
pub fn fade(color: Hsla) -> Hsla {
    hsla(hue_turns(&color), color.saturation * 0.3, color.lightness * 0.6, 1.0)
}

/// Room left around the icicle: the app draws its hover label in the strip along the top.
const ICICLE_MARGIN: f32 = 16.0;
const ICICLE_TOP: f32 = 52.0;
/// The icicle's first row (the folder in focus, a bar across the top) is this share of a
/// normal row: it only needs to be something to click to go up a level.
const ICICLE_CENTER_SHARE: f32 = 0.5;
/// Bars smaller than this lose their rounded corners, which would eat them.
const ROUNDED_MIN_PX: f32 = 6.0;

/// Which way the chart is drawn.
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    /// Rings round `center`; "radius" is the distance from it.
    Sunburst { center: Point<Pixels> },
    /// Rows falling from `top`, between `left` and `right` (window pixels); "radius" is y,
    /// and turns run from `left` to `right`.
    Icicle { top: f32, left: f32, right: f32 },
}

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub shape: Shape,
    /// Where the centre ends: the radius of the disc, or the bottom edge of the icicle's
    /// first row.
    pub inner_radius: f32,
    /// Inner and outer radius of each ring (for the icicle, the top and bottom edge of each
    /// row).
    pub rings: [(f32, f32); MAX_DEPTH],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Center,
    Segment(usize),
}

/// A band as `ChartMotion::paint` draws it this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// Where the whole band is, even the part the reveal hasn't uncovered yet.
    pub r0: f32,
    pub r1: f32,
    /// How far out it's uncovered (the first layout's reveal): `r1` once all of it shows.
    /// It's painted from `r0` to here.
    pub reach: f32,
    /// Turns, after the scan's fraction and any fold placement.
    pub start: f32,
    pub end: f32,
    /// The colour it's painted, before `alpha`, so labels can contrast with exactly that.
    pub color: Hsla,
    pub alpha: f32,
}

impl Band {
    /// The colour as drawn, with `alpha` applied.
    pub fn drawn_color(&self) -> Hsla {
        let c = self.color;
        hsla(hue_turns(&c), c.saturation, c.lightness, c.alpha * self.alpha)
    }
}

/// A band of the latest layout as `ChartMotion::paint` drew it, so labels can go where the
/// bars actually are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Painted {
    /// Index into the latest layout's segments.
    pub index: usize,
    pub band: Band,
}

impl Geometry {
    pub fn new(bounds: Bounds<Pixels>) -> Self {
        let radius = (f32::from(bounds.size.width).min(f32::from(bounds.size.height)) / 2.0 - 24.0)
            .max(40.0);
        let inner_radius = radius * 0.24;
        let weights: Vec<f32> = (0..MAX_DEPTH).map(|k| RING_FALLOFF.powi(k as i32)).collect();
        let unit = (radius - inner_radius) / weights.iter().sum::<f32>();
        let mut rings = [(0.0, 0.0); MAX_DEPTH];
        let mut r = inner_radius;
        for (k, weight) in weights.iter().enumerate() {
            rings[k] = (r, r + weight * unit);
            r += weight * unit;
        }
        Self { shape: Shape::Sunburst { center: bounds.center() }, inner_radius, rings }
    }

    /// The icicle filling `bounds`, less a margin and the strip along the top: the focus
    /// folder a bar across the top, and each level below it a row. Rows are all the same
    /// height (unlike the sunburst's rings, they don't need to thin out to keep areas fair),
    /// apart from the focus folder's, which is half a row.
    pub fn icicle(bounds: Bounds<Pixels>) -> Self {
        let left = f32::from(bounds.origin.x) + ICICLE_MARGIN;
        let right = (f32::from(bounds.origin.x + bounds.size.width) - ICICLE_MARGIN).max(left + 1.0);
        let top = f32::from(bounds.origin.y) + ICICLE_TOP;
        let bottom = (f32::from(bounds.origin.y + bounds.size.height) - ICICLE_MARGIN).max(top + 1.0);
        let unit = (bottom - top) / (ICICLE_CENTER_SHARE + MAX_DEPTH as f32);
        let inner_radius = top + unit * ICICLE_CENTER_SHARE;
        let mut rings = [(0.0, 0.0); MAX_DEPTH];
        for (k, ring) in rings.iter_mut().enumerate() {
            *ring = (inner_radius + unit * k as f32, inner_radius + unit * (k + 1) as f32);
        }
        // Exactly the edge, rather than whatever the sums round to.
        rings[MAX_DEPTH - 1].1 = bottom;
        Self { shape: Shape::Icicle { top, left, right }, inner_radius, rings }
    }

    /// A single ring, for small gauges.
    pub fn ring(center: Point<Pixels>, inner: f32, outer: f32) -> Self {
        Self { shape: Shape::Sunburst { center }, inner_radius: inner, rings: [(inner, outer); MAX_DEPTH] }
    }

    pub fn outer_radius(&self) -> f32 {
        self.rings[MAX_DEPTH - 1].1
    }

    /// The smallest radius there is: the middle of the disc, or the icicle's top edge.
    fn origin(&self) -> f32 {
        match self.shape {
            Shape::Sunburst { .. } => 0.0,
            Shape::Icicle { top, .. } => top,
        }
    }

    /// The icicle's chart area, in window pixels.
    fn icicle_rect(&self) -> Option<Bounds<Pixels>> {
        match self.shape {
            Shape::Sunburst { .. } => None,
            Shape::Icicle { top, left, right } => Some(rect(left, top, right, self.outer_radius())),
        }
    }

    pub fn hit_test(&self, position: Point<Pixels>, segments: &[Segment]) -> Option<Hit> {
        let (distance, turns) = match self.shape {
            Shape::Sunburst { center } => {
                let dx = f32::from(position.x - center.x);
                let dy = f32::from(position.y - center.y);
                let distance = (dx * dx + dy * dy).sqrt();
                if distance < self.inner_radius {
                    return Some(Hit::Center);
                }
                (distance, dx.atan2(-dy).rem_euclid(TAU) / TAU)
            }
            Shape::Icicle { top, left, right } => {
                let (x, y) = (f32::from(position.x), f32::from(position.y));
                if x < left || x >= right || y < top || y >= self.outer_radius() {
                    return None;
                }
                if y < self.inner_radius {
                    return Some(Hit::Center);
                }
                (y, (x - left) / (right - left))
            }
        };
        let depth = self.rings.iter().position(|(r0, r1)| distance >= *r0 && distance < *r1)? + 1;
        segments
            .iter()
            .position(|s| s.depth == depth && turns >= s.start && turns < s.end)
            .map(Hit::Segment)
    }

    fn at(center: Point<Pixels>, radius: f32, turns: f32) -> Point<Pixels> {
        let theta = turns * TAU;
        point(center.x + px(radius * theta.sin()), center.y - px(radius * theta.cos()))
    }

    /// Paint one segment in its ring, leaving a hairline gap around it.
    pub fn paint_sector(&self, window: &mut Window, depth: usize, start: f32, end: f32, color: Hsla) {
        let (r0, r1) = self.rings[depth - 1];
        self.paint_band(window, r0, r1, start, end, color);
    }

    /// Inner and outer radius at a fractional ring depth (1.0 = first ring), so a
    /// segment can glide between rings.
    fn ring_at(&self, depth: f32) -> (f32, f32) {
        let depth = depth.clamp(1.0, MAX_DEPTH as f32);
        let below = depth.floor() as usize;
        let above = (below + 1).min(MAX_DEPTH);
        let t = depth - below as f32;
        let (a, b) = (self.rings[below - 1], self.rings[above - 1]);
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }

    /// Inner and outer radius, and opacity, at any fractional ring depth. Below the first
    /// ring a band sinks behind the centre (for the icicle, slides up into the first row),
    /// fading; beyond the last it moves out past the edge (for the icicle, down off the
    /// bottom), fading. That's where the camera takes segments when zooming.
    pub fn band_at(&self, depth: f32) -> (f32, f32, f32) {
        let last = MAX_DEPTH as f32;
        if depth < 1.0 {
            let (r0, r1) = self.rings[0];
            let shift = (1.0 - depth) * (r1 - r0);
            let floor = self.origin();
            ((r0 - shift).max(floor), (r1 - shift).max(floor), depth.clamp(0.0, 1.0))
        } else if depth > last {
            let (r0, r1) = self.rings[MAX_DEPTH - 1];
            let shift = (depth - last) * (r1 - r0);
            (r0 + shift, r1 + shift, (last + 1.0 - depth).clamp(0.0, 1.0))
        } else {
            let (r0, r1) = self.ring_at(depth);
            (r0, r1, 1.0)
        }
    }

    /// Where a band is drawn in the icicle, inside its hairline gap; `None` if nothing's
    /// left of it (or this is a sunburst).
    fn icicle_bar(&self, r0: f32, r1: f32, start: f32, end: f32) -> Option<(f32, f32, f32, f32)> {
        let Shape::Icicle { left, right, .. } = self.shape else { return None };
        let width = right - left;
        let (x0, x1) = (left + start * width + GAP_PX / 2.0, left + end * width - GAP_PX / 2.0);
        let (y0, y1) = (r0 + GAP_PX / 2.0, r1 - GAP_PX / 2.0);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }

    /// The part of an icicle bar that's actually painted: the bar cut off at the chart's
    /// bottom edge. A band leaving past the last row (`band_at`) squeezes into that edge as
    /// it fades, as one entering the first row does at the top, rather than sliding down
    /// over the legend under the chart, which nothing else clips. Labels still fit the
    /// whole bar (`icicle_bar`) and are clipped to the chart area.
    fn icicle_drawn(&self, r0: f32, r1: f32, start: f32, end: f32) -> Option<(f32, f32, f32, f32)> {
        self.icicle_bar(r0, r1.min(self.outer_radius()), start, end)
    }

    /// Paint a band between two radii and two angles, leaving a hairline gap around it: an
    /// annular sector, or a bar of the icicle.
    pub fn paint_band(&self, window: &mut Window, r0: f32, r1: f32, start: f32, end: f32, color: Hsla) {
        match self.shape {
            Shape::Sunburst { center } => Self::paint_arc(window, center, r0, r1, start, end, color),
            Shape::Icicle { .. } => {
                if let Some((x0, y0, x1, y1)) = self.icicle_drawn(r0, r1, start, end) {
                    paint_rect(window, rect(x0, y0, x1, y1), 2.0, color);
                }
            }
        }
    }

    fn paint_arc(window: &mut Window, center: Point<Pixels>, r0: f32, r1: f32, start: f32, end: f32, color: Hsla) {
        let (r0, r1) = (r0 + GAP_PX / 2.0, r1 - GAP_PX / 2.0);
        let span = end - start;
        let full_circle = span >= 0.9999;
        let inset = |r: f32| if full_circle { 0.0 } else { (GAP_PX / 2.0) / (r * TAU) };
        let (outer_start, outer_end) = (start + inset(r1), end - inset(r1));
        let (inner_start, inner_end) = (start + inset(r0), end - inset(r0));
        if outer_end <= outer_start || r1 <= r0 {
            return;
        }
        let (inner_start, inner_end) = if inner_end > inner_start {
            (inner_start, inner_end)
        } else {
            let mid = (start + end) / 2.0;
            (mid, mid)
        };

        let steps = ((span * 360.0 / 1.5).ceil() as usize).clamp(2, 400);
        let mut builder = PathBuilder::fill();
        builder.move_to(Self::at(center, r1, outer_start));
        for i in 1..=steps {
            let t = outer_start + (outer_end - outer_start) * i as f32 / steps as f32;
            builder.line_to(Self::at(center, r1, t));
        }
        for i in 0..=steps {
            let t = inner_end - (inner_end - inner_start) * i as f32 / steps as f32;
            builder.line_to(Self::at(center, r0, t));
        }
        builder.close();
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    /// Everything within `radius` of the centre. Sunburst only (the chart's own disc, and
    /// `Geometry::ring` gauges): for anything that has to work for the icicle too, use
    /// `paint_backdrop` and `paint_center`, which know what each shape needs.
    pub fn paint_disc(&self, window: &mut Window, radius: f32, color: Hsla) {
        let Shape::Sunburst { center } = self.shape else {
            debug_assert!(false, "paint_disc is for sunbursts; use paint_backdrop or paint_center");
            return;
        };
        let mut builder = PathBuilder::fill();
        let steps = 180;
        builder.move_to(Self::at(center, radius, 0.0));
        for i in 1..steps {
            builder.line_to(Self::at(center, radius, i as f32 / steps as f32));
        }
        builder.close();
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    /// The panel the chart sits on, a little bigger than the chart.
    pub fn paint_backdrop(&self, window: &mut Window, color: Hsla) {
        match self.icicle_rect() {
            None => self.paint_disc(window, self.outer_radius() + 6.0, color),
            Some(area) => paint_rect(window, area.dilate(px(6.0)), 8.0, color),
        }
    }

    /// The focus folder: the disc in the middle, or the bar across the top of the icicle.
    /// Paint it after the segments, so those sinking into the centre go behind it.
    pub fn paint_center(&self, window: &mut Window, color: Hsla) {
        match self.shape {
            Shape::Sunburst { .. } => self.paint_disc(window, self.inner_radius - 1.0, color),
            Shape::Icicle { top, .. } => self.paint_band(window, top, self.inner_radius, 0.0, 1.0, color),
        }
    }
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x0), px(y0)), size(px(x1 - x0), px(y1 - y0)))
}

/// A filled rectangle, rounded unless it's too small for that.
fn paint_rect(window: &mut Window, bounds: Bounds<Pixels>, radius: f32, color: Hsla) {
    let small = f32::from(bounds.size.width).min(f32::from(bounds.size.height)) < ROUNDED_MIN_PX;
    window.paint_quad(gpui::fill(bounds, color).corner_radii(px(if small { 0.0 } else { radius })));
}

const LABEL_PAD_X: f32 = 6.0;
const LABEL_PAD_TOP: f32 = 3.0;
const NAME_SIZE: f32 = 12.0;
const NAME_LINE: f32 = 15.0;
const DETAIL_SIZE: f32 = 11.0;
const DETAIL_LINE: f32 = 13.0;
/// Labels need this much bar, after the gaps, to be worth drawing: room for a few letters,
/// and for the whole name line, descenders and all, under the padding. The text runs along
/// the bar, so its share of the folder (its width) is what usually rules a label out; rows
/// are tall enough for both lines unless the window is tiny.
const LABEL_MIN_WIDTH: f32 = 28.0;
const LABEL_MIN_HEIGHT: f32 = LABEL_PAD_TOP + NAME_LINE;
/// Tall enough for the size under the name.
const LABEL_TWO_LINES: f32 = LABEL_MIN_HEIGHT + DETAIL_LINE;

/// Text that reads on a bar of colour `bar`: near-black on bars that look light, near-white
/// on the rest, faded with the bar. "Look light" goes by luminance, not HSL lightness: a
/// yellow and a blue of the same lightness are far apart to the eye.
pub fn label_color(bar: Hsla, alpha: f32) -> Hsla {
    let alpha = alpha * bar.alpha;
    let rgb: gpui::Rgba = palette::IntoColor::into_color(bar);
    let linear = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let luminance = 0.2126 * linear(rgb.red) + 0.7152 * linear(rgb.green) + 0.0722 * linear(rgb.blue);
    // About where black and white text contrast equally with the bar.
    if luminance > 0.18 { hsla(0.0, 0.0, 0.08, 0.85 * alpha) } else { hsla(0.0, 0.0, 1.0, 0.92 * alpha) }
}

/// `text` as one line: file names can hold newlines and other control characters, which
/// the text system can't lay out on a single line (and asserts against), so they become
/// spaces.
fn single_line(text: &SharedString) -> SharedString {
    if text.contains(char::is_control) { text.replace(char::is_control, " ").into() } else { text.clone() }
}

/// `text` shaped to fit in `max_width`, cut short with "…" if it doesn't; `None` if not even
/// a letter fits. The cut is found on the whole line as shaped (usually cached from the last
/// frame), so a long name costs one or two new shapings, not a search: labels are fitted
/// every frame, and while the chart moves their widths change every frame.
pub fn fit_line(window: &Window, text: &SharedString, font: &Font, font_size: f32, color: Hsla, max_width: f32) -> Option<ShapedLine> {
    if max_width <= 0.0 || text.is_empty() {
        return None;
    }
    let text = single_line(text);
    let shape = |text: SharedString| {
        let run = TextRun { len: text.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None, letter_spacing: None };
        window.text_system().shape_line(text, px(font_size), &[run], None)
    };
    let whole = shape(text.clone());
    if f32::from(whole.width()) <= max_width {
        return Some(whole);
    }
    let ellipsis = f32::from(shape("…".into()).width());
    // Everything before the character that straddles the room left for the "…".
    let room = max_width - ellipsis;
    let mut at = if room > 0.0 { whole.index_for_x(px(room)).unwrap_or(text.len()).min(text.len()) } else { 0 };
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    // Shaped on its own, the shorter text can kern a hair wider: then give up one more character.
    for _ in 0..2 {
        let kept = text[..at].trim_end();
        if kept.is_empty() {
            return None;
        }
        let line = shape(format!("{kept}…").into());
        if f32::from(line.width()) <= max_width {
            return Some(line);
        }
        at = kept.char_indices().last().map_or(0, |(i, _)| i);
    }
    None
}

/// Names (and sizes, where there's room) on the icicle's bars, as `ChartMotion::paint` drew
/// them. `label` gives a segment's name and size; the text's colour is worked out from the
/// colour the bar was painted. Each label is placed on and fitted to its whole bar, and
/// clipped to the part that's showing, so it's uncovered along with the bar (by the reveal
/// coming down the chart, or sliding down out from under its parent) rather than cut short
/// again every frame.
pub fn paint_icicle_labels(geometry: &Geometry, painted: &[Painted], label: impl Fn(usize) -> Option<(SharedString, SharedString)>, window: &mut Window, cx: &mut App) {
    let Some(area) = geometry.icicle_rect() else { return };
    let font = window.text_style().font();
    let name_font = Font { weight: FontWeight::MEDIUM, ..font.clone() };
    for (i, bar) in painted.iter().enumerate() {
        if bar.band.alpha < 0.05 {
            continue;
        }
        let Some(spot) = label_spot(geometry, painted, i) else { continue };
        let Some((name, detail)) = label(bar.index) else { continue };
        let text = label_color(bar.band.color, bar.band.alpha);
        let width = spot.x1 - spot.x0 - LABEL_PAD_X * 2.0;
        let origin = point(px(spot.x0 + LABEL_PAD_X), px(spot.y0 + LABEL_PAD_TOP));
        let clip = ContentMask { bounds: rect(spot.x0, spot.shown.0, spot.x1, spot.shown.1).intersect(&area) };
        window.with_content_mask(Some(clip), |window| {
            if let Some(line) = fit_line(window, &name, &name_font, NAME_SIZE, text, width) {
                line.paint(origin, px(NAME_LINE), TextAlign::Left, None, window, cx).ok();
            }
            if spot.y1 - spot.y0 >= LABEL_TWO_LINES
                && let Some(line) = fit_line(window, &detail, &font, DETAIL_SIZE, text, width)
            {
                line.paint(point(origin.x, origin.y + px(NAME_LINE)), px(DETAIL_LINE), TextAlign::Left, None, window, cx).ok();
            }
        });
    }
}

/// Where a bar's label goes (see `label_spot`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Spot {
    /// The whole bar, inside its gap and below the first row: the label sits on it and is
    /// fitted to it.
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    /// Top and bottom of the part that's showing, which the label is clipped to.
    shown: (f32, f32),
}

/// Where `painted[i]`'s label goes, if its bar is big enough for one and some of it is
/// showing: not below what the reveal has uncovered, nor under bars painted after it that
/// overlap it from above or below (a folder's contents sliding down out from under it).
fn label_spot(geometry: &Geometry, painted: &[Painted], i: usize) -> Option<Spot> {
    let bar = &painted[i].band;
    let (x0, y0, x1, y1) = geometry.icicle_bar(bar.r0.max(geometry.inner_radius), bar.r1, bar.start, bar.end)?;
    // Cheap test first: most bars are far too small to label, and need no more work.
    if x1 - x0 < LABEL_MIN_WIDTH || y1 - y0 < LABEL_MIN_HEIGHT {
        return None;
    }
    let (mut top, mut bottom) = (y0, y1.min(bar.reach - GAP_PX / 2.0));
    // Neighbouring rows meet at an edge; only a real overlap counts.
    const SLACK: f32 = 0.5;
    for over in &painted[i + 1..] {
        let over = &over.band;
        if over.end <= bar.start + 1e-5 || over.start >= bar.end - 1e-5 || over.reach <= top + SLACK || over.r0 >= bottom - SLACK {
            continue;
        }
        if over.r0 <= top {
            top = over.reach + GAP_PX / 2.0;
        } else {
            bottom = over.r0 - GAP_PX / 2.0;
        }
    }
    (bottom > top).then_some(Spot { x0, y0, x1, y1, shown: (top, bottom) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Node;
    use std::path::PathBuf;

    fn node(name: &str, size: u64, kind: Kind, parent: Option<usize>, children: Vec<usize>) -> Node {
        Node { name: name.into(), size, kind, parent, children, items: 0 }
    }

    /// A large slice pinned after many thin ones (like "Not scanned yet") must keep its
    /// own segment instead of being folded into "smaller objects".
    #[test]
    fn large_slice_after_thin_ones_keeps_its_segment() {
        let mut nodes = vec![node("root", 0, Kind::Dir, None, Vec::new())];
        let mut total = 0;
        for (i, size) in [600u64, 1, 1, 1, 400].into_iter().enumerate() {
            let kind = if i == 4 { Kind::Other } else { Kind::File };
            nodes.push(node(&format!("c{i}"), size, kind, Some(0), Vec::new()));
            nodes[0].children.push(i + 1);
            total += size;
        }
        nodes[0].size = total;
        let tree = Tree { root_path: PathBuf::from("/"), nodes, errors: 0, cloud_only: 0 };
        let segments = layout(&tree, Tree::ROOT);
        let targets: Vec<Target> = segments.iter().map(|s| s.target).collect();
        assert!(targets.contains(&Target::Node(5)), "the large last slice has its own segment: {targets:?}");
        let last = segments.iter().find(|s| s.target == Target::Node(5)).unwrap();
        assert!((last.end - 1.0).abs() < 1e-4 && last.end - last.start > 0.39);

        let (start, end, ring) = frame_of(&tree, Tree::ROOT, 5).unwrap();
        assert!((start - last.start).abs() < 1e-4 && (end - last.end).abs() < 1e-4 && ring == 1.0);
    }

    /// Cut short at one level, the layout is `layout`'s top level, segment for segment.
    #[test]
    fn a_depth_limited_layout_is_the_top_of_the_full_one() {
        // root ── a (a1, a2), b, c (c1)
        let nodes = vec![
            node("root", 100, Kind::Dir, None, vec![1, 4, 5]),
            node("a", 60, Kind::Dir, Some(0), vec![2, 3]),
            node("a1", 40, Kind::File, Some(1), Vec::new()),
            node("a2", 20, Kind::File, Some(1), Vec::new()),
            node("b", 25, Kind::File, Some(0), Vec::new()),
            node("c", 15, Kind::Dir, Some(0), vec![6]),
            node("c1", 15, Kind::File, Some(5), Vec::new()),
        ];
        let tree = Tree { root_path: PathBuf::from("/"), nodes, errors: 0, cloud_only: 0 };
        let full = layout(&tree, Tree::ROOT);
        let top = layout_to(&tree, Tree::ROOT, 1);
        let full_top: Vec<_> = full.iter().filter(|s| s.depth == 1).map(|s| (s.target, s.start, s.end)).collect();
        assert_eq!(top.iter().map(|s| (s.target, s.start, s.end)).collect::<Vec<_>>(), full_top);
        assert!(full.len() > top.len() && top.iter().all(|s| s.depth == 1));
        assert_eq!(layout_to(&tree, Tree::ROOT, 2).len(), full.len(), "two levels is all this tree has");
        assert!(layout_to(&tree, Tree::ROOT, 0).is_empty());
    }

    fn bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(50.)), size(px(800.), px(600.)))
    }

    fn seg(depth: usize, start: f32, end: f32) -> Segment {
        Segment { target: Target::Node(0), depth, start, end, kind: Kind::Dir }
    }

    #[test]
    fn icicle_rows_are_contiguous_and_fill_the_height() {
        let g = Geometry::icicle(bounds());
        let Shape::Icicle { top, left, right } = g.shape else { panic!("an icicle") };
        assert_eq!((top, left, right), (102.0, 116.0, 884.0), "below the label strip, inside the margins");
        assert!((g.outer_radius() - 634.0).abs() < 1e-3, "down to the bottom margin");
        assert_eq!(g.icicle_rect(), Some(rect(116.0, 102.0, 884.0, 634.0)));
        assert!((g.rings[0].0 - g.inner_radius).abs() < 1e-3);
        let height = g.rings[0].1 - g.rings[0].0;
        assert!(((g.inner_radius - top) * 2.0 - height).abs() < 1e-3, "the centre is half a row");
        for pair in g.rings.windows(2) {
            assert!(pair[0].0 < pair[0].1 && (pair[0].1 - pair[1].0).abs() < 1e-3, "{:?}", g.rings);
            assert!((pair[1].1 - pair[1].0 - height).abs() < 1e-3, "rows are all the same height");
        }
    }

    #[test]
    fn icicle_hit_test_finds_bars_the_centre_and_nothing_outside() {
        let g = Geometry::icicle(bounds());
        let segments = [seg(1, 0.0, 0.5), seg(1, 0.5, 1.0), seg(2, 0.0, 0.25), seg(2, 0.25, 0.5)];
        let Shape::Icicle { top, left, right } = g.shape else { panic!("an icicle") };
        let at = |turns: f32, y: f32| point(px(left + (right - left) * turns), px(y));
        let row = |depth: usize| (g.rings[depth - 1].0 + g.rings[depth - 1].1) / 2.0;
        assert_eq!(g.hit_test(at(0.375, row(2)), &segments), Some(Hit::Segment(3)));
        assert_eq!(g.hit_test(at(0.75, row(1)), &segments), Some(Hit::Segment(1)), "turns run left to right");
        assert_eq!(g.hit_test(at(0.6, (top + g.inner_radius) / 2.0), &segments), Some(Hit::Center), "the bar across the top");
        assert_eq!(g.hit_test(at(0.1, row(3)), &segments), None, "nothing in that row");
        assert_eq!(g.hit_test(at(0.5, top - 10.0), &segments), None, "above, in the label strip");
        assert_eq!(g.hit_test(point(px(left - 4.0), px(row(1))), &segments), None);
        assert_eq!(g.hit_test(point(px(right + 4.0), px(row(1))), &segments), None);
        assert_eq!(g.hit_test(at(0.5, g.outer_radius() + 1.0), &segments), None);
    }

    /// What you point at is what's painted there: the middle and every corner of each bar as
    /// drawn hit that bar, and each bar runs across its row, below the centre.
    #[test]
    fn icicle_hit_test_agrees_with_painting() {
        let g = Geometry::icicle(bounds());
        let segments = [seg(1, 0.0, 0.6), seg(1, 0.6, 1.0), seg(2, 0.0, 0.1), seg(2, 0.1, 0.6), seg(3, 0.1, 0.35)];
        for (i, s) in segments.iter().enumerate() {
            let (r0, r1) = g.rings[s.depth - 1];
            let (x0, y0, x1, y1) = g.icicle_bar(r0, r1, s.start, s.end).unwrap();
            assert!(y0 > g.inner_radius && (y1 - y0 - (r1 - r0 - GAP_PX)).abs() < 1e-3, "the whole row, below the centre: {s:?}");
            let inside = 0.1;
            for (x, y) in [((x0 + x1) / 2.0, (y0 + y1) / 2.0), (x0 + inside, y0 + inside), (x1 - inside, y0 + inside), (x0 + inside, y1 - inside), (x1 - inside, y1 - inside)] {
                assert_eq!(g.hit_test(point(px(x), px(y)), &segments), Some(Hit::Segment(i)), "{s:?} at {x}, {y}");
            }
        }
        // And the bar across the top is the centre all the way along.
        let Shape::Icicle { top, left, right } = g.shape else { unreachable!() };
        for x in [left, (left + right) / 2.0, right - 0.1] {
            assert_eq!(g.hit_test(point(px(x), px(top)), &segments), Some(Hit::Center));
        }
    }

    #[test]
    fn sunburst_hit_test_is_unchanged() {
        let g = Geometry::new(bounds());
        let center = bounds().center();
        let segments = [seg(1, 0.0, 0.5), seg(1, 0.5, 1.0)];
        let ring = (g.rings[0].0 + g.rings[0].1) / 2.0;
        assert_eq!(g.hit_test(center, &segments), Some(Hit::Center));
        assert_eq!(g.hit_test(point(center.x + px(ring), center.y), &segments), Some(Hit::Segment(0)), "3 o'clock is a quarter turn");
        assert_eq!(g.hit_test(point(center.x - px(ring), center.y), &segments), Some(Hit::Segment(1)));
        assert_eq!(g.hit_test(point(center.x + px(g.outer_radius() + 2.0), center.y), &segments), None);
    }

    #[test]
    fn bands_move_continuously_through_the_first_and_last_ring() {
        for g in [Geometry::new(bounds()), Geometry::icicle(bounds())] {
            for edge in [1.0, MAX_DEPTH as f32] {
                let (below, above, at) = (g.band_at(edge - 1e-4), g.band_at(edge + 1e-4), g.band_at(edge));
                for band in [below, above] {
                    assert!((band.0 - at.0).abs() < 0.1 && (band.1 - at.1).abs() < 0.1 && (band.2 - at.2).abs() < 1e-3, "{g:?} at {edge}: {band:?} vs {at:?}");
                }
            }
            // All the way into the centre, and off past the edge, it's gone.
            let (r0, r1, alpha) = g.band_at(0.0);
            assert!(alpha == 0.0 && r0 <= r1 && r1 <= g.inner_radius + 1e-3, "{g:?}: {r0} {r1}");
            assert_eq!(g.band_at(MAX_DEPTH as f32 + 1.0).2, 0.0);
        }
        // The icicle's bands slide up into its first row rather than past its top edge, and
        // fall off the bottom past the last.
        let g = Geometry::icicle(bounds());
        let Shape::Icicle { top, .. } = g.shape else { unreachable!() };
        assert_eq!(g.band_at(0.0).0, top);
        assert!(g.band_at(-3.0).1 >= top);
        let (gone, _, _) = g.band_at(MAX_DEPTH as f32 + 1.0);
        assert!((gone - g.outer_radius()).abs() < 1e-3, "just below the bottom: {gone}");
        let (higher, lower) = (g.band_at(1.5), g.band_at(2.0));
        assert!(higher.0 < lower.0 && higher.1 < lower.1, "deeper is lower down");
    }

    /// A band falling off the bottom is cut off at the chart's bottom edge as it fades, so it
    /// never paints over the legend under the chart; in the rows it's the whole bar.
    #[test]
    fn icicle_bands_leaving_the_last_row_stay_inside_the_chart() {
        let g = Geometry::icicle(bounds());
        let bottom = g.outer_radius() - GAP_PX / 2.0;
        for step in 0..=20 {
            let depth = MAX_DEPTH as f32 + step as f32 / 20.0;
            let (r0, r1, _) = g.band_at(depth);
            if let Some((_, y0, _, y1)) = g.icicle_drawn(r0, r1, 0.2, 0.7) {
                assert!(y0 < y1 && y1 <= bottom + 1e-3, "at depth {depth}: {y0}..{y1}, bottom {bottom}");
            }
        }
        assert_eq!(g.icicle_drawn(g.outer_radius(), g.outer_radius() + 50.0, 0.2, 0.7), None, "all the way off, nothing's left");
        for depth in 1..=MAX_DEPTH {
            let (r0, r1) = g.rings[depth - 1];
            assert_eq!(g.icicle_drawn(r0, r1, 0.2, 0.7), g.icicle_bar(r0, r1, 0.2, 0.7));
        }
    }

    #[test]
    fn labels_contrast_with_their_bars() {
        let light = label_color(hsla(0.3, 0.5, 0.8, 1.0), 1.0);
        let dark = label_color(hsla(0.3, 0.5, 0.3, 1.0), 0.5);
        assert!(light.lightness < 0.2 && (light.alpha - 0.85).abs() < 1e-4);
        assert!(dark.lightness > 0.9 && (dark.alpha - 0.46).abs() < 1e-4, "faded with the bar");
        // A first-ring folder's yellow and blue (`base_color`): same lightness, but the yellow
        // looks far lighter, and white on it would barely read.
        assert!(label_color(hsla(0.17, 0.7, 0.52, 1.0), 1.0).lightness < 0.2, "dark text on yellow");
        assert!(label_color(hsla(0.66, 0.7, 0.52, 1.0), 1.0).lightness > 0.9, "light text on blue");
    }

    fn bar(index: usize, r0: f32, r1: f32, start: f32, end: f32) -> Painted {
        Painted { index, band: Band { r0, r1, reach: r1, start, end, color: hsla(0.5, 0.5, 0.5, 1.0), alpha: 1.0 } }
    }

    /// A folder's contents sliding down out from under it: the label sits on the whole bar,
    /// and is only clipped to where it shows.
    #[test]
    fn labels_keep_to_the_part_of_a_bar_that_shows() {
        let g = Geometry::icicle(bounds());
        let Shape::Icicle { left, right, .. } = g.shape else { unreachable!() };
        let (c1, c2) = (g.rings[0], g.rings[1]);
        let half = (c2.1 - c2.0) / 2.0;
        // The child is halfway out from under its parent, which is painted over it.
        let child = bar(1, c1.0 + half, c1.1 + half, 0.0, 0.5);
        let parent = bar(0, c1.0, c1.1, 0.0, 0.5);
        let spot = label_spot(&g, &[child, parent], 0).unwrap();
        assert!((spot.y0 - (c1.0 + half + GAP_PX / 2.0)).abs() < 1e-3 && (spot.y1 - (c1.1 + half - GAP_PX / 2.0)).abs() < 1e-3, "{spot:?}");
        assert!((spot.x0 - (left + GAP_PX / 2.0)).abs() < 1e-3 && (spot.x1 - ((left + right) / 2.0 - GAP_PX / 2.0)).abs() < 1e-3, "as wide as its share: {spot:?}");
        assert!(spot.shown.0 > c1.1 && (spot.shown.1 - spot.y1).abs() < 1e-3, "{spot:?}");
        // Neighbouring rows don't hide each other.
        let next = bar(2, c2.0, c2.1, 0.0, 0.25);
        assert!((label_spot(&g, &[parent, next], 0).unwrap().shown.1 - (c1.1 - GAP_PX / 2.0)).abs() < 1e-3);
        // Nor does anything in the first row get a label.
        let sunk = bar(3, g.inner_radius - 30.0, g.inner_radius, 0.0, 1.0);
        assert_eq!(label_spot(&g, &[sunk], 0), None);
        // Too narrow for a few letters, even though there's height to spare.
        let turns = |width: f32| (width + GAP_PX) / (right - left);
        assert_eq!(label_spot(&g, &[bar(0, c1.0, c1.1, 0.0, 0.01)], 0), None);
        assert_eq!(label_spot(&g, &[bar(0, c1.0, c1.1, 0.5, 0.5 + turns(LABEL_MIN_WIDTH - 1.0))], 0), None);
        assert!(label_spot(&g, &[bar(0, c1.0, c1.1, 0.5, 0.5 + turns(LABEL_MIN_WIDTH + 1.0))], 0).is_some());
        // Too short (a bar on its way into the first row), however wide.
        let short = c1.0 + LABEL_MIN_HEIGHT - 1.0 + GAP_PX;
        assert!(short - c1.0 - GAP_PX > NAME_SIZE);
        assert_eq!(label_spot(&g, &[bar(0, c1.0, short, 0.0, 1.0)], 0), None, "descenders would be cut off");
        // A whole row has room for the size under the name.
        assert!(c1.1 - c1.0 - GAP_PX >= LABEL_TWO_LINES);
    }

    /// While the reveal uncovers a bar from the top down, its label is fitted to the whole bar
    /// (so the text doesn't change as more of it shows) and clipped to what's uncovered.
    #[test]
    fn labels_are_uncovered_with_their_bars() {
        let g = Geometry::icicle(bounds());
        let Shape::Icicle { left, right, .. } = g.shape else { unreachable!() };
        let (r0, r1) = g.rings[0];
        let revealing = Painted { band: Band { reach: r0 + 10.0, ..bar(0, r0, r1, 0.0, 0.5).band }, ..bar(0, r0, r1, 0.0, 0.5) };
        let spot = label_spot(&g, &[revealing], 0).expect("a label, partly uncovered");
        assert!((spot.y1 - spot.y0 - (r1 - r0 - GAP_PX)).abs() < 1e-3, "fitted to the whole bar: {spot:?}");
        assert!((spot.x1 - spot.x0 - ((right - left) / 2.0 - GAP_PX)).abs() < 1e-3, "{spot:?}");
        assert!((spot.shown.0 - spot.y0).abs() < 1e-3 && (spot.shown.1 - (r0 + 10.0 - GAP_PX / 2.0)).abs() < 1e-3, "clipped to what's uncovered: {spot:?}");
        // Nothing uncovered yet: no label.
        let hidden = Painted { band: Band { reach: r0, ..revealing.band }, ..revealing };
        assert_eq!(label_spot(&g, &[hidden], 0), None);
    }

    #[test]
    fn names_with_newlines_become_one_line() {
        assert_eq!(single_line(&"Icon\r".into()).as_ref(), "Icon ");
        assert_eq!(single_line(&"two\nlines\tand a tab".into()).as_ref(), "two lines and a tab");
        assert_eq!(single_line(&"plain…".into()).as_ref(), "plain…");
    }
}
