//! The provider boundary the session runtime depends on.

use std::future::Future;

use tokio::sync::broadcast;

use crate::{
    Result,
    bridge_controller::{ApplyOutcome, BridgeController, BridgeEvent},
    protocol::{SessionMusicState, SessionOperation, SessionSnapshot},
};

/// A connected music provider, seen through the shared session protocol.
///
/// [`BridgeController`] implements it over CDP. Providers that aren't
/// Chromium apps (e.g. a future native Apple Music integration) can
/// implement it differently; tests use a fake. Keep it free of transport
/// details.
pub trait ProviderBridge: Send + Sync + 'static {
    /// Current provider state, current entry first.
    fn get_state(&self) -> impl Future<Output = Result<SessionMusicState>> + Send;

    /// Load a whole session state. Upcoming entries the provider can't find
    /// are skipped and reported; an unfound current entry yields
    /// [`crate::Error::TrackUnavailable`] and changes nothing.
    fn apply_snapshot(
        &self,
        snapshot: &SessionSnapshot,
    ) -> impl Future<Output = Result<ApplyOutcome>> + Send;

    /// Apply one accepted operation, reporting unavailable entries the same way.
    fn apply_operation(
        &self,
        operation: &SessionOperation,
    ) -> impl Future<Output = Result<ApplyOutcome>> + Send;

    /// Local provider events, including `BridgeEvent::Operation` for changes
    /// the user made in the music client.
    fn subscribe(&self) -> broadcast::Receiver<BridgeEvent>;
}

impl ProviderBridge for BridgeController {
    fn get_state(&self) -> impl Future<Output = Result<SessionMusicState>> + Send {
        self.get_session_state()
    }

    fn apply_snapshot(
        &self,
        snapshot: &SessionSnapshot,
    ) -> impl Future<Output = Result<ApplyOutcome>> + Send {
        BridgeController::apply_snapshot(self, snapshot)
    }

    fn apply_operation(
        &self,
        operation: &SessionOperation,
    ) -> impl Future<Output = Result<ApplyOutcome>> + Send {
        BridgeController::apply_operation(self, operation)
    }

    fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        BridgeController::subscribe(self)
    }
}
