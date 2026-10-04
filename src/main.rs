mod app;
mod classify;
mod clock;
mod dirlist;
mod disk;
mod eta;
mod findings;
mod icons;
mod live;
mod motion;
mod onboarding;
mod scan;
#[cfg(feature = "snapshot")]
mod snapshot;
mod sunburst;
mod treemap;
mod trashing;
mod watch;

use std::path::PathBuf;

use gpui::{
    App, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowOptions, actions,
    point, prelude::*, px, size,
};

use app::{ChartType, GoUp, OpenFolder, Petal, Rescan, ShowIcicle, ShowSunburst, ShowTreemap, StartOver};

actions!(petal, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom, CloseWindow]);

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
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &Minimize, cx| with_window(cx, |window| window.minimize_window()));
        cx.on_action(|_: &Zoom, cx| with_window(cx, |window| window.zoom_window()));
        cx.on_action(|_: &CloseWindow, cx| with_window(cx, |window| window.remove_window()));
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-o", OpenFolder, None),
            KeyBinding::new("cmd-r", Rescan, None),
            KeyBinding::new("cmd-up", GoUp, None),
            KeyBinding::new("backspace", GoUp, None),
            KeyBinding::new("escape", GoUp, None),
            KeyBinding::new("cmd-shift-d", StartOver, None),
            KeyBinding::new("cmd-1", ShowSunburst, None),
            KeyBinding::new("cmd-2", ShowIcicle, None),
            KeyBinding::new("cmd-3", ShowTreemap, None),
        ]);
        cx.set_menus(menus(ChartType::Sunburst));

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
                // Petal draws its own toolbar in the titlebar and handles dragging and
                // double-clicks there itself; otherwise macOS also acts on double-clicks
                // on the toolbar's buttons (minimizing the window, say).
                app_owns_titlebar_drag: true,
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

/// The menu bar, with the chart type in use ticked in the View menu (the app sets them again
/// when it changes).
pub fn menus(chart: ChartType) -> Vec<Menu> {
    vec![
        Menu {
            name: "Petal".into(),
            items: vec![
                MenuItem::action("Hide Petal", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Petal", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::action("Rescan", Rescan),
                MenuItem::separator(),
                MenuItem::action("Show Disks", StartOver),
                MenuItem::separator(),
                MenuItem::action("Close Window", CloseWindow),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("as Sunburst", ShowSunburst).checked(chart == ChartType::Sunburst),
                MenuItem::action("as Icicle", ShowIcicle).checked(chart == ChartType::Icicle),
                MenuItem::action("as Treemap", ShowTreemap).checked(chart == ChartType::Treemap),
            ],
            disabled: false,
        },
        Menu {
            name: "Go".into(),
            items: vec![MenuItem::action("Enclosing Folder", GoUp)],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)],
            disabled: false,
        },
    ]
}

/// Run `f` on the frontmost window, if Petal has one.
fn with_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| f(window)).ok();
    }
}
