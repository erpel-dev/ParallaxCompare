// Hide the console window of release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod merge;

fn main() {
    let startup = commands::StartupRequest::from_args(std::env::args().skip(1));
    // Loading the grammars takes a few hundred milliseconds; do it before the first file opens.
    std::thread::spawn(parallax_core::highlight::view_syntax);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::new(startup))
        .invoke_handler(tauri::generate_handler![
            commands::startup_request,
            commands::git_refs,
            commands::parse_source,
            commands::preview_source,
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
