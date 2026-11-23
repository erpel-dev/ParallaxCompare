//! ParallaxCompare with a GPUI front end, on the same compare core
//! (`parallax-core`) as the Tauri + Svelte desktop app.
//!
//! `parallax-gpui [LEFT RIGHT]` compares two folders, files, archives or Git
//! revisions, written as for the CLI (`repo@main:src`, `a.zip!/dir` …); without
//! arguments it opens the "New comparison" screen.

mod actions;
mod conflicts_view;
mod folder;
mod input;
mod menu;
mod merge;
mod merge_view;
mod prompt;
mod root;
mod session;
mod style;
mod text_view;
mod workspace;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

use crate::actions::Quit;
use crate::merge::MergeSource;
use crate::root::Root;
use crate::workspace::Start;

const USAGE: &str = "usage: parallax-gpui [LEFT RIGHT]
       parallax-gpui REPOSITORY
       parallax-gpui --merge BASE LOCAL REMOTE [-o OUTPUT]

Compares two folders, files, archives or Git revisions, written as for the
`parallax` command-line tool, e.g. `parallax-gpui old/ new/` or
`parallax-gpui repo@main repo@feature`. Without arguments, it asks for them.
With one folder, it lists the conflicted files of its repository. With --merge,
it merges three files as `git mergetool` passes them and writes OUTPUT (LOCAL
when not given); it exits with 0 once the result was saved without conflicts.

--self-test [REPO_URL] checks that git, gh (and xdg-open) start as the app
starts them and, with a display, that a window opens and draws; the AppImage
build runs it.";

/// What the command line asks for, as the Tauri app's `StartupArgs` reads it.
fn parse(args: Vec<String>) -> Result<Option<Start>, ()> {
    let mut merge = false;
    let mut output = None;
    let mut paths = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--merge" => merge = true,
            "--output" | "-o" => output = args.next(),
            a if a.starts_with('-') && a.len() > 1 => {}
            _ => paths.push(arg),
        }
    }
    if merge {
        // Git passes $BASE empty when there is none (an add/add conflict).
        let [base, local, remote] = <[String; 3]>::try_from(paths).map_err(|_| ())?;
        let output = output.unwrap_or_else(|| local.clone());
        return Ok(Some(Start::Merge { source: MergeSource::Files { base, local, remote, output }, startup: true }));
    }
    match <[String; 2]>::try_from(paths) {
        Ok([left, right]) => Ok(Some(Start::Compare { left, right })),
        Err(paths) => match <[String; 1]>::try_from(paths) {
            Ok([dir]) => Ok(Some(Start::Conflicts { dir: dir.into() })),
            Err(paths) if paths.is_empty() => Ok(None),
            Err(_) => Err(()),
        },
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    if args.first().map(String::as_str) == Some("--self-test") {
        std::process::exit(self_test(args.get(1).map(String::as_str)));
    }
    let Ok(start) = parse(args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    Application::new().run(move |cx: &mut App| {
        style::init(cx);
        cx.bind_keys(actions::bindings());
        cx.bind_keys(input::bindings());
        // The window asks about unsaved edits first (see Root); this is for when
        // no window is left to handle it.
        cx.on_action(|_: &Quit, cx| cx.quit());
        prompt::install(cx);
        // Closing the window ends the app, as in the Tauri app. Not right away:
        // on X11 this runs inside the platform's close handler, which still holds
        // the state that quitting needs.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.spawn(async |cx| cx.update(|cx| cx.quit()).ok()).detach();
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
    std::process::exit(merge_view::EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed));
}

/// The packaged app's self-test: the same checks of git, gh and xdg-open as the
/// Tauri app's, then a window that must draw a frame (which needs Vulkan, the
/// window system's libraries and fonts). Without a display, only the first part.
fn self_test(remote: Option<&str>) -> i32 {
    let mut passed =
        parallax_core::tools::print_self_test(&format!("ParallaxCompare (GPUI) {}", env!("CARGO_PKG_VERSION")), remote);
    let display = ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|v| std::env::var_os(v).is_some_and(|v| !v.is_empty()));
    if !display {
        println!("skip window         no display");
    } else {
        let drawn = window_draws();
        println!("{} window         {}", if drawn { "ok  " } else { "FAIL" }, if drawn { "drawn" } else { "no frame" });
        passed &= drawn;
    }
    println!("{}", if passed { "self-test passed" } else { "self-test FAILED" });
    if passed { 0 } else { 1 }
}

/// Opens a window and waits for its first frame, at most 30 seconds.
fn window_draws() -> bool {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Probe(Arc<AtomicBool>);
    impl Render for Probe {
        fn render(&mut self, window: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            let drawn = self.0.clone();
            window.on_next_frame(move |_, cx| {
                drawn.store(true, Ordering::Relaxed);
                cx.quit();
            });
            window.request_animation_frame();
            gpui::div().size_full().bg(style::bg()).text_color(style::fg()).child("ParallaxCompare self-test")
        }
    }

    let drawn = Arc::new(AtomicBool::new(false));
    // A renderer that hangs instead of failing must not hang the test.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(30));
        println!("FAIL window         no frame within 30 s");
        std::process::exit(1);
    });
    let probe = drawn.clone();
    Application::new().run(move |cx: &mut App| {
        style::init(cx);
        let bounds = Bounds::centered(None, size(px(480.), px(240.)), cx);
        let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
        if let Err(e) = cx.open_window(options, |_, cx| cx.new(|_| Probe(probe))) {
            println!("FAIL window         {e}");
            cx.quit();
        }
    });
    drawn.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Option<Start>, ()> {
        parse(a.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn command_lines() {
        assert!(matches!(args(&[]), Ok(None)));
        assert!(matches!(args(&["a", "b"]), Ok(Some(Start::Compare { .. }))));
        assert!(matches!(args(&["repo"]), Ok(Some(Start::Conflicts { .. }))));
        let Ok(Some(Start::Merge { source: MergeSource::Files { base, output, .. }, startup: true })) =
            args(&["--merge", "", "l", "r"])
        else {
            panic!("a merge")
        };
        assert_eq!((base.as_str(), output.as_str()), ("", "l"));
        assert!(matches!(
            args(&["--merge", "b", "l", "r", "-o", "out"]),
            Ok(Some(Start::Merge { source: MergeSource::Files { output, .. }, .. })) if output == "out"
        ));
        assert!(args(&["a", "b", "c"]).is_err());
        assert!(args(&["--merge", "a"]).is_err());
    }
}
