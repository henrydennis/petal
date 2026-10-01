mod app;
mod clock;
mod dirlist;
mod disk;
mod eta;
mod findings;
mod live;
mod motion;
mod onboarding;
mod scan;
#[cfg(feature = "snapshot")]
mod snapshot;
mod sunburst;

use std::path::PathBuf;

use gpui::{
    App, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowOptions, actions,
    point, prelude::*, px, size,
};

use app::{GoUp, OpenFolder, Petal, Rescan, StartOver};

actions!(petal, [Quit]);

fn main() {
    let _ = app::LAUNCHED.set(std::time::Instant::now());
    let args: Vec<String> = std::env::args().collect();
    // Headless benchmark: `petal --bench-scan <path> [runs]`
    if args.get(1).map(String::as_str) == Some("--bench-scan") {
        let path = PathBuf::from(args.get(2).expect("path required"));
        let runs = args.get(3).and_then(|r| r.parse().ok()).unwrap_or(5);
        scan::bench(&path, runs);
        return;
    }
    // Diagnostic: does this process have Full Disk Access? (Launch through `open` to ask
    // about Petal.app itself rather than the terminal that started it.)
    if args.get(1).map(String::as_str) == Some("--check-access") {
        println!("full disk access: {}", if onboarding::has_full_disk_access() { "yes" } else { "no" });
        return;
    }
    // Headless live-chart benchmark: `petal --bench-live <path> [runs]`
    if args.get(1).map(String::as_str) == Some("--bench-live") {
        let path = PathBuf::from(args.get(2).expect("path required"));
        let runs = args.get(3).and_then(|r| r.parse().ok()).unwrap_or(3);
        live::bench(&path, runs);
        return;
    }
    // On first launch, start scanning the startup disk straight away.
    let initial = args.get(1).map(PathBuf::from).or_else(|| onboarding::is_first_run().then(|| PathBuf::from("/")));

    gpui_platform::application().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-o", OpenFolder, None),
            KeyBinding::new("cmd-r", Rescan, None),
            KeyBinding::new("cmd-up", GoUp, None),
            KeyBinding::new("backspace", GoUp, None),
            KeyBinding::new("escape", GoUp, None),
            KeyBinding::new("cmd-shift-d", StartOver, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "Petal".into(),
                items: vec![MenuItem::action("Quit Petal", Quit)],
                disabled: false,
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Open Folder…", OpenFolder),
                    MenuItem::action("Rescan", Rescan),
                    MenuItem::separator(),
                    MenuItem::action("Show Disks", StartOver),
                ],
                disabled: false,
            },
            Menu {
                name: "Go".into(),
                items: vec![MenuItem::action("Enclosing Folder", GoUp)],
                disabled: false,
            },
        ]);

        let bounds = Bounds::centered(None, size(px(1240.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Petal".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(16.))),
                }),
                window_min_size: Some(size(px(860.), px(560.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Petal::new(initial, window, cx)),
        )
        .expect("failed to open window");

        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
