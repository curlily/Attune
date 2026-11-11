mod commands;
mod error;
mod settings;
mod state;

use tauri::Manager;

use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // chromiumoxide 0.9 warns on every CDP message it cannot
                // parse from newer Chromium builds; those are harmless.
                "warn,jam_core=debug,chromiumoxide::handler=error".into()
            }),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            app.manage(AppState::new(data_dir));
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                commands::playback::auto_connect(&handle, &state).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::playback::get_providers,
            commands::playback::connect_provider,
            commands::playback::disconnect_provider,
            commands::playback::get_playstate,
            commands::playback::get_session_state,
            commands::session::create_session,
            commands::session::join_session,
            commands::session::get_rejoin_offer,
            commands::session::rejoin_session,
            commands::session::dismiss_rejoin,
            commands::session::leave_session,
            commands::session::step_out,
            commands::session::resync,
            commands::session::queue_move,
            commands::session::queue_remove,
            commands::session::get_session_status,
            commands::session::get_join_code,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
