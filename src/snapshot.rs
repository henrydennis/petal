//! Dev aid (feature `snapshot`): drive the window from a script, and save PNGs of it or
//! record it as a video.
//!
//! Screenshots, with the script running in real time:
//!
//! ```sh
//! PETAL_SNAPSHOT=/tmp/petal PETAL_SCRIPT="wait 3; shot start; move 700 400; shot hover; click 700 400; wait 1; shot zoomed; quit" \
//!   cargo run --features snapshot -- ~/Downloads
//! ```
//!
//! Video (needs `ffmpeg`), with the script running on the recording's own timeline:
//!
//! ```sh
//! PETAL_VIDEO=/tmp/petal.mp4 PETAL_SCRIPT="move 900 300; until-done 20; wait 1; glide 700 420 0.8; wait 0.5; click; wait 2; quit" \
//!   cargo run --release --features snapshot -- ~/Downloads
//! ```
//!
//! While recording, the app's clock (`crate::clock`) and the scan stop during each frame's
//! capture, so the video plays back at the app's real speed and frame rate, however long
//! each capture takes and whether or not the window is visible (`slow-motion 4` plays the
//! steps after it four times slower, still at the full frame rate). The pointer and the window
//! buttons, which macOS draws outside the app, are painted onto the frames.

use std::io::Write as _;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use gpui::{
    AsyncWindowContext, Context, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Point, Window,
    WeakEntity, point, px,
};
use image::{Rgba, RgbaImage};

use crate::app::Petal;
use crate::clock::recording;

pub fn install(window: &mut Window, cx: &mut Context<Petal>) {
    if let Ok(output) = std::env::var("PETAL_VIDEO") {
        let script = std::env::var("PETAL_SCRIPT").unwrap_or_else(|_| "until-done 30; wait 2; quit".into());
        recording::stop();
        cx.spawn_in(window, async move |this, cx| record(this, output, script, cx).await).detach();
        return;
    }
    let Ok(prefix) = std::env::var("PETAL_SNAPSHOT") else { return };
    let script = std::env::var("PETAL_SCRIPT").unwrap_or_else(|_| "wait 3; shot main; quit".into());
    cx.spawn_in(window, async move |_, cx| {
        for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = step.split_whitespace().collect();
            let num = |i: usize| parts.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            // Give the UI a moment to settle after every step.
            let settle = Duration::from_millis(if parts[0] == "wait" { (num(1) * 1000.0) as u64 } else { 150 });
            match parts[0] {
                // `burst name count ms`: capture `count` frames `ms` apart, for animations.
                // Frames are kept in memory and written afterwards, so encoding doesn't stretch
                // the interval; `name-times.txt` records when each frame was taken.
                "burst" => {
                    let count = num(2) as usize;
                    let gap = Duration::from_millis(num(3) as u64);
                    let start = std::time::Instant::now();
                    let mut frames = Vec::new();
                    for _ in 0..count {
                        if let Ok(Ok(image)) = cx.update(|window, _| window.render_to_image()) {
                            frames.push((start.elapsed().as_secs_f64(), image));
                        }
                        cx.background_executor().timer(gap).await;
                    }
                    let name = parts.get(1).unwrap_or(&"burst");
                    let mut times = String::new();
                    for (frame, (t, image)) in frames.iter().enumerate() {
                        image.save(format!("{prefix}-{name}-{frame:03}.png")).ok();
                        times.push_str(&format!("{t:.4}\n"));
                    }
                    std::fs::write(format!("{prefix}-{name}-times.txt"), times).ok();
                }
                "shot" => {
                    let path = format!("{prefix}-{}.png", parts.get(1).unwrap_or(&"shot"));
                    cx.update(|window, _| match window.render_to_image() {
                        Ok(image) => {
                            image.save(&path).ok();
                            eprintln!("snapshot: wrote {path}");
                        }
                        Err(error) => eprintln!("snapshot: {error:#}"),
                    })
                    .ok();
                }
                "move" | "click" | "hover" => {
                    let position = point(px(num(1)), px(num(2)));
                    cx.update(|window, cx| {
                        move_to(window, cx, position);
                        if parts[0] == "hover" {
                            // Re-render right away so a stray real-cursor event can't sneak in first.
                            window.refresh();
                        }
                        if parts[0] == "click" {
                            click(window, cx, position);
                        }
                    })
                    .ok();
                }
                "quit" => {
                    cx.update(|_, cx| cx.quit()).ok();
                    return;
                }
                _ => {}
            }
            cx.background_executor().timer(settle).await;
        }
    })
    .detach();
}

fn move_to(window: &mut Window, cx: &mut gpui::App, position: Point<gpui::Pixels>) {
    window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Default::default() }), cx);
}

fn click(window: &mut Window, cx: &mut gpui::App, position: Point<gpui::Pixels>) {
    let modifiers = Default::default();
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position, modifiers, click_count: 1, first_mouse: false }),
        cx,
    );
    window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers, click_count: 1 }), cx);
}

const FPS: f64 = 30.0;
/// How long the ring drawn around a click takes to fade.
const CLICK_RING: f64 = 0.35;

/// One step of a video script, on the recording's timeline.
enum Step {
    /// Record for this many seconds.
    Wait(f64),
    /// Move the pointer here over this many seconds (eased), recording as it goes.
    Glide(f32, f32, f64),
    Move(f32, f32),
    Click,
    /// Record until the scan finishes, for at most this many seconds.
    UntilDone(f64),
    HidePointer,
    ShowPointer,
    /// Play back this many times slower from here on (1 for real time).
    SlowMotion(f64),
    Quit,
}

fn parse(script: &str) -> Vec<Step> {
    script
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|step| {
            let parts: Vec<&str> = step.split_whitespace().collect();
            let num = |i: usize| parts.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            Some(match parts[0] {
                "wait" => Step::Wait(num(1)),
                "glide" => Step::Glide(num(1) as f32, num(2) as f32, num(3)),
                "move" => Step::Move(num(1) as f32, num(2) as f32),
                "click" => Step::Click,
                "until-done" => Step::UntilDone(num(1)),
                "hide-pointer" => Step::HidePointer,
                "show-pointer" => Step::ShowPointer,
                "slow-motion" => Step::SlowMotion(num(1).max(1.0)),
                "quit" => Step::Quit,
                other => {
                    eprintln!("video: unknown step {other:?}");
                    return None;
                }
            })
        })
        .collect()
}

struct Recorder {
    output: String,
    ffmpeg: Option<(Child, ChildStdin)>,
    written: u64,
    /// App time shown by each output frame: `1 / FPS`, or less in slow motion.
    frame_time: f64,
    /// App time the next output frame shows.
    next_due: f64,
    /// Seconds of app time recorded so far.
    elapsed: f64,
    pointer: Point<f32>,
    shown_pointer: bool,
    last_click: Option<(f64, Point<f32>)>,
    cursor: Sprite,
}

impl Recorder {
    /// Let the app run until the next frame is due, then capture it.
    async fn frame(&mut self, cx: &mut AsyncWindowContext) -> bool {
        // The first frame shows the app at time zero, so the window's first (slow) layout
        // doesn't count as app time.
        if self.written > 0 {
            let started = recording::now();
            recording::start();
            cx.background_executor().timer(Duration::from_secs_f64((self.next_due - self.elapsed).max(0.002))).await;
            recording::stop();
            self.elapsed += recording::now().saturating_duration_since(started).as_secs_f64();
        }

        let pointer = point(px(self.pointer.x), px(self.pointer.y));
        let captured = cx.update(|window, cx| {
            move_to(window, cx, pointer);
            window.refresh();
            window.draw(cx).clear(cx);
            let scale = window.scale_factor();
            window.render_to_image().map(|image| (image, scale))
        });
        let Ok(Ok((mut image, scale))) = captured else {
            eprintln!("video: frame capture failed");
            return false;
        };
        self.decorate(&mut image, scale);
        // Hold the frame for as many output frames as the app time it covers.
        while self.next_due <= self.elapsed {
            if !self.write(&image) {
                return false;
            }
            self.next_due += self.frame_time;
        }
        true
    }

    fn decorate(&self, image: &mut RgbaImage, scale: f32) {
        // The window buttons (macOS draws them outside the app's own rendering).
        for (i, color) in [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]].into_iter().enumerate() {
            let centre = point(16.0 + 6.0 + 20.0 * i as f32, 16.0 + 6.0);
            disc(image, scale, centre, 6.0, Rgba([color[0], color[1], color[2], 255]));
        }
        if let Some((at, position)) = self.last_click {
            let t = (self.elapsed - at) / CLICK_RING;
            if (0.0..1.0).contains(&t) {
                let radius = 10.0 + 16.0 * t as f32;
                ring(image, scale, position, radius, 2.5, Rgba([255, 255, 255, (150.0 * (1.0 - t)) as u8]));
            }
        }
        if self.shown_pointer {
            self.cursor.paint(image, scale, self.pointer);
        }
    }

    fn write(&mut self, image: &RgbaImage) -> bool {
        if self.ffmpeg.is_none() {
            let (width, height) = image.dimensions();
            let child = Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{width}x{height}")])
                .args(["-framerate", &FPS.to_string(), "-i", "-"])
                .args(["-c:v", "libx264", "-preset", "slow", "-crf", "12", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
                .arg(&self.output)
                .stdin(Stdio::piped())
                .spawn();
            match child {
                Ok(mut child) => {
                    let stdin = child.stdin.take().expect("piped stdin");
                    self.ffmpeg = Some((child, stdin));
                }
                Err(error) => {
                    eprintln!("video: couldn't start ffmpeg: {error}");
                    return false;
                }
            }
        }
        let (_, stdin) = self.ffmpeg.as_mut().unwrap();
        self.written += 1;
        stdin.write_all(image.as_raw()).is_ok()
    }

    fn finish(self) {
        if let Some((mut child, stdin)) = self.ffmpeg {
            drop(stdin);
            let _ = child.wait();
            eprintln!("video: wrote {} ({} frames, {:.1} s of video)", self.output, self.written, self.written as f64 / FPS);
        }
    }
}

async fn record(this: WeakEntity<Petal>, output: String, script: String, cx: &mut AsyncWindowContext) {
    let mut recorder = Recorder {
        output,
        ffmpeg: None,
        written: 0,
        frame_time: 1.0 / FPS,
        next_due: 0.0,
        elapsed: 0.0,
        pointer: point(-50.0, -50.0),
        shown_pointer: true,
        last_click: None,
        cursor: Sprite::arrow(),
    };
    'script: for step in parse(&script) {
        match step {
            Step::Wait(seconds) => {
                let until = recorder.elapsed + seconds;
                while recorder.elapsed < until {
                    if !recorder.frame(cx).await {
                        break 'script;
                    }
                }
            }
            Step::Glide(x, y, seconds) => {
                let (from, start) = (recorder.pointer, recorder.elapsed);
                loop {
                    let t = ((recorder.elapsed - start) / seconds.max(0.001)).min(1.0) as f32;
                    // Ease in and out, like a hand moving a mouse.
                    let e = t * t * (3.0 - 2.0 * t);
                    recorder.pointer = point(from.x + (x - from.x) * e, from.y + (y - from.y) * e);
                    if !recorder.frame(cx).await {
                        break 'script;
                    }
                    if t >= 1.0 {
                        break;
                    }
                }
            }
            Step::Move(x, y) => recorder.pointer = point(x, y),
            Step::Click => {
                let position = point(px(recorder.pointer.x), px(recorder.pointer.y));
                cx.update(|window, cx| click(window, cx, position)).ok();
                recorder.last_click = Some((recorder.elapsed, recorder.pointer));
            }
            Step::UntilDone(limit) => {
                let until = recorder.elapsed + limit;
                while recorder.elapsed < until && !this.read_with(cx, |petal, _| petal.showing_results()).unwrap_or(true) {
                    if !recorder.frame(cx).await {
                        break 'script;
                    }
                }
            }
            Step::HidePointer => recorder.shown_pointer = false,
            Step::ShowPointer => recorder.shown_pointer = true,
            Step::SlowMotion(factor) => recorder.frame_time = 1.0 / (FPS * factor),
            Step::Quit => break,
        }
    }
    recorder.finish();
    recording::start();
    cx.update(|_, cx| cx.quit()).ok();
}

/// The pointer: a black arrow with a white border and a soft shadow, worked out per pixel
/// from its outline (in points), so it's sharp at any scale.
struct Sprite {
    outline: Vec<Point<f32>>,
}

impl Sprite {
    /// The standard macOS arrow pointer, tip at (0, 0), in points.
    fn arrow() -> Self {
        let outline = [(0.0, 0.0), (0.0, 16.0), (4.0, 12.3), (6.9, 18.6), (9.6, 17.4), (6.8, 11.4), (11.8, 11.4)]
            .into_iter()
            .map(|(x, y)| point(x, y))
            .collect();
        Self { outline }
    }

    fn paint(&self, image: &mut RgbaImage, scale: f32, at: Point<f32>) {
        const BORDER: f32 = 1.3;
        const SAMPLES: usize = 4;
        let (w, h) = image.dimensions();
        let x0 = ((at.x - 3.0) * scale).floor().max(0.0) as u32;
        let y0 = ((at.y - 3.0) * scale).floor().max(0.0) as u32;
        let x1 = (((at.x + 16.0) * scale).ceil() as u32).min(w);
        let y1 = (((at.y + 23.0) * scale).ceil() as u32).min(h);
        for py in y0..y1 {
            for px_ in x0..x1 {
                let (mut black, mut white, mut shadow) = (0.0f32, 0.0f32, 0.0f32);
                for sy in 0..SAMPLES {
                    for sx in 0..SAMPLES {
                        let p = point(
                            (px_ as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / scale - at.x,
                            (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / scale - at.y,
                        );
                        let inside = contains(&self.outline, p);
                        let d = distance(&self.outline, p);
                        if inside {
                            black += 1.0;
                        } else if d <= BORDER {
                            white += 1.0;
                        }
                        let s = point(p.x, p.y - 1.0);
                        if contains(&self.outline, s) || distance(&self.outline, s) <= BORDER + 1.5 {
                            shadow += 1.0;
                        }
                    }
                }
                let n = (SAMPLES * SAMPLES) as f32;
                blend(image, px_, py, [0, 0, 0], 0.22 * shadow / n);
                blend(image, px_, py, [255, 255, 255], white / n);
                blend(image, px_, py, [0, 0, 0], black / n);
            }
        }
    }
}

fn contains(polygon: &[Point<f32>], p: Point<f32>) -> bool {
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn distance(polygon: &[Point<f32>], p: Point<f32>) -> f32 {
    let mut best = f32::MAX;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (ex, ey) = (a.x + t * dx - p.x, a.y + t * dy - p.y);
        best = best.min((ex * ex + ey * ey).sqrt());
    }
    best
}

fn blend(image: &mut RgbaImage, x: u32, y: u32, color: [u8; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let pixel = image.get_pixel_mut(x, y);
    for c in 0..3 {
        pixel[c] = (pixel[c] as f32 * (1.0 - alpha) + color[c] as f32 * alpha).round() as u8;
    }
}

/// Paints an antialiased shape given its signed coverage function, in points.
fn shade(image: &mut RgbaImage, scale: f32, centre: Point<f32>, reach: f32, color: Rgba<u8>, coverage: impl Fn(f32) -> f32) {
    let (w, h) = image.dimensions();
    let x0 = ((centre.x - reach) * scale).floor().max(0.0) as u32;
    let y0 = ((centre.y - reach) * scale).floor().max(0.0) as u32;
    let x1 = (((centre.x + reach) * scale).ceil().max(0.0) as u32).min(w);
    let y1 = (((centre.y + reach) * scale).ceil().max(0.0) as u32).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = (x as f32 + 0.5) / scale - centre.x;
            let dy = (y as f32 + 0.5) / scale - centre.y;
            let a = coverage((dx * dx + dy * dy).sqrt()) * color[3] as f32 / 255.0;
            blend(image, x, y, [color[0], color[1], color[2]], a);
        }
    }
}

fn disc(image: &mut RgbaImage, scale: f32, centre: Point<f32>, radius: f32, color: Rgba<u8>) {
    shade(image, scale, centre, radius + 1.0, color, |d| ((radius - d) * scale + 0.5).clamp(0.0, 1.0));
}

fn ring(image: &mut RgbaImage, scale: f32, centre: Point<f32>, radius: f32, width: f32, color: Rgba<u8>) {
    shade(image, scale, centre, radius + width, color, |d| ((width / 2.0 - (d - radius).abs()) * scale + 0.5).clamp(0.0, 1.0));
}
