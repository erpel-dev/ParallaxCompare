//! ParallaxCompare with a GPUI front end, on the same compare core
//! (`parallax-core`) as the Tauri + Svelte desktop app.
//!
//! `parallax-gpui [LEFT RIGHT]` compares two folders, files, archives or Git
//! revisions, written as for the CLI (`repo@main:src`, `a.zip!/dir` …); without
//! arguments it opens the "New comparison" screen.

mod actions;
mod folder;
mod input;
mod menu;
mod root;
mod session;
mod style;
mod text_view;
mod workspace;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

use crate::actions::Quit;
use crate::root::Root;

const USAGE: &str = "usage: parallax-gpui [LEFT RIGHT]

Compares two folders, files, archives or Git revisions, written as for the
`parallax` command-line tool, e.g. `parallax-gpui old/ new/` or
`parallax-gpui repo@main repo@feature`. Without arguments, it asks for them.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let start = match <[String; 2]>::try_from(args) {
        Ok([left, right]) => Some((left, right)),
        Err(args) if args.is_empty() => None,
        Err(_) => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    Application::new().run(move |cx: &mut App| {
        style::init(cx);
        cx.bind_keys(actions::bindings());
        cx.bind_keys(input::bindings());
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
            titlebar: Some(TitlebarOptions { title: Some("ParallaxCompare (GPUI)".into()), ..Default::default() }),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| cx.new(|cx| Root::new(start, window, cx))).expect("open the window");
        cx.activate(true);
    });
}
