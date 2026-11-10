mod commands;
mod error;
mod state;

use crate::state::AppState;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,jam_core=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            greet,
            commands::playback::get_playstate,
            commands::playback::connect_tidal,
            commands::playback::connect_spotify,
            commands::playback::get_session_state,
            commands::playback::apply_session_snapshot,
            commands::playback::apply_session_operation,
            commands::playback::disconnect_provider,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
