//! Finding and connecting the music app.
//!
//! The frontend hears about connection changes through the
//! `provider-changed` event (the connected [`ProviderId`], or `null`), and
//! `provider-lost` (the app's name) when the music app goes away on its own.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use crate::{error::AppError, state::AppState};
use attune_core::{
    bridge_controller::{open_sign_in_window, BridgeController, BridgeEvent},
    detect::{detect_providers, provider_config, ProviderAvailability},
    protocol::SessionMusicState,
    structs::{PlayState, ProviderId},
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// The connected provider, without holding the lock across provider calls.
pub(crate) async fn connected_controller(
    state: &AppState,
) -> Result<Arc<BridgeController>, AppError> {
    let controller = state
        .controller
        .lock()
        .await
        .clone()
        .ok_or(attune_core::Error::NotConnected)?;
    // Gone, and about to be let go of (see `watch_for_loss`).
    if controller.is_closed() {
        return Err(attune_core::Error::NotConnected.into());
    }
    Ok(controller)
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
        return Err(attune_core::Error::AlreadyConnected.into());
    }
    let controller = Arc::new(BridgeController::initialise(provider_config(id)?).await?);
    watch_for_loss(app.clone(), &controller);
    *controller_slot = Some(controller);
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

/// When the music app goes away (closed, or reopened without the debug
/// port), let go of it: close the session, keeping the seat to rejoin after
/// reconnecting, and send the user back to the connect screen. Holds only a
/// weak reference, so a deliberate disconnect isn't kept waiting.
fn watch_for_loss(app: AppHandle, controller: &Arc<BridgeController>) {
    let mut events = controller.subscribe();
    let watched = Arc::downgrade(controller);
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(BridgeEvent::Closed) => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return, // disconnected
            }
        }
        provider_lost(&app, &watched).await;
    });
}

async fn provider_lost(app: &AppHandle, watched: &Weak<BridgeController>) {
    let state = app.state::<AppState>();
    let controller = {
        let mut slot = state.controller.lock().await;
        match (slot.as_ref(), watched.upgrade()) {
            (Some(current), Some(lost)) if Arc::ptr_eq(current, &lost) => slot.take(),
            _ => return, // already replaced or disconnected
        }
    };
    let Some(controller) = controller else {
        return;
    };
    let name = provider_config(controller.provider_id())
        .map(|config| config.display_name)
        .unwrap_or_else(|_| "Your music app".to_owned());
    warn!(provider = %name, "Lost the music app");

    // Bounded: a create or join still talking to the dead app holds the
    // slot until its calls fail.
    let session = tokio::time::timeout(Duration::from_secs(5), state.session.lock()).await;
    if let Ok(mut session) = session {
        if let Some(runtime) = session.take() {
            if tokio::time::timeout(Duration::from_secs(3), runtime.close())
                .await
                .is_err()
            {
                warn!("Closing the session after losing the music app timed out");
            }
        }
    }
    drop(controller);
    let _ = app.emit("provider-changed", None::<ProviderId>);
    let _ = app.emit("provider-lost", name);
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

/// Connect a music app. If it runs without Attune's debug port it is
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

/// Open a browser-based music service in an ordinary window so the user
/// can sign in. Needed once: Google refuses sign-ins in the window Attune
/// connects to. The user closes it again before connecting.
#[tauri::command]
pub async fn open_provider_sign_in(id: ProviderId) -> Result<(), AppError> {
    Ok(open_sign_in_window(&provider_config(id)?)?)
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
