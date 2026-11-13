//! Updates from the GitHub releases (see `plugins.updater` in tauri.conf.json).
//!
//! The page checks once on launch and offers what it finds; nothing is
//! downloaded or installed until the user chooses to update. The update
//! that was found is kept here so installing is exactly what was offered.
//! Every release is signed, and the updater refuses one whose signature
//! doesn't match the public key in the config.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{error::AppError, state::AppState};

/// The update found by the last check, until it's installed.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

impl PendingUpdate {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Update>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    /// The release notes, if the release has any.
    pub notes: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    downloaded: u64,
    total: Option<u64>,
}

fn update_error(error: tauri_plugin_updater::Error) -> AppError {
    AppError::Update(error.to_string())
}

/// Look for a newer release. `automatic` is the check on launch: it's left
/// out of development builds and stays quiet about a version the user chose
/// to skip. Checking from Settings always reports what's there.
#[tauri::command]
pub async fn check_for_update(
    app: AppHandle,
    state: State<'_, AppState>,
    pending: State<'_, PendingUpdate>,
    automatic: bool,
) -> Result<Option<UpdateInfo>, AppError> {
    if automatic && cfg!(debug_assertions) {
        return Ok(None);
    }
    let found = app
        .updater()
        .map_err(update_error)?
        .check()
        .await
        .map_err(update_error)?;
    let Some(update) = found else {
        *pending.slot() = None;
        return Ok(None);
    };
    let skipped = state.settings().skipped_update_version.clone();
    if automatic && skipped.as_deref() == Some(update.version.as_str()) {
        return Ok(None);
    }
    let info = UpdateInfo {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone().filter(|notes| !notes.trim().is_empty()),
    };
    *pending.slot() = Some(update);
    Ok(Some(info))
}

/// Download and install the update the last check found, then restart.
/// Reports progress as `update-progress` events. On Windows the installer
/// closes Attune itself and opens the new version.
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<(), AppError> {
    let update = pending.slot().take().ok_or(AppError::NoUpdate)?;
    let progress = app.clone();
    let mut downloaded = 0u64;
    let result = update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.emit("update-progress", DownloadProgress { downloaded, total });
            },
            || {},
        )
        .await;
    if let Err(error) = result {
        // Keep it, so the user can try again without checking again.
        *pending.slot() = Some(update);
        return Err(update_error(error));
    }
    app.restart()
}

/// Don't offer this version on launch again (a newer one is still offered).
#[tauri::command]
pub fn skip_update(state: State<'_, AppState>, version: String) -> Result<(), AppError> {
    state.update_settings(|settings| {
        settings.skipped_update_version = Some(version);
        Ok(())
    })
}
