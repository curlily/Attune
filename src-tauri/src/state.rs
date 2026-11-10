use jam_core::bridge_controller::BridgeController;
use tokio::sync::Mutex;

#[derive(Default)]
pub struct AppState {
    // None until the user connects to a provider
    pub controller: Mutex<Option<BridgeController>>,
}