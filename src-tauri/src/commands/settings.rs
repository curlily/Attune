//! Reading and changing user settings.

use tauri::State;

use crate::discord::DiscordPresence;

use crate::{
    error::AppError,
    settings::{Settings, SettingsUpdate},
    state::AppState,
};

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings().clone()
}

/// Change some settings. Invalid values are refused with a message for the
/// user, and nothing is changed. A new display name is used from the next
/// create or join on; who may change what's playing applies at once (if
/// you host the session you're in).
#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    presence: State<'_, DiscordPresence>,
    update: SettingsUpdate,
) -> Result<Settings, AppError> {
    let settings = state.update_settings(|settings| {
        settings.apply(update)?;
        Ok(settings.clone())
    })?;
    presence.set_enabled(settings.discord_presence);
    if let Some(runtime) = state.session.lock().await.as_ref() {
        // Gone already only if the session just ended; nothing to tell.
        let _ = runtime
            .set_others_can_control(settings.others_can_control)
            .await;
    }
    Ok(settings)
}
