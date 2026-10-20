// Hide the console window of release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

fn main() {
    let startup = commands::StartupRequest::from_args(std::env::args().skip(1));
    // Loading the grammars takes a few hundred milliseconds; do it before the first file opens.
    std::thread::spawn(parallax_core::highlight::view_syntax);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::new(startup))
        .invoke_handler(tauri::generate_handler![
            commands::startup_request,
            commands::inspect_path,
            commands::git_refs,
            commands::parse_source,
            commands::resolve_pull_request,
            commands::side_source,
            commands::complete_path,
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
            commands::set_line_endings,
            commands::replace_rows,
            commands::undo,
            commands::redo,
            commands::update_diff,
            commands::save_side,
            commands::syntax_names,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ParallaxCompare");
}
