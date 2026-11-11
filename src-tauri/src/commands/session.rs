//! Session commands. The runtime does the work; these only check
//! preconditions, keep at most one session, and forward its status and
//! notices to the frontend as events:
//!
//! - `session-status-changed`: [`RuntimeStatus`] whenever it changes
//! - `session-notice`: [`jam_core::session::Notice`] one-off messages

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::broadcast;
use tracing::debug;

use crate::{commands::playback::connected_controller, error::AppError, state::AppState};
use jam_core::{
    network::{JoinCredentials, SessionClientConfig},
    session::{RuntimeStatus, SessionRuntime, SyncState},
};

/// Returned by create/join. `invite` is the shareable secret.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub invite: String,
    pub status: RuntimeStatus,
}

fn client_config(
    state: &AppState,
    server_url: String,
    display_name: String,
    provider: &'static str,
) -> SessionClientConfig {
    let mut config = SessionClientConfig::new(server_url, state.client_id.clone(), display_name);
    config.provider = Some(provider.to_owned());
    config
}

/// Forward runtime status and notices to the frontend until it stops.
fn forward_events(app: AppHandle, runtime: &SessionRuntime) {
    let mut status = runtime.watch_status();
    let status_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = status_app.emit("session-status-changed", status.borrow_and_update().clone());
        while status.changed().await.is_ok() {
            let current = status.borrow_and_update().clone();
            let _ = status_app.emit("session-status-changed", current);
        }
        debug!("Session status forwarding stopped");
    });

    let mut notices = runtime.notices();
    tauri::async_runtime::spawn(async move {
        loop {
            match notices.recv().await {
                Ok(notice) => {
                    let _ = app.emit("session-notice", notice);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

async fn start(
    app: AppHandle,
    state: &AppState,
    server_url: String,
    display_name: String,
    invite: Option<String>,
) -> Result<SessionInfo, AppError> {
    // Holding the slot through start-up keeps a second create/join out.
    let mut session = state.session.lock().await;
    if let Some(existing) = session.as_ref() {
        // A session that ended on its own (e.g. the server restarted) just
        // makes room for the next one.
        if existing.status().sync != SyncState::Ended {
            return Err(AppError::SessionActive);
        }
        *session = None;
    }
    let controller = connected_controller(state).await?;
    let config = client_config(
        state,
        server_url,
        display_name,
        controller.provider_id().label(),
    );

    let runtime = match invite {
        None => SessionRuntime::create(controller, config).await?,
        Some(invite) => {
            let credentials = JoinCredentials::parse_invite(&invite)?;
            SessionRuntime::join(controller, config, credentials).await?
        }
    };
    forward_events(app, &runtime);
    let info = SessionInfo {
        invite: runtime.credentials().invite(),
        status: runtime.status(),
    };
    *session = Some(runtime);
    Ok(info)
}

/// Create a session from the connected provider's current queue.
#[tauri::command]
pub async fn create_session(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    display_name: String,
) -> Result<SessionInfo, AppError> {
    start(app, &state, server_url, display_name, None).await
}

/// Join with an invite (`<sessionId>.<joinToken>`); the session's state is
/// applied to the provider before this returns.
#[tauri::command]
pub async fn join_session(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    invite: String,
    display_name: String,
) -> Result<SessionInfo, AppError> {
    start(app, &state, server_url, display_name, Some(invite)).await
}

/// Leave the session; the provider stays connected.
#[tauri::command]
pub async fn leave_session(state: State<'_, AppState>) -> Result<(), AppError> {
    let runtime = state
        .session
        .lock()
        .await
        .take()
        .ok_or(AppError::NotInSession)?;
    runtime.leave().await?;
    Ok(())
}

/// Stop following without leaving.
#[tauri::command]
pub async fn step_out(state: State<'_, AppState>) -> Result<(), AppError> {
    let session = state.session.lock().await;
    session
        .as_ref()
        .ok_or(AppError::NotInSession)?
        .step_out()
        .await?;
    Ok(())
}

/// Apply the session's current state and follow again.
#[tauri::command]
pub async fn resync(state: State<'_, AppState>) -> Result<(), AppError> {
    let session = state.session.lock().await;
    session
        .as_ref()
        .ok_or(AppError::NotInSession)?
        .resync()
        .await?;
    Ok(())
}

/// Current status, or `null` when not in a session. Never includes the invite.
#[tauri::command]
pub async fn get_session_status(
    state: State<'_, AppState>,
) -> Result<Option<RuntimeStatus>, AppError> {
    Ok(state
        .session
        .lock()
        .await
        .as_ref()
        .map(SessionRuntime::status))
}

/// The invite of the current session, for sharing.
#[tauri::command]
pub async fn get_invite(state: State<'_, AppState>) -> Result<String, AppError> {
    let session = state.session.lock().await;
    Ok(session
        .as_ref()
        .ok_or(AppError::NotInSession)?
        .credentials()
        .invite())
}
