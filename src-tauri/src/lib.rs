mod commands;
mod discord;
mod error;
mod links;
mod settings;
mod state;
mod theme;
mod tray;
mod updates;
mod window;

use std::time::Duration;

use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_deep_link::DeepLinkExt;

use crate::state::AppState;

/// Passed when Windows starts Attune: stay in the tray. Otherwise Attune
/// opens as a normal window.
const HIDDEN_ARG: &str = "--hidden";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // chromiumoxide 0.9 warns on every CDP message it cannot
                // parse from newer Chromium builds; those are harmless.
                "warn,attune_core=debug,chromiumoxide::handler=error".into()
            }),
        )
        .init();

    tauri::Builder::default()
        // First, so a second launch (e.g. from a attune:// link) is handed to
        // the running copy before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            window::show(app);
            links::handle(app, argv.iter().map(String::as_str));
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .on_window_event(window::on_event)
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let state = AppState::new(data_dir);
            let discord_presence = state.settings().discord_presence;
            app.manage(state);
            let handle = app.handle().clone();
            app.manage(discord::DiscordPresence::start(
                discord_presence,
                move |event| match event {
                    discord::DiscordEvent::Join { secret } => links::join_invite(&handle, secret),
                    discord::DiscordEvent::JoinRequest(request) => {
                        window::show(&handle);
                        let _ = handle.emit("discord-join-request", request);
                    }
                },
            ));
            app.manage(window::WindowState::default());
            app.manage(links::PendingJoin::default());
            app.manage(updates::PendingUpdate::default());
            tray::create(app.handle())?;
            theme::follow_taskbar(app.handle());

            // Links that reach the running app, and the one that started it.
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                let urls: Vec<String> = event.urls().iter().map(ToString::to_string).collect();
                links::handle(&handle, urls.iter().map(String::as_str));
            });
            let args: Vec<String> = std::env::args().collect();
            links::handle(app.handle(), args.iter().map(String::as_str));

            if !args.iter().any(|arg| arg == HIDDEN_ARG) {
                window::show(app.handle());
            }

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
            updates::check_for_update,
            updates::install_update,
            updates::skip_update,
            commands::playback::get_providers,
            commands::playback::connect_provider,
            commands::playback::open_provider_sign_in,
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
            commands::session::set_repeat,
            commands::session::queue_play_now,
            commands::session::get_session_status,
            commands::session::get_join_code,
            window::set_window_pinned,
            window::hide_window,
            links::take_join_link,
            commands::session::join_invite,
            commands::session::answer_join_request,
            links::get_autostart,
            links::set_autostart,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                close_session(app);
            }
        });
}

/// On quit, stop the session without leaving (so it can be rejoined) and
/// give the music client its shuffle/repeat/autoplay settings back. Bounded,
/// so a stuck client can't keep Attune from exiting.
fn close_session(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    tauri::async_runtime::block_on(async {
        // The lock is inside the bound too: a create or join stuck on an
        // unresponsive music app holds it, and must not keep Attune open.
        let closed = tokio::time::timeout(Duration::from_secs(3), async {
            if let Some(runtime) = state.session.lock().await.take() {
                runtime.close().await;
            }
        })
        .await;
        if closed.is_err() {
            tracing::warn!("Closing the session on exit timed out");
        }
    });
}
