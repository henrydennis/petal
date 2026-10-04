//! Small pictures of the three charts, for the toolbar's Chart toggle. They're painted, not
//! loaded (the app ships no image assets), from the same shapes the charts are made of: a
//! sunburst's rings, an icicle's falling rows and a treemap's boxes. Each is drawn in the
//! text colour it's placed in, so it dims, lights up on hover and shows selection exactly as
//! the label beside it does.

use std::f32::consts::TAU;

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, canvas, point, prelude::*, px, size};

use crate::app::ChartType;

/// Width and height of an icon.
const SIZE: f32 = 14.0;
/// Space left between the pieces of an icon, as on the charts.
const GAP: f32 = 1.0;

/// An icon of `chart`, `SIZE` points square.
pub fn chart(chart: ChartType) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let color = window.text_style().color;
            match chart {
                ChartType::Sunburst => sunburst(bounds, color, window),
                ChartType::Icicle => icicle(bounds, color, window),
                ChartType::Treemap => treemap(bounds, color, window),
            }
        },
    )
    .flex_none()
    .size(px(SIZE))
}

/// A disc for the folder, a full ring around it and a broken ring outside that.
fn sunburst(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let center = bounds.center();
    disc(window, center, 2.3, color);
    for (start, end) in [(0.0, 0.5), (0.5, 0.82), (0.82, 1.0)] {
        arc(window, center, 3.6, 5.1, start, end, color);
    }
    for (start, end) in [(0.0, 0.34), (0.5, 0.7), (0.82, 0.94)] {
        arc(window, center, 5.8, 7.0, start, end, color);
    }
}

/// The folder as a bar across the top, and its contents falling from it, a row per level.
fn icicle(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let rows: [(f32, f32, &[(f32, f32)]); 3] = [
        (1.0, 4.6, &[(1.0, 13.0)]),
        (4.6, 8.8, &[(1.0, 8.0), (8.0, 13.0)]),
        (8.8, 13.0, &[(1.0, 5.0), (5.0, 8.0), (8.0, 11.0)]),
    ];
    for (top, bottom, bars) in rows {
        for &(left, right) in bars {
            rect(window, bounds, left, top, right, bottom, color);
        }
    }
}

/// A folder's contents as boxes sized by space: one big, the rest sharing the room beside it.
fn treemap(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    for (left, top, right, bottom) in [(1.0, 1.0, 8.0, 13.0), (8.0, 1.0, 13.0, 7.5), (8.0, 7.5, 11.0, 13.0), (11.0, 7.5, 13.0, 13.0)] {
        rect(window, bounds, left, top, right, bottom, color);
    }
}

/// A box from (`left`, `top`) to (`right`, `bottom`) in the icon's own points, inset by half a
/// gap on every side so neighbours that share an edge stand apart.
fn rect(window: &mut Window, bounds: Bounds<Pixels>, left: f32, top: f32, right: f32, bottom: f32, color: Hsla) {
    let inset = GAP / 2.0;
    let origin = point(bounds.origin.x + px(left + inset), bounds.origin.y + px(top + inset));
    let piece = Bounds::new(origin, size(px(right - left - GAP), px(bottom - top - GAP)));
    window.paint_quad(gpui::fill(piece, color).corner_radii(px(1.0)));
}

/// Where a point `radius` from `center` at `turns` (clockwise from 12 o'clock) is.
fn at(center: Point<Pixels>, radius: f32, turns: f32) -> Point<Pixels> {
    let theta = turns * TAU;
    point(center.x + px(radius * theta.sin()), center.y - px(radius * theta.cos()))
}

fn disc(window: &mut Window, center: Point<Pixels>, radius: f32, color: Hsla) {
    let mut builder = PathBuilder::fill();
    builder.move_to(at(center, radius, 0.0));
    for i in 1..48 {
        builder.line_to(at(center, radius, i as f32 / 48.0));
    }
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// A piece of ring between radii `r0` and `r1`, from `start` to `end` in turns, with half a
/// gap trimmed off each end.
fn arc(window: &mut Window, center: Point<Pixels>, r0: f32, r1: f32, start: f32, end: f32, color: Hsla) {
    let trim = |r: f32| (GAP / 2.0) / (r * TAU);
    let (outer, inner) = ((start + trim(r1), end - trim(r1)), (start + trim(r0), end - trim(r0)));
    if inner.1 <= inner.0 {
        return;
    }
    let steps = ((end - start) * 48.0).ceil().max(2.0) as usize;
    let mut builder = PathBuilder::fill();
    builder.move_to(at(center, r1, outer.0));
    for i in 1..=steps {
        builder.line_to(at(center, r1, outer.0 + (outer.1 - outer.0) * i as f32 / steps as f32));
    }
    for i in 0..=steps {
        builder.line_to(at(center, r0, inner.1 - (inner.1 - inner.0) * i as f32 / steps as f32));
    }
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}
