//! The provider boundary the session runtime depends on.

use std::future::Future;

use serde_json::Value;
use tokio::sync::broadcast;

use crate::{
    Result,
    bridge_controller::{ApplyOutcome, BridgeController, BridgeEvent},
    protocol::{QueueEntry, SessionMusicState, SessionOperation, SessionSnapshot},
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

    /// Look `entries` up without touching the queue, alongside other
    /// changes, so loading them afterwards is quick. Reports the entries
    /// the provider can't find. Without it the runtime loads long lists
    /// in one go.
    fn prefetch(
        &self,
        entries: Vec<QueueEntry>,
    ) -> impl Future<Output = Result<ApplyOutcome>> + Send {
        let _ = entries;
        async { Ok(ApplyOutcome::default()) }
    }

    /// Switch off provider features a session can't follow (shuffle, repeat,
    /// autoplay) and return the user's settings, for
    /// [`ProviderBridge::restore_after_session`].
    fn prepare_for_session(&self) -> impl Future<Output = Result<Value>> + Send {
        async { Ok(Value::Null) }
    }

    /// Put back the settings [`ProviderBridge::prepare_for_session`] returned.
    fn restore_after_session(&self, previous: &Value) -> impl Future<Output = Result<()>> + Send {
        let _ = previous;
        async { Ok(()) }
    }
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

    async fn prefetch(&self, entries: Vec<QueueEntry>) -> Result<ApplyOutcome> {
        BridgeController::prefetch(self, &entries).await
    }

    fn prepare_for_session(&self) -> impl Future<Output = Result<Value>> + Send {
        BridgeController::prepare_for_session(self)
    }

    fn restore_after_session(&self, previous: &Value) -> impl Future<Output = Result<()>> + Send {
        BridgeController::restore_after_session(self, previous)
    }
}
