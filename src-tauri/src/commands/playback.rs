use crate::{error::AppError, state::AppState};
use jam_core::{
    bridge_controller::BridgeController,
    protocol::{SessionOperation, SessionSnapshot},
    providers,
    structs::{PlayState, ProviderConfig},
};
use tauri::State;

/// Connect one provider. Keep the same mutex guard through initialization so
/// two concurrent connect requests cannot create competing controllers.
async fn connect_provider(state: &AppState, config: ProviderConfig) -> Result<(), AppError> {
    let mut controller_slot = state.controller.lock().await;

    if controller_slot.is_some() {
        return Err(jam_core::Error::AlreadyConnected.into());
    }

    let controller = BridgeController::initialise(config).await?;
    *controller_slot = Some(controller);
    Ok(())
}

#[tauri::command]
pub async fn connect_tidal(state: State<'_, AppState>) -> Result<(), AppError> {
    connect_provider(&state, providers::tidal()?).await
}

#[tauri::command]
pub async fn connect_spotify(state: State<'_, AppState>) -> Result<(), AppError> {
    connect_provider(&state, providers::spotify()?).await
}

#[tauri::command]
pub async fn get_playstate(state: State<'_, AppState>) -> Result<PlayState, AppError> {
    let controller_slot = state.controller.lock().await;
    let controller = controller_slot.as_ref().ok_or(jam_core::Error::NotConnected)?;
    Ok(controller.get_playstate().await?)
}

#[tauri::command]
pub async fn get_session_state(
    state: State<'_, AppState>,
) -> Result<jam_core::protocol::SessionMusicState, AppError> {
    let controller_slot = state.controller.lock().await;
    let controller = controller_slot.as_ref().ok_or(jam_core::Error::NotConnected)?;
    Ok(controller.get_session_state().await?)
}

#[tauri::command]
pub async fn apply_session_snapshot(
    state: State<'_, AppState>,
    snapshot: SessionSnapshot,
) -> Result<(), AppError> {
    let controller_slot = state.controller.lock().await;
    let controller = controller_slot.as_ref().ok_or(jam_core::Error::NotConnected)?;
    controller.apply_snapshot(&snapshot).await?;
    Ok(())
}

#[tauri::command]
pub async fn apply_session_operation(
    state: State<'_, AppState>,
    operation: SessionOperation,
) -> Result<(), AppError> {
    let controller_slot = state.controller.lock().await;
    let controller = controller_slot.as_ref().ok_or(jam_core::Error::NotConnected)?;
    controller.apply_operation(&operation).await?;
    Ok(())
}

#[tauri::command]
pub async fn disconnect_provider(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut controller_slot = state.controller.lock().await;
    if let Some(mut controller) = controller_slot.take() {
        controller.dispose().await?;
    }
    Ok(())
}
