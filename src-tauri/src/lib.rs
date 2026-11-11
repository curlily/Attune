mod commands;
mod error;
mod state;

use crate::state::AppState;

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
            commands::playback::get_playstate,
            commands::playback::connect_tidal,
            commands::playback::connect_spotify,
            commands::playback::get_session_state,
            commands::playback::disconnect_provider,
            commands::session::create_session,
            commands::session::join_session,
            commands::session::leave_session,
            commands::session::step_out,
            commands::session::resync,
            commands::session::get_session_status,
            commands::session::get_invite,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
