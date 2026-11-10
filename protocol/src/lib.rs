//! Provider-independent MyJam session protocol.
//!
//! Shared by the desktop client (`jam-core`) and the relay server. Contains no
//! I/O: models, wire messages, pure state transitions and the per-session
//! coordinator that decides which operations are accepted.
//!
//! # Sequencing
//!
//! Every session has one monotonically increasing `u64` sequence. A new
//! session starts at `0`; each accepted operation is assigned the next value.
//! [`SessionSnapshot::revision`] is that sequence, and
//! [`OperationRequest::base_sequence`] is the last sequence the sender knew.

pub mod coordinator;
pub mod messages;
pub mod model;
pub mod state;

pub use messages::*;
pub use model::*;

/// Wire protocol version spoken by this crate.
pub const PROTOCOL_VERSION: u16 = 1;
