//! Smooth motion for the live chart. Snapshots arrive ten times a second; instead of
//! jumping to each one, every segment eases from where it is towards where the latest
//! snapshot puts it. Segments are matched across snapshots by folder path, so a folder
//! keeps its identity as it grows, moves, or slides between rings.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{Hsla, SharedString, hsla};

use crate::sunburst::Segment;

/// A segment's identity across snapshots.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Node(Vec<SharedString>),
    /// The "smaller objects" sliver inside the folder at this path.
    Small(Vec<SharedString>),
}

pub struct Motion {
    pub start: f32,
    pub end: f32,
    pub depth: f32,
    target: (f32, f32, f32),
    /// Index into the latest layout's segments; `None` once the segment has left it.
    pub current: Option<usize>,
    /// Colour last drawn with, for segments that are on their way out.
    pub color: Hsla,
}

/// Seconds for a segment to cover ~63% of the way to its target (settles in ~0.4 s).
const TIME_CONSTANT: f32 = 0.12;
const SETTLED: f32 = 1e-4;

#[derive(Default)]
pub struct ChartMotion {
    segments: HashMap<Key, Motion>,
    last_frame: Option<Instant>,
    /// Which layout the targets come from (its address), to retarget only on change.
    layout_id: usize,
    pub fraction: f32,
    fraction_target: f32,
}

impl ChartMotion {
    /// Aim every segment at its place in a new layout. New segments sprout from their
    /// leading edge; segments no longer present shrink away into their middle.
    pub fn retarget(&mut self, layout_id: usize, keys: &[Key], segments: &[Segment], fraction: f32) {
        self.fraction_target = fraction;
        if self.layout_id == layout_id {
            return;
        }
        self.layout_id = layout_id;
        for motion in self.segments.values_mut() {
            motion.current = None;
        }
        for (i, (key, segment)) in keys.iter().zip(segments).enumerate() {
            let target = (segment.start, segment.end, segment.depth as f32);
            match self.segments.get_mut(key) {
                Some(motion) => {
                    motion.target = target;
                    motion.current = Some(i);
                }
                None => {
                    self.segments.insert(
                        key.clone(),
                        Motion {
                            start: segment.start,
                            end: segment.start,
                            depth: target.2,
                            target,
                            current: Some(i),
                            color: hsla(0., 0., 0., 0.),
                        },
                    );
                }
            }
        }
        for motion in self.segments.values_mut().filter(|m| m.current.is_none()) {
            let middle = (motion.start + motion.end) / 2.0;
            motion.target = (middle, middle, motion.depth);
        }
    }

    /// Advance by the time since the last frame. Returns whether anything is still moving.
    pub fn step(&mut self, now: Instant) -> bool {
        let dt = self.last_frame.map(|last| (now - last).as_secs_f32()).unwrap_or(0.0).min(0.1);
        self.last_frame = Some(now);
        let k = 1.0 - (-dt / TIME_CONSTANT).exp();
        let mut moving = false;
        self.fraction += (self.fraction_target - self.fraction) * k;
        if (self.fraction_target - self.fraction).abs() > SETTLED {
            moving = true;
        }
        self.segments.retain(|_, motion| {
            motion.start += (motion.target.0 - motion.start) * k;
            motion.end += (motion.target.1 - motion.end) * k;
            motion.depth += (motion.target.2 - motion.depth) * k;
            let off = (motion.target.0 - motion.start).abs() + (motion.target.1 - motion.end).abs() + (motion.target.2 - motion.depth).abs();
            if off > SETTLED {
                moving = true;
            }
            // Drop segments once they have fully shrunk away.
            motion.current.is_some() || motion.end - motion.start > SETTLED
        });
        moving
    }

    pub fn segments_mut(&mut self) -> impl Iterator<Item = &mut Motion> {
        self.segments.values_mut()
    }
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

    #[test]
    fn eases_towards_targets_and_settles() {
        let key = |n: &str| Key::Node(vec![n.into()]);
        let mut motion = ChartMotion::default();
        let t0 = Instant::now();
        motion.retarget(1, &[key("a"), key("b")], &[seg(0.0, 0.5, 1), seg(0.5, 1.0, 1)], 1.0);
        motion.step(t0);
        // New segments start with zero width at their leading edge.
        let widths: Vec<f32> = motion.segments_mut().map(|m| m.end - m.start).collect();
        assert!(widths.iter().all(|w| *w < 0.01));
        // Next layout: "a" grows, "b" is gone, "c" arrives.
        motion.retarget(2, &[key("a"), key("c")], &[seg(0.0, 0.8, 1), seg(0.8, 1.0, 1)], 1.0);
        let mut t = t0;
        let mut frames = 0;
        while motion.step(t) && frames < 600 {
            t += Duration::from_millis(16);
            frames += 1;
        }
        assert!(frames < 120, "should settle within ~2 s, took {frames} frames");
        let mut settled: Vec<(f32, f32)> = motion.segments_mut().map(|m| (m.start, m.end)).collect();
        settled.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert_eq!(settled.len(), 2, "the departed segment is removed once gone");
        assert!((settled[0].1 - 0.8).abs() < 1e-3 && (settled[1].0 - 0.8).abs() < 1e-3);
    }
}
