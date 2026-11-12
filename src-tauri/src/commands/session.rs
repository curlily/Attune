//! Session commands. The runtime does the work; these only check
//! preconditions, keep at most one session, and forward its status and
//! notices to the frontend as events:
//!
//! - `session-status-changed`: [`RuntimeStatus`] whenever it changes
//! - `session-notice`: [`Notice`] one-off messages
//!
//! The server address, display name and client ID come from the settings.
//! After a successful create or join the seat is saved (`rejoin.json`), so
//! a restarted Attune can offer to rejoin; it is forgotten once the session
//! is over for this client.

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::{
    commands::playback::connected_controller,
    error::AppError,
    settings::{RejoinInfo, Store},
    state::AppState,
    tray,
};
use attune_core::{
    network::{JoinCredentials, SessionClientConfig},
    protocol::ErrorCode,
    session::{Notice, QueueEdit, RuntimeStatus, SessionRuntime, SyncState},
};

/// Returned by create/join. `code` is the join code to share.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub code: String,
    pub status: RuntimeStatus,
}

/// How to get in: a new session, a code someone shared, or the saved seat.
enum Entry {
    Create,
    Join(JoinCredentials),
    Rejoin(RejoinInfo),
}

/// Forward runtime status and notices to the frontend until it stops.
fn forward_events(app: AppHandle, store: Store, runtime: &SessionRuntime) {
    let mut status = runtime.watch_status();
    let status_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let initial = status.borrow_and_update().clone();
        tray::set_status(&status_app, Some(initial.sync));
        let _ = status_app.emit("session-status-changed", initial);
        while status.changed().await.is_ok() {
            let current = status.borrow_and_update().clone();
            tray::set_status(&status_app, Some(current.sync));
            let _ = status_app.emit("session-status-changed", current);
        }
        tray::set_status(&status_app, None);
        debug!("Session status forwarding stopped");
    });

    let mut notices = runtime.notices();
    tauri::async_runtime::spawn(async move {
        loop {
            match notices.recv().await {
                Ok(notice) => {
                    // Left, or the server no longer has the session. (Quitting
                    // Attune sends no notice, so the seat stays saved then.)
                    if matches!(notice, Notice::Ended { .. }) {
                        store.clear_rejoin();
                    }
                    let _ = app.emit("session-notice", notice);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

async fn start(app: AppHandle, state: &AppState, entry: Entry) -> Result<SessionInfo, AppError> {
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
    let settings = state.settings().clone();
    let display_name = settings.display_name.ok_or(AppError::NoDisplayName)?;
    let controller = connected_controller(state).await?;
    let server_url = match &entry {
        Entry::Rejoin(saved) => saved.server_url.clone(),
        _ => settings.server_url,
    };
    let mut config = SessionClientConfig::new(server_url.clone(), settings.client_id, display_name);
    config.provider = Some(controller.provider_id().label().to_owned());

    let runtime = match entry {
        Entry::Create => SessionRuntime::create(controller, config).await?,
        Entry::Join(credentials) => SessionRuntime::join(controller, config, credentials).await?,
        Entry::Rejoin(saved) => {
            let credentials = JoinCredentials {
                code: saved.code,
                rejoin_secret: Some(saved.rejoin_secret),
            };
            SessionRuntime::join(controller, config, credentials)
                .await
                .inspect_err(|error| {
                    // The session is gone (or the seat was given up): don't
                    // offer it again.
                    if matches!(
                        error,
                        attune_core::Error::Remote {
                            code: ErrorCode::InvalidCredentials,
                            ..
                        }
                    ) {
                        state.store.clear_rejoin();
                    }
                })?
        }
    };

    let credentials = runtime.credentials();
    if let Some(rejoin_secret) = credentials.rejoin_secret.clone() {
        let saved = RejoinInfo {
            server_url,
            code: credentials.code.clone(),
            rejoin_secret,
        };
        if let Err(error) = state.store.save_rejoin(&saved) {
            warn!(%error, "Could not save the session for rejoining");
        }
    }
    forward_events(app, state.store.clone(), &runtime);
    let info = SessionInfo {
        code: credentials.code.clone(),
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
) -> Result<SessionInfo, AppError> {
    start(app, &state, Entry::Create).await
}

/// Join with a four-letter code; the session's state is applied to the
/// provider before this returns.
#[tauri::command]
pub async fn join_session(
    app: AppHandle,
    state: State<'_, AppState>,
    code: String,
) -> Result<SessionInfo, AppError> {
    let credentials = JoinCredentials::from_code(&code)?;
    start(app, &state, Entry::Join(credentials)).await
}

/// The code of a session Attune was in when it last closed, if any.
#[tauri::command]
pub fn get_rejoin_offer(state: State<'_, AppState>) -> Option<String> {
    state.store.load_rejoin().map(|saved| saved.code)
}

/// Take the saved seat back.
#[tauri::command]
pub async fn rejoin_session(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<SessionInfo, AppError> {
    let saved = state.store.load_rejoin().ok_or(AppError::NoSavedSession)?;
    start(app, &state, Entry::Rejoin(saved)).await
}

/// Forget the saved seat (the user doesn't want to rejoin).
#[tauri::command]
pub fn dismiss_rejoin(state: State<'_, AppState>) {
    state.store.clear_rejoin();
}

/// Leave the session; the provider stays connected.
pub(crate) async fn leave(state: &AppState) -> Result<(), AppError> {
    let runtime = state
        .session
        .lock()
        .await
        .take()
        .ok_or(AppError::NotInSession)?;
    state.store.clear_rejoin();
    runtime.leave().await?;
    Ok(())
}

/// Step out, or resync if stepped out or out of sync (the tray menu's
/// single entry for it).
pub(crate) async fn toggle_following(state: &AppState) -> Result<(), AppError> {
    let session = state.session.lock().await;
    let runtime = session.as_ref().ok_or(AppError::NotInSession)?;
    match runtime.status().sync {
        SyncState::SteppedOut | SyncState::Error => runtime.resync().await?,
        _ => runtime.step_out().await?,
    }
    Ok(())
}

#[tauri::command]
pub async fn leave_session(state: State<'_, AppState>) -> Result<(), AppError> {
    leave(&state).await
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

async fn edit(state: &AppState, edit: QueueEdit) -> Result<(), AppError> {
    let session = state.session.lock().await;
    session
        .as_ref()
        .ok_or(AppError::NotInSession)?
        .edit_queue(edit)
        .await?;
    Ok(())
}

/// Move a song in the session queue (index counts the current song as 0).
#[tauri::command]
pub async fn queue_move(
    state: State<'_, AppState>,
    entry_id: String,
    to_index: usize,
) -> Result<(), AppError> {
    edit(&state, QueueEdit::Move { entry_id, to_index }).await
}

/// Remove a song from the session queue.
#[tauri::command]
pub async fn queue_remove(state: State<'_, AppState>, entry_id: String) -> Result<(), AppError> {
    edit(&state, QueueEdit::Remove { entry_id }).await
}

/// Play a queued song now: it replaces the current song, which leaves the
/// queue. Done as two edits (move it up next, remove the current song), so
/// everyone's music app sees changes it already knows.
#[tauri::command]
pub async fn queue_play_now(state: State<'_, AppState>, entry_id: String) -> Result<(), AppError> {
    let session = state.session.lock().await;
    let runtime = session.as_ref().ok_or(AppError::NotInSession)?;
    let queue = runtime.status().queue;
    let Some(index) = queue.iter().position(|item| item.entry_id == entry_id) else {
        return Ok(()); // gone already
    };
    if index == 0 {
        return Ok(());
    }
    let current = queue[0].entry_id.clone();
    if index > 1 {
        runtime
            .edit_queue(QueueEdit::Move {
                entry_id,
                to_index: 1,
            })
            .await?;
    }
    runtime
        .edit_queue(QueueEdit::Remove { entry_id: current })
        .await?;
    Ok(())
}

/// Current status, or `null` when not in a session.
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

/// The current session's join code, for sharing.
#[tauri::command]
pub async fn get_join_code(state: State<'_, AppState>) -> Result<String, AppError> {
    let session = state.session.lock().await;
    Ok(session
        .as_ref()
        .ok_or(AppError::NotInSession)?
        .credentials()
        .code
        .clone())
}
