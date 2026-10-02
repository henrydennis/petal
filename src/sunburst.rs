//! Sunburst layout, painting and hit testing.
//!
//! Angles are measured in turns (0..1), clockwise from 12 o'clock.

use std::f32::consts::TAU;

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, hsla, point, px};

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
    let mut out = Vec::new();
    layout_children(tree, focus, 1, 0.0, 1.0, &mut out);
    out
}

fn layout_children(tree: &Tree, ix: usize, depth: usize, start: f32, end: f32, out: &mut Vec<Segment>) {
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
        if child.kind == Kind::Dir && depth < MAX_DEPTH {
            layout_children(tree, child_ix, depth + 1, angle, angle + width, out);
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

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub center: Point<Pixels>,
    pub inner_radius: f32,
    /// Inner and outer radius of each ring.
    pub rings: [(f32, f32); MAX_DEPTH],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Center,
    Segment(usize),
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
        Self { center: bounds.center(), inner_radius, rings }
    }

    /// A single ring, for small gauges.
    pub fn ring(center: Point<Pixels>, inner: f32, outer: f32) -> Self {
        Self { center, inner_radius: inner, rings: [(inner, outer); MAX_DEPTH] }
    }

    pub fn outer_radius(&self) -> f32 {
        self.rings[MAX_DEPTH - 1].1
    }

    pub fn hit_test(&self, position: Point<Pixels>, segments: &[Segment]) -> Option<Hit> {
        let dx = f32::from(position.x - self.center.x);
        let dy = f32::from(position.y - self.center.y);
        let distance = (dx * dx + dy * dy).sqrt();
        if distance < self.inner_radius {
            return Some(Hit::Center);
        }
        let depth = self.rings.iter().position(|(r0, r1)| distance >= *r0 && distance < *r1)? + 1;
        let turns = dx.atan2(-dy).rem_euclid(TAU) / TAU;
        segments
            .iter()
            .position(|s| s.depth == depth && turns >= s.start && turns < s.end)
            .map(Hit::Segment)
    }

    fn at(&self, radius: f32, turns: f32) -> Point<Pixels> {
        let theta = turns * TAU;
        point(
            self.center.x + px(radius * theta.sin()),
            self.center.y - px(radius * theta.cos()),
        )
    }

    /// Paint an annular sector, leaving a hairline gap around it.
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
    /// ring a band sinks behind the centre, fading; beyond the last it moves out past the
    /// edge, fading. That's where the camera takes segments when zooming.
    pub fn band_at(&self, depth: f32) -> (f32, f32, f32) {
        let last = MAX_DEPTH as f32;
        if depth < 1.0 {
            let (r0, r1) = self.rings[0];
            let shift = (1.0 - depth) * (r1 - r0);
            ((r0 - shift).max(0.0), (r1 - shift).max(0.0), depth.clamp(0.0, 1.0))
        } else if depth > last {
            let (r0, r1) = self.rings[MAX_DEPTH - 1];
            let shift = (depth - last) * (r1 - r0);
            (r0 + shift, r1 + shift, (last + 1.0 - depth).clamp(0.0, 1.0))
        } else {
            let (r0, r1) = self.ring_at(depth);
            (r0, r1, 1.0)
        }
    }

    /// Paint an annular sector between two radii, leaving a hairline gap around it.
    pub fn paint_band(&self, window: &mut Window, r0: f32, r1: f32, start: f32, end: f32, color: Hsla) {
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
        builder.move_to(self.at(r1, outer_start));
        for i in 1..=steps {
            let t = outer_start + (outer_end - outer_start) * i as f32 / steps as f32;
            builder.line_to(self.at(r1, t));
        }
        for i in 0..=steps {
            let t = inner_end - (inner_end - inner_start) * i as f32 / steps as f32;
            builder.line_to(self.at(r0, t));
        }
        builder.close();
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    pub fn paint_disc(&self, window: &mut Window, radius: f32, color: Hsla) {
        let mut builder = PathBuilder::fill();
        let steps = 180;
        builder.move_to(self.at(radius, 0.0));
        for i in 1..steps {
            builder.line_to(self.at(radius, i as f32 / steps as f32));
        }
        builder.close();
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }
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
}
