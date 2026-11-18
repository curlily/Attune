use std::{
    path::PathBuf,
    sync::{Arc, Mutex as SyncMutex, MutexGuard},
};

use attune_core::{
    bridge_controller::BridgeController, session::SessionRuntime, structs::ProviderId,
};
use tokio::sync::Mutex;

use crate::settings::{Settings, Store};

/// Provider connection and session membership have separate lifecycles:
/// connecting a provider doesn't join a session, and leaving a session keeps
/// the provider connected.
pub struct AppState {
    /// `None` until the user connects a provider. Shared with the session
    /// runtime, hence the `Arc`.
    pub controller: Mutex<Option<Arc<BridgeController>>>,
    /// At most one session at a time.
    pub session: Mutex<Option<SessionRuntime>>,
    pub store: Store,
    /// Never held across an `.await`.
    settings: SyncMutex<Settings>,
    /// The provider being connected right now (the controller's lock is
    /// held meanwhile), so the UI can show it without waiting for it.
    connecting: SyncMutex<Option<ProviderId>>,
}

impl AppState {
    /// Load settings from `data_dir`.
    pub fn new(data_dir: PathBuf) -> Self {
        let store = Store::new(data_dir);
        let settings = store.load_settings();
        Self {
            controller: Mutex::new(None),
            session: Mutex::new(None),
            store,
            settings: SyncMutex::new(settings),
            connecting: SyncMutex::new(None),
        }
    }

    pub fn connecting(&self) -> Option<ProviderId> {
        *self
            .connecting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mark `id` as being connected until the returned guard is dropped.
    pub fn mark_connecting(&self, id: ProviderId) -> ConnectingGuard<'_> {
        *self
            .connecting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(id);
        ConnectingGuard(&self.connecting)
    }

    pub fn settings(&self) -> MutexGuard<'_, Settings> {
        // Settings are plain data; a panic elsewhere doesn't make them invalid.
        self.settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Change the settings and save them; on a failed save nothing changes.
    pub fn update_settings<T>(
        &self,
        change: impl FnOnce(&mut Settings) -> Result<T, crate::error::AppError>,
    ) -> Result<T, crate::error::AppError> {
        let mut settings = self.settings();
        let mut next = settings.clone();
        let result = change(&mut next)?;
        self.store.save_settings(&next)?;
        *settings = next;
        Ok(result)
    }
}

/// Clears [`AppState::connecting`] when connecting ends, however it ends.
pub struct ConnectingGuard<'a>(&'a SyncMutex<Option<ProviderId>>);

impl Drop for ConnectingGuard<'_> {
    fn drop(&mut self) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}
