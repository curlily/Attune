use std::sync::Arc;

use jam_core::{
    bridge_controller::BridgeController, network::new_client_id, session::SessionRuntime,
};
use tokio::sync::Mutex;

/// Provider connection and session membership have separate lifecycles:
/// connecting a provider doesn't join a session, and leaving a session keeps
/// the provider connected.
pub struct AppState {
    /// `None` until the user connects a provider. Shared with the session
    /// runtime, hence the `Arc`.
    pub controller: Mutex<Option<Arc<BridgeController>>>,
    /// At most one session at a time.
    pub session: Mutex<Option<SessionRuntime>>,
    /// Identifies this app run to the session server; stable across
    /// reconnects so a dropped connection gets its seat back.
    pub client_id: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            controller: Mutex::new(None),
            session: Mutex::new(None),
            client_id: new_client_id(),
        }
    }
}
