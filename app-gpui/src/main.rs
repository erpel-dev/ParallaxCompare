//! ParallaxCompare with a GPUI front end: a prototype on the same compare core
//! (`parallax-core`) as the Tauri + Svelte desktop app.
//!
//! `parallax-gpui LEFT RIGHT` compares two folders, files, archives or Git
//! revisions, written as for the CLI (`repo@main:src`, `a.zip!/dir` …).

mod actions;
mod folder;
mod session;
mod style;
mod text_view;
mod workspace;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

use crate::actions::Quit;
use crate::workspace::Workspace;

const USAGE: &str = "usage: parallax-gpui LEFT RIGHT

Compares two folders, files, archives or Git revisions, written as for the
`parallax` command-line tool, e.g. `parallax-gpui old/ new/` or
`parallax-gpui repo@main repo@feature`.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let Ok([left, right]) = <[String; 2]>::try_from(args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    Application::new().run(move |cx: &mut App| {
        style::init(cx);
        cx.bind_keys(actions::bindings());
        cx.on_action(|_: &Quit, cx| cx.quit());
        // Closing the window ends the app, as in the Tauri app.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("ParallaxCompare (GPUI prototype)".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| cx.new(|cx| Workspace::new(left, right, window, cx)))
            .expect("open the window");
        cx.activate(true);
    });
}
