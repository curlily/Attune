//! Keeping a music provider in step with a session.

mod bridge;
mod runtime;
mod status;

pub use bridge::ProviderBridge;
pub use runtime::{QueueEdit, SessionRuntime};
pub use status::{Notice, QueueItem, RuntimeStatus, SyncState, UnavailableWarning};
