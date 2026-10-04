//! Motion for the chart. The chart moves as one piece: every edge of every segment
//! follows the same critically damped spring, so whenever the layout changes (a new
//! snapshot mid-scan, zooming into a folder, items moved to the Trash) the whole picture
//! morphs together, without gaps opening or segments sliding over each other. Segments
//! are matched across layouts by folder path, so a folder keeps its identity as it grows,
//! moves, or slides between rings.
//!
//! - New segments enter where they belong in the chart as drawn: at the edge they share
//!   with their previous sibling, or, when a folder's contents appear for the first time,
//!   sliding out from under the folder.
//! - Segments that are gone close up where their neighbours meet.
//! - Segments that change places among their siblings (the results re-sort as sizes change)
//!   close up where they were and open up where they're going, rather than sliding across
//!   their neighbours.
//! - Changing the folder in focus moves a camera (`Camera`): one transform for the whole
//!   chart, so the folder opens out to fill the circle and its parents sink into the centre.
//! - The very first layout is revealed outward from the centre, all at once.
//! - When the scan finishes, its chart is re-sorted into the results' order, which moves
//!   almost everything. Rather than shuffle, the scan's chart folds away clockwise, smallest
//!   first, while the results open clockwise behind it at their true sizes, largest first
//!   (`fold`).
//!
//! This drives the sunburst and the icicle alike, through `Geometry`, in the rings and turns
//! `sunburst` describes: on the icicle, "outward" is top to bottom (down from the focus
//! folder's bar) and "clockwise" is left to right. The treemap has its own motion,
//! `treemap::TreemapMotion`, on the same principles.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gpui::{Hsla, SharedString, Window, hsla};

use crate::scan::Tree;
use crate::sunburst::{self, Band, Geometry, Painted, Segment};

/// A segment's identity across layouts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Node(Vec<SharedString>),
    /// A "smaller objects" sliver, by the path of the first item in it.
    Small(Vec<SharedString>),
    /// A segment on its way out of a place it moved away from (see `retire`).
    Retired(u64),
}

impl Key {
    /// Whether this is something inside the folder `self`.
    fn contains(&self, other: &Key) -> bool {
        match (self, other) {
            (Key::Node(folder), Key::Node(path) | Key::Small(path)) => path.len() > folder.len() && path.starts_with(folder),
            _ => false,
        }
    }
}

/// Stiffness of the chart's spring: a change settles in about half a second, starting
/// and finishing gently.
const OMEGA: f32 = 13.0;
const SETTLED: f32 = 1e-4;
/// How long the first layout takes to be revealed.
const REVEAL_SECONDS: f32 = 0.9;
/// The revealing edge fades in over this many pixels instead of being a hard line.
const REVEAL_EDGE: f32 = 28.0;
/// Folding the scan's chart away: each top-level segment closes over this long, smallest
/// first, their starts spread over `FOLD_STAGGER` in proportion to size so the chart winds
/// down at an even pace (a crowd of slivers goes almost at once; the big ones take turns).
/// The results open into the room it leaves, so this is the whole handover.
const FOLD_EACH: f32 = 0.35;
const FOLD_STAGGER: f32 = 0.65;

/// A critically damped spring: no overshoot, and a change of target mid-flight carries
/// on smoothly from the current speed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spring {
    pub(crate) x: f32,
    pub(crate) v: f32,
    pub(crate) target: f32,
}

impl Spring {
    pub(crate) fn still(x: f32, target: f32) -> Self {
        Self { x, v: 0.0, target }
    }

    /// Start from where `other` is, moving as it moves.
    fn from(other: Spring, target: f32) -> Self {
        Self { x: other.x, v: other.v, target }
    }

    /// Exact solution over `dt`, so it's stable for any frame time. It's linear in the
    /// state, so edges that start together and share a target stay together.
    pub(crate) fn step(&mut self, dt: f32) {
        let offset = self.x - self.target;
        let c = self.v + OMEGA * offset;
        let decay = (-OMEGA * dt).exp();
        self.x = self.target + (offset + c * dt) * decay;
        self.v = (self.v - OMEGA * c * dt) * decay;
    }

    pub(crate) fn settled(&self) -> bool {
        (self.x - self.target).abs() + self.v.abs() / OMEGA < SETTLED
    }
}

/// One segment on its way to (or away from) its place in the latest layout.
struct Motion {
    start: Spring,
    end: Spring,
    depth: Spring,
    /// Index into the latest layout's segments; `None` once the segment has left it.
    current: Option<usize>,
    /// Colour last drawn with, for segments that are on their way out.
    color: Hsla,
    /// Neighbours in the last layout it was part of, so it knows where to close up.
    next: Option<Key>,
    parent: Option<Key>,
}

impl Motion {
    fn settled(&self) -> bool {
        self.start.settled() && self.end.settled() && self.depth.settled()
    }
}

/// A move between the charts of two folders, as one transform of the whole chart:
/// angle → angle × scale + offset, ring → ring + shift.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    scale: f32,
    offset: f32,
    shift: f32,
}

impl Camera {
    /// From the chart centred on `from` to the chart centred on `to`.
    pub fn between(tree: &Tree, from: usize, to: usize) -> Option<Self> {
        if from == to {
            return None;
        }
        // Lay both out from their nearest common folder.
        let mut common = to;
        while !tree.is_ancestor_or_self(common, from) {
            common = tree.nodes[common].parent?;
        }
        let (o0, o1, od) = sunburst::frame_of(tree, common, from)?;
        let (n0, n1, nd) = sunburst::frame_of(tree, common, to)?;
        let span = n1 - n0;
        if span <= 0.0 || o1 <= o0 {
            return None;
        }
        Some(Self { scale: (o1 - o0) / span, offset: (o0 - n0) / span, shift: od - nd })
    }

    fn angle(&self, turns: f32) -> f32 {
        (turns * self.scale + self.offset).clamp(0.0, 1.0)
    }

    fn unangle(&self, turns: f32) -> f32 {
        ((turns - self.offset) / self.scale).clamp(0.0, 1.0)
    }
}

/// A top-level segment (with everything in it) closing or opening in the fold.
#[derive(Clone, Copy, Debug)]
struct Group {
    start: f32,
    width: f32,
    /// Seconds into the fold that it starts to close.
    from: f32,
}

/// Something of the scan's chart, as it was last drawn, folding away.
#[derive(Clone, Copy, Debug)]
struct Piece {
    start: f32,
    end: f32,
    depth: f32,
    color: Hsla,
    group: usize,
}

/// The scan's chart folding away and the results opening (see `ChartMotion::fold`).
struct Fold {
    /// Seconds in.
    t: f32,
    old: Vec<Piece>,
    old_groups: Vec<Group>,
    /// How long it takes (it depends on the sizes).
    length: f32,
    new_groups: Vec<Group>,
    /// The next layout has arrived (it may be empty: an empty folder).
    opened: bool,
    /// Which of `new_groups` each segment of the results belongs to.
    new_group_of: HashMap<Key, usize>,
}

impl Fold {
    /// Where the scan's chart is now: each group's (end, how open), and its front edge.
    /// Groups close clockwise, towards their end, and everything before one moves round to
    /// take up the slack, so the chart is pushed round into the top. (If it didn't reach
    /// all the way round, it drifts there as it goes.)
    fn closing(&self) -> (Vec<(f32, f32)>, f32) {
        let groups = &self.old_groups;
        let open: Vec<f32> = groups.iter().map(|g| 1.0 - smootherstep((self.t - g.from) / FOLD_EACH)).collect();
        let total: f32 = groups.iter().map(|g| g.width).sum();
        let left: f32 = groups.iter().zip(&open).map(|(g, k)| g.width * k).sum();
        if total <= SETTLED {
            return (vec![(1.0, 0.0); groups.len()], smootherstep(self.t / FOLD_EACH));
        }
        let end = groups.iter().map(|g| g.start + g.width).fold(0.0, f32::max);
        let drift = (1.0 - end) * (1.0 - left / total);
        let mut by_start: Vec<usize> = (0..groups.len()).collect();
        by_start.sort_by(|&a, &b| groups[b].start.total_cmp(&groups[a].start));
        let mut out = vec![(0.0, 0.0); groups.len()];
        let mut slack = 0.0;
        let mut front = 1.0f32;
        for i in by_start {
            let at = groups[i].start + groups[i].width + drift + slack;
            out[i] = (at, open[i]);
            front = front.min(at - groups[i].width * open[i]);
            slack += groups[i].width * (1.0 - open[i]);
        }
        (out, front.clamp(0.0, 1.0))
    }
}

/// When each group starts: in order of `rank`, spread over `stagger` in proportion to the
/// size of the groups that go before it.
fn schedule(groups: &mut [Group], stagger: f32, rank: impl Fn(&Group, &Group) -> std::cmp::Ordering) {
    let total: f32 = groups.iter().map(|g| g.width).sum::<f32>().max(1e-6);
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&a, &b| rank(&groups[a], &groups[b]));
    let mut before = 0.0;
    for i in order {
        groups[i].from = stagger * before / total;
        before += groups[i].width;
    }
}

/// The results filling the circle up to `reach`, clockwise from the top: each group as
/// where its start is drawn and how open it is. Groups open in order round the circle,
/// which for the results is largest first.
fn filling(groups: &[Group], reach: f32) -> Vec<(f32, f32)> {
    let mut by_start: Vec<usize> = (0..groups.len()).collect();
    by_start.sort_by(|&a, &b| groups[a].start.total_cmp(&groups[b].start));
    let mut out = vec![(0.0, 1.0); groups.len()];
    let (mut before, mut slack) = (0.0, 0.0);
    for i in by_start {
        let width = groups[i].width;
        let k = if width > 0.0 { ((reach - before) / width).clamp(0.0, 1.0) } else { 1.0 };
        out[i] = (groups[i].start - slack, k);
        before += width;
        slack += width * (1.0 - k);
    }
    out
}

pub struct ChartMotion {
    segments: HashMap<Key, Motion>,
    last_frame: Option<Instant>,
    /// Which layout the targets come from, to retarget only on change.
    layout_id: usize,
    /// Share of the chart it covers, in turns (volume scans fill it as they go).
    fraction: Spring,
    /// Applied to the next layout: the focus changed.
    camera: Option<Camera>,
    /// Seconds into revealing the first layout; `None` once done (or not started).
    reveal: Option<f32>,
    revealed: bool,
    fold: Option<Fold>,
    /// Numbers `Key::Retired`.
    retired: u64,
}

impl Default for ChartMotion {
    fn default() -> Self {
        Self {
            segments: HashMap::new(),
            last_frame: None,
            layout_id: 0,
            fraction: Spring::still(1.0, 1.0),
            camera: None,
            reveal: None,
            revealed: false,
            fold: None,
            retired: 0,
        }
    }
}

impl ChartMotion {
    /// The next layout is centred on a different folder: move there as a camera would.
    pub fn zoom(&mut self, camera: Option<Camera>) {
        self.camera = camera;
    }

    /// Fold the chart as drawn away, and open the next layout in its place: for a layout so
    /// differently ordered (the scan's chart re-sorted by size) that morphing into it would
    /// be a shuffle. The chart closes clockwise, smallest top-level segment first, and the
    /// new one opens clockwise from the top into the room it leaves, largest first, at its
    /// true sizes.
    pub fn fold(&mut self) {
        let fraction = self.fraction.x;
        // Everything takes its top-level segment's turn.
        let top_of = |key: &Key| -> Key {
            let mut at = key;
            for _ in 0..64 {
                match self.segments.get(at).and_then(|m| m.parent.as_ref()).filter(|p| self.segments.contains_key(*p)) {
                    Some(parent) => at = parent,
                    None => break,
                }
            }
            at.clone()
        };
        let mut groups: Vec<Group> = Vec::new();
        let mut group_of: HashMap<Key, usize> = HashMap::new();
        let mut old = Vec::new();
        for (key, motion) in &self.segments {
            if motion.end.x - motion.start.x <= SETTLED {
                continue;
            }
            let top = top_of(key);
            let group = *group_of.entry(top.clone()).or_insert_with(|| {
                let m = &self.segments[&top];
                groups.push(Group { start: m.start.x * fraction, width: (m.end.x - m.start.x).max(0.0) * fraction, from: 0.0 });
                groups.len() - 1
            });
            old.push(Piece { start: motion.start.x * fraction, end: motion.end.x * fraction, depth: motion.depth.x, color: motion.color, group });
        }
        // Smallest first; of equal ones, the one nearer the top first (clockwise).
        schedule(&mut groups, FOLD_STAGGER, |a, b| a.width.total_cmp(&b.width).then(a.start.total_cmp(&b.start)));
        let length = groups.iter().map(|g| g.from).fold(0.0, f32::max) + FOLD_EACH;
        self.fold = Some(Fold { t: 0.0, old, old_groups: groups, length, new_groups: Vec::new(), opened: false, new_group_of: HashMap::new() });
        self.segments.clear();
        self.camera = None;
        self.layout_id = 0;
    }

    /// Whether the chart is folding: what's drawn isn't where things are, so it shouldn't
    /// respond to the pointer.
    pub fn folding(&self) -> bool {
        self.fold.is_some()
    }

    pub fn fraction(&self) -> f32 {
        self.fraction.x
    }

    /// Aim every segment at its place in a new layout.
    pub fn retarget(&mut self, layout_id: usize, keys: &[Key], segments: &[Segment], fraction: f32) {
        if !self.revealed {
            // Nothing to grow from yet: start at the first real share instead.
            self.fraction = Spring::still(fraction, fraction);
        }
        self.fraction.target = fraction;
        if self.layout_id == layout_id {
            return;
        }
        self.layout_id = layout_id;
        let camera = self.camera.take();
        // In place straight away: the very first layout (revealed from the centre), and the
        // one opening in a fold.
        let first = !self.revealed || (self.fold.is_some() && self.segments.is_empty());
        if !self.revealed && !segments.is_empty() && self.fold.is_none() {
            self.reveal = Some(0.0);
        }
        self.revealed |= !segments.is_empty();
        if let Some(fold) = &mut self.fold {
            // The opening goes by the new layout's top-level segments.
            fold.new_groups.clear();
            fold.new_group_of.clear();
            fold.opened = true;
            for (key, segment) in keys.iter().zip(segments) {
                if segment.depth == 1 {
                    fold.new_groups.push(Group { start: segment.start, width: segment.end - segment.start, from: 0.0 });
                }
                if let Some(group) = fold.new_groups.len().checked_sub(1) {
                    fold.new_group_of.insert(key.clone(), group);
                }
            }
        }

        // Each segment's parent and neighbouring siblings, by index. Layouts are in
        // depth-first order, so the parent and previous sibling come first.
        let mut parent = vec![None; segments.len()];
        let mut prev = vec![None; segments.len()];
        let mut next = vec![None; segments.len()];
        let mut last: [Option<usize>; sunburst::MAX_DEPTH + 2] = Default::default();
        for (i, segment) in segments.iter().enumerate() {
            let depth = segment.depth;
            parent[i] = if depth > 1 { last[depth - 1] } else { None };
            prev[i] = last[depth];
            if let Some(p) = prev[i] {
                next[p] = Some(i);
            }
            last[depth] = Some(i);
            last[depth + 1..].fill(None);
        }
        if camera.is_none() && !first {
            self.retire_reordered(keys, &parent);
        }
        // Parents (None: the centre) that already have something on screen.
        let shown: HashSet<Option<usize>> =
            keys.iter().zip(&parent).filter(|(key, _)| self.segments.contains_key(key)).map(|(_, p)| *p).collect();

        for motion in self.segments.values_mut() {
            motion.current = None;
        }
        for (i, (key, segment)) in keys.iter().zip(segments).enumerate() {
            let (start, end, depth) = (segment.start, segment.end, segment.depth as f32);
            let relations = (next[i].map(|n| keys[n].clone()), parent[i].map(|p| keys[p].clone()));
            if let Some(motion) = self.segments.get_mut(key) {
                motion.start.target = start;
                motion.end.target = end;
                motion.depth.target = depth;
                motion.current = Some(i);
                (motion.next, motion.parent) = relations;
                continue;
            }
            let springs = if let Some(camera) = camera {
                // Where it would have been in the old chart, had it been drawn.
                (
                    Spring::still(camera.unangle(start), start),
                    Spring::still(camera.unangle(end), end),
                    Spring::still(depth - camera.shift, depth),
                )
            } else if first {
                // The reveal brings in the whole first chart at once.
                (Spring::still(start, start), Spring::still(end, end), Spring::still(depth, depth))
            } else if let (Some(p), false) = (parent[i], shown.contains(&parent[i])) {
                // A folder's contents appearing for the first time slide out from under it.
                let host = &self.segments[&keys[p]];
                let (p0, p1) = (segments[p].start, segments[p].end);
                let along = |turns: f32| {
                    let t = if p1 > p0 { (turns - p0) / (p1 - p0) } else { 0.0 };
                    Spring {
                        x: host.start.x + (host.end.x - host.start.x) * t,
                        v: host.start.v + (host.end.v - host.start.v) * t,
                        target: turns,
                    }
                };
                (along(start), along(end), Spring::from(host.depth, depth))
            } else {
                // Open up at the edge shared with the previous sibling.
                let (edge, ring) = match (prev[i], parent[i]) {
                    (Some(p), _) => {
                        let sibling = &self.segments[&keys[p]];
                        (sibling.end, sibling.depth)
                    }
                    (None, Some(p)) => {
                        let host = &self.segments[&keys[p]];
                        (host.start, Spring { x: host.depth.x + 1.0, ..host.depth })
                    }
                    (None, None) => (Spring::still(start, start), Spring::still(depth, depth)),
                };
                (Spring::from(edge, start), Spring::from(edge, end), Spring::from(ring, depth))
            };
            self.segments.insert(
                key.clone(),
                Motion {
                    start: springs.0,
                    end: springs.1,
                    depth: springs.2,
                    current: Some(i),
                    color: hsla(0., 0., 0., 0.),
                    next: relations.0,
                    parent: relations.1,
                },
            );
        }

        // Segments that are gone.
        let gone: Vec<Key> = self.segments.iter().filter(|(_, m)| m.current.is_none()).map(|(k, _)| k.clone()).collect();
        // Where each folder's (None: the centre's) contents now end.
        let mut ends: HashMap<Option<&Key>, f32> = HashMap::new();
        for (i, segment) in segments.iter().enumerate() {
            ends.insert(parent[i].map(|p| &keys[p]), segment.end);
        }
        let mut memo = HashMap::new();
        let targets: Vec<Option<(f32, f32, f32)>> = gone
            .iter()
            .map(|key| {
                let motion = &self.segments[key];
                match camera {
                    // Carried along by the camera: out past the edges, into the centre, or
                    // beyond the outer ring.
                    Some(camera) => Some((
                        camera.angle(motion.start.target),
                        camera.angle(motion.end.target),
                        motion.depth.target + camera.shift,
                    )),
                    None => self.close_up(key, &ends, &mut memo).map(|at| (at, at, motion.depth.target)),
                }
            })
            .collect();
        for (key, target) in gone.iter().zip(targets) {
            if let (Some((start, end, depth)), Some(motion)) = (target, self.segments.get_mut(key)) {
                motion.start.target = start;
                motion.end.target = end;
                motion.depth.target = depth;
            }
        }
    }

    /// Find segments whose order among their siblings changed, and retire them: keep the
    /// largest set still in order where they are, and move the rest.
    fn retire_reordered(&mut self, keys: &[Key], parent: &[Option<usize>]) {
        let mut groups: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
        for (i, key) in keys.iter().enumerate() {
            if self.segments.contains_key(key) {
                groups.entry(parent[i]).or_default().push(i);
            }
        }
        let mut moved = Vec::new();
        for group in groups.values() {
            let was: Vec<f32> = group.iter().map(|&i| self.segments[&keys[i]].start.target).collect();
            let in_order = longest_in_order(&was);
            moved.extend(group.iter().zip(in_order).filter(|(_, kept)| !kept).map(|(&i, _)| keys[i].clone()));
        }
        for key in moved {
            self.retire(&key);
        }
    }

    /// Send a segment, and everything inside it, on its way out under new names, so the
    /// key is free to come in fresh at its new place.
    fn retire(&mut self, key: &Key) {
        let going: Vec<Key> = self.segments.keys().filter(|k| *k == key || key.contains(k)).cloned().collect();
        let mut renames = HashMap::new();
        for old in going {
            renames.insert(old, Key::Retired(self.retired));
            self.retired += 1;
        }
        let rename = |key: Option<Key>| key.map(|k| renames.get(&k).cloned().unwrap_or(k));
        for (old, new) in &renames {
            if let Some(mut motion) = self.segments.remove(old) {
                motion.current = None;
                // Close up alongside the rest of what's moving, where it was.
                motion.next = rename(motion.next.take());
                motion.parent = rename(motion.parent.take());
                self.segments.insert(new.clone(), motion);
            }
        }
    }

    /// Where a departed segment closes up: the edge its next sibling starts at, or (when
    /// it was last) where its siblings now end, following departed neighbours along. That's
    /// the far edge of anything new arriving in its place, so the two never overlap.
    fn close_up(&self, key: &Key, ends: &HashMap<Option<&Key>, f32>, memo: &mut HashMap<Key, Option<f32>>) -> Option<f32> {
        if let Some(at) = memo.get(key) {
            return *at;
        }
        // Neighbours from different layouts could in principle point at each other.
        memo.insert(key.clone(), None);
        let motion = self.segments.get(key)?;
        let edge = |key: &Key, from_end: bool, memo: &mut HashMap<Key, Option<f32>>| match self.segments.get(key) {
            Some(m) if m.current.is_some() => Some(if from_end { m.end.target } else { m.start.target }),
            Some(_) => self.close_up(key, ends, memo),
            None => None,
        };
        let at = motion
            .next
            .as_ref()
            .and_then(|n| edge(n, false, memo))
            .or_else(|| ends.get(&motion.parent.as_ref()).copied())
            .or_else(|| motion.parent.as_ref().and_then(|p| edge(p, true, memo)))
            .unwrap_or((motion.start.target + motion.end.target) / 2.0);
        memo.insert(key.clone(), Some(at));
        Some(at)
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
        if let Some(fold) = &mut self.fold {
            fold.t += dt;
            moving = true;
            if fold.t >= fold.length && fold.opened {
                self.fold = None;
            }
        }
        self.segments.retain(|_, motion| {
            motion.start.step(dt);
            motion.end.step(dt);
            motion.depth.step(dt);
            let settled = motion.settled();
            moving |= !settled;
            // Departed segments go once they've closed up or been carried off.
            motion.current.is_some() || (!settled && motion.end.x - motion.start.x > SETTLED)
        });
        moving
    }

    /// Paint every segment where it is right now, and say where the segments of the latest
    /// layout went (for labels). `color` gets the segment's index in the latest layout and
    /// its current angles (so hues can follow the motion).
    pub fn paint(&mut self, window: &mut Window, geometry: &Geometry, color: impl FnMut(usize, f32, f32) -> Hsla) -> Vec<Painted> {
        let mut painted = Vec::new();
        for (index, band) in self.bands(geometry, color) {
            geometry.paint_band(window, band.r0, band.reach, band.start, band.end, band.drawn_color());
            if let Some(index) = index {
                painted.push(Painted { index, band });
            }
        }
        painted
    }

    /// Everything to draw this frame, bottom first, each with its index in the latest
    /// layout if it's part of it: the scan's chart folding away, then the segments, outer
    /// rings first.
    fn bands(&mut self, geometry: &Geometry, mut color: impl FnMut(usize, f32, f32) -> Hsla) -> Vec<(Option<usize>, Band)> {
        let fraction = self.fraction.x;
        // The first chart is uncovered from the centre outward.
        let reach = match self.reveal {
            Some(t) => {
                let t = (t / REVEAL_SECONDS).min(1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                geometry.inner_radius + (geometry.outer_radius() + REVEAL_EDGE - geometry.inner_radius) * eased
            }
            None => f32::INFINITY,
        };
        let mut bands = Vec::new();
        for piece in self.folding_away().unwrap_or_default() {
            let (r0, r1, alpha) = geometry.band_at(piece.depth);
            bands.push((None, Band { r0, r1, reach: r1, start: piece.start, end: piece.end, color: piece.color, alpha }));
        }
        let opening = Self::opening(self.fold.as_ref());
        // Outer rings first, so a ring sliding out from under its parent stays beneath it.
        let mut order: Vec<(&Key, &mut Motion)> = self.segments.iter_mut().collect();
        order.sort_by(|a, b| b.1.depth.x.total_cmp(&a.1.depth.x));
        for (key, motion) in order {
            let (start, end) = (motion.start.x, motion.end.x);
            if let Some(i) = motion.current {
                motion.color = color(i, start, end);
            }
            let (start, end) = opened(&opening, key, start, end);
            let (r0, r1, mut alpha) = geometry.band_at(motion.depth.x);
            if r0 >= reach {
                continue;
            }
            alpha *= ((reach - r0) / REVEAL_EDGE).min(1.0);
            if alpha < 0.01 {
                continue;
            }
            let band = Band { r0, r1, reach: r1.min(reach), start: start * fraction, end: end * fraction, color: motion.color, alpha };
            bands.push((motion.current, band));
        }
        bands
    }

    /// While the scan's chart is folding away: its pieces, where they are now.
    fn folding_away(&self) -> Option<Vec<Piece>> {
        let fold = self.fold.as_ref()?;
        let (placed, _) = fold.closing();
        let pieces = fold
            .old
            .iter()
            .filter_map(|piece| {
                let group = &fold.old_groups[piece.group];
                let ((at, k), base) = (placed[piece.group], group.start + group.width);
                (k > 0.0).then_some(Piece { start: at - (base - piece.start) * k, end: at - (base - piece.end) * k, ..*piece })
            })
            .collect();
        Some(pieces)
    }

    /// While the results are opening: each top-level segment's placement (see `placement`),
    /// which group each segment is in, and the groups.
    fn opening(fold: Option<&Fold>) -> Opening<'_> {
        let fold = fold?;
        let (_, front) = fold.closing();
        Some((filling(&fold.new_groups, front), &fold.new_group_of, &fold.new_groups))
    }
}

type Opening<'a> = Option<(Vec<(f32, f32)>, &'a HashMap<Key, usize>, &'a [Group])>;

/// Where a segment of the opening results is drawn.
fn opened(opening: &Opening, key: &Key, start: f32, end: f32) -> (f32, f32) {
    let Some((placed, group_of, groups)) = opening else { return (start, end) };
    match group_of.get(key) {
        Some(&g) => {
            let ((at, k), base) = (placed[g], groups[g].start);
            (at + (start - base) * k, at + (end - base) * k)
        }
        None => (start, end),
    }
}

/// Eases in and out with no jolt at either end (zero speed and acceleration).
pub(crate) fn smootherstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Which values belong to a longest run (not necessarily contiguous) that never decreases.
fn longest_in_order(values: &[f32]) -> Vec<bool> {
    // Patience sorting: `tails[k]` ends the best run of length k + 1 found so far.
    let mut tails: Vec<usize> = Vec::new();
    let mut before = vec![None; values.len()];
    for (i, &value) in values.iter().enumerate() {
        let at = tails.partition_point(|&t| values[t] <= value);
        before[i] = at.checked_sub(1).map(|k| tails[k]);
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut kept = vec![false; values.len()];
    let mut at = tails.last().copied();
    while let Some(i) = at {
        kept[i] = true;
        at = before[i];
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Kind, Node};
    use crate::sunburst::Target;
    use std::path::PathBuf;
    use std::time::Duration;

    fn seg(start: f32, end: f32, depth: usize) -> Segment {
        Segment { target: Target::Node(0), depth, start, end, kind: Kind::Dir }
    }

    fn key(path: &str) -> Key {
        Key::Node(path.split('/').map(SharedString::from).collect())
    }

    fn at(motion: &ChartMotion, path: &str) -> (f32, f32, f32) {
        let m = &motion.segments[&key(path)];
        (m.start.x, m.end.x, m.depth.x)
    }

    /// Step until settled, checking `each` on every frame. Returns the frame count.
    fn run(motion: &mut ChartMotion, t: &mut Instant, mut each: impl FnMut(&ChartMotion)) -> usize {
        let mut frames = 0;
        while motion.step(*t) && frames < 600 {
            each(motion);
            *t += Duration::from_millis(16);
            frames += 1;
        }
        frames
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn new_and_departed_segments_never_open_gaps() {
        let mut motion = ChartMotion::default();
        let mut t = Instant::now();
        motion.retarget(1, &[key("a"), key("b"), key("c")], &[seg(0.0, 0.4, 1), seg(0.4, 0.7, 1), seg(0.7, 1.0, 1)], 1.0);
        run(&mut motion, &mut t, |_| {});
        // "b" leaves, "n" arrives between "a" and "c".
        motion.retarget(2, &[key("a"), key("n"), key("c")], &[seg(0.0, 0.2, 1), seg(0.2, 0.5, 1), seg(0.5, 1.0, 1)], 1.0);
        let frames = run(&mut motion, &mut t, |m| {
            let (a, n, c) = (at(m, "a"), at(m, "n"), at(m, "c"));
            assert!(close(a.1, n.0), "a and n stay joined: {a:?} {n:?}");
            if let Some(b) = m.segments.get(&key("b")) {
                // The departed segment sits between the new one and "c" as it closes.
                assert!(close(n.1, b.start.x) && close(b.end.x, c.0), "no gap around b: {n:?} {:?} {c:?}", (b.start.x, b.end.x));
            } else {
                assert!(close(n.1, c.0), "n and c stay joined: {n:?} {c:?}");
            }
        });
        assert!(frames < 75, "settles within ~1 s, took {frames} frames");
        assert!(!motion.segments.contains_key(&key("b")), "the departed segment is removed once closed");
        let (n, c) = (at(&motion, "n"), at(&motion, "c"));
        assert!(close(n.0, 0.2) && close(n.1, 0.5) && close(c.0, 0.5));
    }

    #[test]
    fn reordered_segments_never_slide_across_each_other() {
        let mut motion = ChartMotion::default();
        let mut t = Instant::now();
        let keys = [key("a"), key("a/x"), key("b"), key("c")];
        motion.retarget(1, &keys, &[seg(0.0, 0.4, 1), seg(0.0, 0.4, 2), seg(0.4, 0.7, 1), seg(0.7, 1.0, 1)], 1.0);
        run(&mut motion, &mut t, |_| {});
        // "c" overtakes "a" and "b".
        let keys = [key("c"), key("a"), key("a/x"), key("b")];
        motion.retarget(2, &keys, &[seg(0.0, 0.5, 1), seg(0.5, 0.8, 1), seg(0.5, 0.8, 2), seg(0.8, 1.0, 1)], 1.0);
        run(&mut motion, &mut t, |m| {
            let mut ring: Vec<(f32, f32)> =
                m.segments.values().filter(|s| s.depth.x < 1.5 && s.end.x - s.start.x > 1e-4).map(|s| (s.start.x, s.end.x)).collect();
            ring.sort_by(|a, b| a.0.total_cmp(&b.0));
            for pair in ring.windows(2) {
                assert!(close(pair[0].1, pair[1].0), "ring 1 stays one seamless piece: {ring:?}");
            }
            // What's inside "a" goes along with it.
            let (a, x) = (at(m, "a"), at(m, "a/x"));
            assert!(close(a.0, x.0) && close(a.1, x.1));
        });
        assert!(close(at(&motion, "c").0, 0.0) && close(at(&motion, "a").0, 0.5));
        assert!(motion.segments.keys().all(|k| !matches!(k, Key::Retired(_))), "retired copies are gone once closed");
    }

    #[test]
    fn the_scan_folds_away_smallest_first_as_the_results_open_largest_first() {
        let mut motion = ChartMotion::default();
        let mut t = Instant::now();
        let keys = [key("a"), key("b"), key("c"), key("d"), key("d/x")];
        motion.retarget(1, &keys, &[seg(0.0, 0.1, 1), seg(0.1, 0.4, 1), seg(0.4, 0.6, 1), seg(0.6, 1.0, 1), seg(0.6, 0.8, 2)], 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.fold();
        let keys = [key("d"), key("d/x"), key("b"), key("c"), key("a")];
        let layout = [seg(0.0, 0.4, 1), seg(0.0, 0.2, 2), seg(0.4, 0.7, 1), seg(0.7, 0.9, 1), seg(0.9, 1.0, 1)];
        let (mut gone, mut shown): (Vec<f32>, Vec<Key>) = (Vec::new(), Vec::new());
        let (mut old_arc, mut new_arc) = (1.0, 0.0);
        let mut frames = 0;
        loop {
            motion.retarget(2, &keys, &layout, 1.0);
            if !motion.step(t) || frames > 600 {
                break;
            }
            let old: Vec<Piece> = motion.folding_away().unwrap_or_default().into_iter().filter(|p| p.depth < 1.5).collect();
            let opening = ChartMotion::opening(motion.fold.as_ref());
            let new: Vec<(&Key, (f32, f32))> =
                motion.segments.iter().filter(|(_, m)| m.depth.x < 1.5).map(|(k, m)| (k, opened(&opening, k, m.start.x, m.end.x))).collect();
            // Old and new together: the whole circle, in one piece, new from the top.
            let mut ring: Vec<(f32, f32, bool)> = old.iter().map(|p| (p.start, p.end, false)).chain(new.iter().map(|(_, p)| (p.0, p.1, true))).filter(|p| p.1 - p.0 > 1e-5).collect();
            ring.sort_by(|a, b| a.0.total_cmp(&b.0));
            assert!(close(ring[0].0, 0.0) && close(ring.last().unwrap().1, 1.0), "the whole circle: {ring:?}");
            for pair in ring.windows(2) {
                assert!(close(pair[0].1, pair[1].0), "in one piece: {ring:?}");
                assert!(pair[0].2 || !pair[1].2, "the new chart comes first round, the old after it: {ring:?}");
            }
            let (old_now, new_now): (f32, f32) = (old.iter().map(|p| p.end - p.start).sum(), new.iter().map(|(_, p)| p.1 - p.0).sum());
            assert!(old_now <= old_arc + 1e-5 && new_now + 1e-5 >= new_arc, "only ever closing and opening");
            (old_arc, new_arc) = (old_now, new_now);
            for (group, g) in motion.fold.as_ref().map(|f| f.old_groups.clone()).unwrap_or_default().iter().enumerate() {
                if !old.iter().any(|p| p.group == group && p.end - p.start > 1e-3) && !gone.contains(&g.start) {
                    gone.push(g.start);
                }
            }
            let mut showing: Vec<&(&Key, (f32, f32))> = new.iter().filter(|(_, p)| p.1 - p.0 > 1e-5).collect();
            showing.sort_by(|a, b| (b.1.1 - b.1.0).total_cmp(&(a.1.1 - a.1.0)));
            for (k, _) in showing {
                if !shown.contains(k) {
                    shown.push((*k).clone());
                }
            }
            // What's inside a folder opens with it.
            let (d, x) = (opened(&opening, &key("d"), 0.0, 0.4), opened(&opening, &key("d/x"), 0.0, 0.2));
            assert!(close(x.0, d.0) && close((x.1 - x.0) * 2.0, d.1 - d.0));
            t += Duration::from_millis(16);
            frames += 1;
        }
        // Smallest first going (named by where they were), largest first coming.
        assert_eq!(gone, [0.0, 0.4, 0.1, 0.6]);
        assert_eq!(shown, [key("d"), key("b"), key("c"), key("a")]);
        assert!(!motion.folding() && close(new_arc, 1.0));
        assert!(frames < 90, "done within ~1.5 s, took {frames} frames");
    }

    #[test]
    fn folding_into_an_empty_chart_still_ends() {
        let mut motion = ChartMotion::default();
        let mut t = Instant::now();
        motion.retarget(1, &[key("a")], &[seg(0.0, 1.0, 1)], 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.fold();
        let mut frames = 0;
        loop {
            motion.retarget(2, &[], &[], 1.0);
            if !motion.step(t) || frames > 600 {
                break;
            }
            t += Duration::from_millis(16);
            frames += 1;
        }
        assert!(!motion.folding() && frames < 90, "the fold ends, after {frames} frames");
    }

    #[test]
    fn longest_in_order_keeps_the_most() {
        assert_eq!(longest_in_order(&[0.7, 0.0, 0.4]), [false, true, true]);
        assert_eq!(longest_in_order(&[0.0, 0.4, 0.4, 0.2, 0.9]), [true, true, true, false, true]);
    }

    /// What `paint` reports drawing: the latest layout's segments where they are, scaled by
    /// the scan's fraction, in the colour they're painted, and nothing on its way out. While
    /// the reveal uncovers a bar, it's still the whole bar, with how far it's uncovered.
    #[test]
    fn painting_reports_where_the_latest_layout_went() {
        let geometry = Geometry::icicle(gpui::Bounds::new(gpui::point(gpui::px(0.), gpui::px(0.)), gpui::size(gpui::px(800.), gpui::px(600.))));
        let mut motion = ChartMotion::default();
        let mut t = Instant::now();
        motion.retarget(1, &[key("a"), key("a/x"), key("b")], &[seg(0.0, 0.6, 1), seg(0.0, 0.3, 2), seg(0.6, 1.0, 1)], 0.5);
        motion.step(t);
        assert!(motion.bands(&geometry, |_, _, _| hsla(0., 0., 0.5, 1.)).is_empty(), "the reveal starts with nothing showing");
        for _ in 0..2 {
            t += Duration::from_millis(16);
            motion.step(t);
        }
        let bands = motion.bands(&geometry, |i, _, _| hsla(i as f32 / 4.0, 0.5, 0.5, 1.));
        let (_, a) = bands.iter().find(|(index, _)| *index == Some(0)).expect("a is coming into view");
        assert!(a.reach < a.r1 && (a.r1 - geometry.rings[0].1).abs() < 0.1, "the whole bar, partly uncovered: {a:?}");
        assert_eq!(a.color, hsla(0.0, 0.5, 0.5, 1.), "the colour it's painted, before its alpha");
        run(&mut motion, &mut t, |_| {});
        motion.retarget(2, &[key("a"), key("a/x")], &[seg(0.0, 1.0, 1), seg(0.0, 0.5, 2)], 0.5);
        motion.step(t);
        let bands = motion.bands(&geometry, |_, _, _| hsla(0., 0., 0.5, 1.));
        assert_eq!(bands.iter().filter(|(index, _)| index.is_none()).count(), 1, "b, on its way out");
        run(&mut motion, &mut t, |_| {});
        let mut painted: Vec<(usize, f32, f32, f32)> =
            motion.bands(&geometry, |_, _, _| hsla(0., 0., 0.5, 1.)).into_iter().filter_map(|(index, b)| Some((index?, b.start, b.end, b.r0))).collect();
        painted.sort_by_key(|p| p.0);
        assert_eq!(painted.len(), 2);
        assert!(close(painted[0].1, 0.0) && close(painted[0].2, 0.5) && close(painted[1].2, 0.25), "{painted:?}");
        assert!((painted[0].3 - geometry.rings[0].0).abs() < 0.1 && (painted[1].3 - geometry.rings[1].0).abs() < 0.1);
    }

    #[test]
    fn a_folders_contents_slide_out_from_under_it() {
        let mut motion = ChartMotion::default();
        let mut t = Instant::now();
        motion.retarget(1, &[key("a"), key("b")], &[seg(0.0, 0.6, 1), seg(0.6, 1.0, 1)], 1.0);
        run(&mut motion, &mut t, |_| {});
        motion.retarget(2, &[key("a"), key("a/x"), key("a/y"), key("b")], &[seg(0.0, 0.6, 1), seg(0.0, 0.4, 2), seg(0.4, 0.6, 2), seg(0.6, 1.0, 1)], 1.0);
        // Full width straight away, starting in the parent's ring.
        assert_eq!(at(&motion, "a/x"), (0.0, 0.4, 1.0));
        run(&mut motion, &mut t, |m| assert!(close(at(m, "a/x").1, at(m, "a/y").0)));
        assert!(close(at(&motion, "a/y").2, 2.0));
    }

    fn tree() -> Tree {
        // root (100) ── a (60) ── a/x (30), a/y (30)
        //            └─ b (40)
        let node = |name: &str, size, parent, children| Node { name: SharedString::from(name), size, kind: Kind::Dir, parent, children, items: 0 };
        Tree {
            root_path: PathBuf::from("/"),
            nodes: vec![
                node("root", 100, None, vec![1, 2]),
                node("a", 60, Some(0), vec![3, 4]),
                node("b", 40, Some(0), vec![]),
                node("x", 30, Some(1), vec![]),
                node("y", 30, Some(1), vec![]),
            ],
            errors: 0,
            cloud_only: 0,
            ..Default::default()
        }
    }

    fn path(tree: &Tree, mut ix: usize) -> String {
        let mut names = Vec::new();
        while let Some(parent) = tree.nodes[ix].parent {
            names.push(tree.nodes[ix].name.to_string());
            ix = parent;
        }
        names.reverse();
        names.join("/")
    }

    #[test]
    fn zooming_moves_the_whole_chart_as_one() {
        let tree = tree();
        let layout = |focus| {
            let segments = sunburst::layout(&tree, focus);
            let keys: Vec<Key> = segments
                .iter()
                .map(|s| match s.target {
                    Target::Node(ix) => key(&path(&tree, ix)),
                    Target::Small { .. } => unreachable!(),
                })
                .collect();
            (keys, segments)
        };
        let mut motion = ChartMotion::default();
        let mut t = Instant::now();
        let (keys, segments) = layout(0);
        motion.retarget(1, &keys, &segments, 1.0);
        run(&mut motion, &mut t, |_| {});

        // Into "a": it fills the circle, sinking into the centre; "b" is pushed off the edge.
        motion.zoom(Camera::between(&tree, 0, 1));
        let (keys, segments) = layout(1);
        motion.retarget(2, &keys, &segments, 1.0);
        run(&mut motion, &mut t, |m| {
            let (a, x, y) = (at(m, "a"), at(m, "a/x"), at(m, "a/y"));
            // One transform for everything: the children split "a" evenly and sit one ring out.
            assert!(close(x.0, a.0) && close(x.1, y.0) && close(y.1, a.1), "{a:?} {x:?} {y:?}");
            assert!(close((x.1 - x.0) * 2.0, a.1 - a.0) && close(x.2, a.2 + 1.0));
            if let Some(b) = m.segments.get(&key("b")) {
                assert!(close(b.start.x, a.1), "b stays against a's edge");
            }
        });
        let x = at(&motion, "a/x");
        assert!(close(x.0, 0.0) && close(x.1, 0.5) && close(x.2, 1.0), "{x:?}");
        assert!(!motion.segments.contains_key(&key("a")) && !motion.segments.contains_key(&key("b")));

        // And back out: "a" rises from the centre, "b" comes back in from the edge.
        motion.zoom(Camera::between(&tree, 1, 0));
        let (keys, segments) = layout(0);
        motion.retarget(3, &keys, &segments, 1.0);
        assert_eq!(at(&motion, "a"), (0.0, 1.0, 0.0));
        assert_eq!(at(&motion, "b"), (1.0, 1.0, 0.0));
        run(&mut motion, &mut t, |m| assert!(close(at(m, "a").1, at(m, "b").0)));
        let b = at(&motion, "b");
        assert!(close(b.0, 0.6) && close(b.1, 1.0) && close(b.2, 1.0), "{b:?}");
    }
}
