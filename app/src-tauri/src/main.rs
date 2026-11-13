// Hide the console window of release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod merge;

fn main() {
    // `--self-test [REPO_URL]`: checks that git, gh (and xdg-open) start as the app
    // starts them, then exits without opening a window. The release build runs it
    // on the packaged AppImage.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--self-test") {
        std::process::exit(self_test(args.get(1).map(String::as_str)));
    }
    let startup = commands::StartupArgs::from_args(args.into_iter());
    // Loading the grammars takes a few hundred milliseconds; do it before the first file opens.
    std::thread::spawn(parallax_core::highlight::view_syntax);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::new(startup))
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::startup_request,
            commands::git_refs,
            commands::parse_source,
            commands::preview_source,
            commands::resolve_pull_request,
            commands::pull_request_tools,
            commands::side_source,
            commands::complete_path,
            commands::side_versions,
            commands::version_spec,
            commands::changed_files,
            commands::git_merge_base,
            commands::git_fetch,
            commands::source_parent,
            commands::open_source,
            commands::file_history,
            commands::open_session,
            commands::close_session,
            commands::run_compare,
            commands::cancel_compare,
            commands::expand,
            commands::copy_entries,
            commands::delete_entries,
            commands::cancel_op,
            commands::refresh_paths,
            commands::open_file_diff,
            commands::file_rows,
            commands::close_file_diff,
            commands::search_diff,
            commands::edit_text,
            commands::insert_line,
            commands::copy_rows,
            commands::compare_pairs,
            commands::copy_file_as,
            commands::rows_text,
            commands::rows_of_lines,
            commands::pull_request_conversation,
            commands::pull_request_checks,
            commands::pull_request_files,
            commands::open_pull_request_link,
            commands::open_pull_request_lines,
            commands::pull_request_write,
            commands::set_line_endings,
            commands::replace_rows,
            commands::undo,
            commands::redo,
            commands::update_diff,
            commands::save_side,
            commands::syntax_names,
            merge::open_merge,
            merge::close_merge,
            merge::merge_info,
            merge::merge_input_rows,
            merge::merge_result_rows,
            merge::merge_resolve,
            merge::merge_edit,
            merge::merge_result_text,
            merge::merge_input_line,
            merge::merge_undo,
            merge::merge_use_git_result,
            merge::syntax_engine,
            merge::merge_structured,
            merge::merge_use_structured_result,
            merge::structured_merge_conflict,
            merge::merge_save,
            merge::repo_conflicts,
            merge::take_conflict_side,
            merge::stage_conflicts,
        ])
        .build(tauri::generate_context!())
        .expect("error while running ParallaxCompare")
        .run(|app, event| {
            // As `git mergetool`: 0 only when the merge was saved without conflicts.
            if let tauri::RunEvent::Exit = event {
                use tauri::Manager;
                std::process::exit(app.state::<commands::AppState>().exit_code());
            }
        });
}

/// Prints one line per check; the exit code is 0 when every required check passed.
fn self_test(remote: Option<&str>) -> i32 {
    println!("ParallaxCompare {} self-test", env!("CARGO_PKG_VERSION"));
    match parallax_core::process::bundle_dir() {
        Some(dir) => println!("AppImage: yes ({})", dir.to_string_lossy()),
        None => println!("AppImage: no"),
    }
    if parallax_core::process::in_flatpak() {
        println!("Flatpak: yes; git and gh run on the host through flatpak-spawn");
    }
    let (checks, passed) = parallax_core::tools::self_test(remote);
    for (check, required) in &checks {
        let mark = match (check.ok(), required) {
            (true, _) => "ok  ",
            (false, true) => "FAIL",
            (false, false) => "skip",
        };
        println!("{mark} {:<14} {}", check.name, check.detail);
    }
    if parallax_core::process::bundle_dir().is_some() {
        // For comparison: git started with the environment the AppImage gives the app.
        // With a URL it talks to the server, which loads libcurl (git-remote-https),
        // where the bundled libraries break a newer system's git.
        let args: Vec<&str> = match remote {
            Some(url) => vec!["ls-remote", "--heads", "--", url],
            None => vec!["--version"],
        };
        #[allow(clippy::disallowed_methods)] // deliberately without process::command
        let plain = std::process::Command::new("git").args(&args).env("GIT_TERMINAL_PROMPT", "0").output();
        let note = match plain {
            Ok(o) if o.status.success() => "starts too".to_string(),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let line = err.lines().find(|l| l.contains("symbol lookup") || l.contains("error while loading"));
                format!("fails: {}", line.or_else(|| err.lines().next()).unwrap_or("").trim())
            }
            Err(e) => format!("could not be run: {e}"),
        };
        println!("note git {} with the AppImage's own environment {note}", args[0]);
    }
    println!("{}", if passed { "self-test passed" } else { "self-test FAILED" });
    if passed { 0 } else { 1 }
}
