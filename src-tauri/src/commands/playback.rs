//! Finding and connecting the music app.
//!
//! The frontend hears about connection changes through the
//! `provider-changed` event (the connected [`ProviderId`], or `null`).

use std::sync::Arc;

use crate::{error::AppError, state::AppState};
use jam_core::{
    bridge_controller::BridgeController,
    detect::{detect_providers, provider_config, ProviderAvailability},
    protocol::SessionMusicState,
    structs::{PlayState, ProviderId},
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tracing::{info, warn};

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

/// Connect one provider and remember it for next time. The same mutex guard
/// is held through initialisation, so two concurrent connect requests can't
/// create competing controllers.
pub(crate) async fn connect(
    app: &AppHandle,
    state: &AppState,
    id: ProviderId,
) -> Result<(), AppError> {
    let mut controller_slot = state.controller.lock().await;
    if controller_slot.is_some() {
        return Err(jam_core::Error::AlreadyConnected.into());
    }
    let controller = BridgeController::initialise(provider_config(id)?).await?;
    *controller_slot = Some(Arc::new(controller));
    drop(controller_slot);

    if let Err(error) = state.update_settings(|settings| {
        settings.last_provider = Some(id);
        Ok(())
    }) {
        warn!(%error, "Could not remember the provider");
    }
    let _ = app.emit("provider-changed", Some(id));
    Ok(())
}

/// On start: connect the last used provider, but only if that needs no
/// restart. Anything else waits for the user on the connect screen.
pub(crate) async fn auto_connect(app: &AppHandle, state: &AppState) {
    let Some(id) = state.settings().last_provider else {
        return;
    };
    let ready = match detect_providers().await {
        Ok(found) => found
            .iter()
            .any(|provider| provider.id == id && provider.ready),
        Err(error) => {
            warn!(%error, "Could not check for music apps");
            false
        }
    };
    if !ready {
        return;
    }
    match connect(app, state, id).await {
        Ok(()) => info!(?id, "Reconnected to the last music app"),
        Err(error) => warn!(%error, "Reconnecting to the last music app failed"),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOverview {
    pub providers: Vec<ProviderAvailability>,
    pub connected: Option<ProviderId>,
}

/// Which music apps are installed and running, and which one is connected.
#[tauri::command]
pub async fn get_providers(state: State<'_, AppState>) -> Result<ProviderOverview, AppError> {
    let connected = state
        .controller
        .lock()
        .await
        .as_ref()
        .map(|controller| controller.provider_id());
    Ok(ProviderOverview {
        providers: detect_providers().await?,
        connected,
    })
}

/// Connect a music app. If it runs without MyJam's debug port it is
/// restarted (interrupting playback once); if it isn't running it is
/// started. The UI asks the user before calling this in those cases.
#[tauri::command]
pub async fn connect_provider(
    app: AppHandle,
    state: State<'_, AppState>,
    id: ProviderId,
) -> Result<(), AppError> {
    connect(&app, &state, id).await
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
pub async fn disconnect_provider(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    if let Some(runtime) = state.session.lock().await.take() {
        if let Err(error) = runtime.leave().await {
            warn!(%error, "Leaving the session before disconnecting failed");
        }
        state.store.clear_rejoin();
    }

    let Some(controller) = state.controller.lock().await.take() else {
        return Ok(());
    };
    let _ = app.emit("provider-changed", None::<ProviderId>);
    match Arc::try_unwrap(controller) {
        Ok(mut controller) => controller.dispose().await?,
        // A command still holds a reference; its tasks stop when the last
        // reference is dropped (BridgeController's Drop).
        Err(_) => warn!("Provider still in use; it will be released when the last call finishes"),
    }
    Ok(())
}
