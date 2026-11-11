//! Reading and changing user settings.

use tauri::State;

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
/// create or join on.
#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    update: SettingsUpdate,
) -> Result<Settings, AppError> {
    state.update_settings(|settings| {
        settings.apply(update)?;
        Ok(settings.clone())
    })
}
