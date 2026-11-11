//! Desktop side of session networking: a provider-independent WebSocket
//! client for the MyJam relay server. It never touches a music provider;
//! applying what it receives is the session runtime's job.

mod client;

pub use client::{
    ClockEstimate, CloseReason, JoinCredentials, ReconnectPolicy, SessionClient,
    SessionClientConfig, SessionEvent, SessionStart, SubmitOutcome, new_client_id,
};
