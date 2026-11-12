mod error;

pub mod bridge_controller;
pub mod detect;
pub mod network;
pub mod providers;
pub mod session;
pub mod structs;

/// Shared session protocol (re-exported so dependents need only `attune-core`).
pub use attune_protocol as protocol;

pub use error::{Error, Result};
