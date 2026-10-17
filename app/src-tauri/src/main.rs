// Hide the console window of release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

fn main() {
    let startup = commands::StartupRequest::from_args(std::env::args().skip(1));
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::new(startup))
        .invoke_handler(tauri::generate_handler![
            commands::startup_request,
            commands::inspect_path,
            commands::git_refs,
            commands::open_session,
            commands::close_session,
            commands::run_compare,
            commands::cancel_compare,
            commands::expand,
            commands::copy_entry,
            commands::delete_entry,
            commands::open_file_diff,
            commands::file_rows,
            commands::close_file_diff,
            commands::syntax_names,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ParallaxCompare");
}
