use std::sync::Arc;

use crate::{error::AppError, state::AppState};
use jam_core::{
    bridge_controller::BridgeController,
    protocol::SessionMusicState,
    providers,
    structs::{PlayState, ProviderConfig},
};
use tauri::State;
use tracing::warn;

/// The connected provider, without holding the lock across provider calls.
pub(crate) async fn connected_controller(
    state: &AppState,
) -> Result<Arc<BridgeController>, AppError> {
    state
        .controller
        .lock()
        .await
        .clone()
        .ok_or_else(|| jam_core::Error::NotConnected.into())
}

/// Connect one provider. Keep the same mutex guard through initialization so
/// two concurrent connect requests cannot create competing controllers.
async fn connect_provider(state: &AppState, config: ProviderConfig) -> Result<(), AppError> {
    let mut controller_slot = state.controller.lock().await;

    if controller_slot.is_some() {
        return Err(jam_core::Error::AlreadyConnected.into());
    }

    let controller = BridgeController::initialise(config).await?;
    *controller_slot = Some(Arc::new(controller));
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
    Ok(connected_controller(&state).await?.get_playstate().await?)
}

#[tauri::command]
pub async fn get_session_state(state: State<'_, AppState>) -> Result<SessionMusicState, AppError> {
    Ok(connected_controller(&state)
        .await?
        .get_session_state()
        .await?)
}

/// Disconnect the provider. Leaves the session first, since a session
/// without a provider has nothing to follow.
#[tauri::command]
pub async fn disconnect_provider(state: State<'_, AppState>) -> Result<(), AppError> {
    if let Some(runtime) = state.session.lock().await.take() {
        if let Err(error) = runtime.leave().await {
            warn!(%error, "Leaving the session before disconnecting failed");
        }
    }

    let Some(controller) = state.controller.lock().await.take() else {
        return Ok(());
    };
    match Arc::try_unwrap(controller) {
        Ok(mut controller) => controller.dispose().await?,
        // A command still holds a reference; its tasks stop when the last
        // reference is dropped (BridgeController's Drop).
        Err(_) => warn!("Provider still in use; it will be released when the last call finishes"),
    }
    Ok(())
}
