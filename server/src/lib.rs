//! MyJam session relay.
//!
//! A small WebSocket server that keeps sessions in memory and delegates every
//! decision (sequencing, acceptance, participants, skipping unplayable songs)
//! to `jam_protocol::coordinator::SessionCoordinator`. This crate only
//! accepts connections, routes messages and enforces resource limits.
//!
//! Endpoints:
//! - `GET /ws`: WebSocket; the first message must be `session.create` or
//!   `session.join`.
//! - `GET /healthz`: returns `ok` (for tunnels and uptime monitors).
//!
//! Sessions are lost on restart by design (v1 has no persistence). A session
//! ends when its last participant leaves or its reconnect grace period
//! expires.

mod connection;
mod hub;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{State, WebSocketUpgrade},
    response::Response,
    routing::get,
};
use tokio::net::TcpListener;

use crate::hub::Hub;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Concurrent sessions; further creates get `serverBusy`.
    pub max_sessions: usize,
    /// Participants per session, including disconnected ones in their grace period.
    pub max_participants: usize,
    /// How long a disconnected participant keeps its seat.
    pub reconnect_grace: Duration,
    /// Time allowed for the first (create/join) message.
    pub handshake_timeout: Duration,
    /// Largest accepted WebSocket message.
    pub max_message_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_sessions: 100,
            max_participants: 16,
            reconnect_grace: Duration::from_secs(60),
            handshake_timeout: Duration::from_secs(10),
            max_message_bytes: 1024 * 1024,
        }
    }
}

/// The HTTP application. Useful for embedding or tests; [`serve`] runs it.
pub fn app(config: ServerConfig) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(websocket))
        .with_state(Arc::new(Hub::new(config)))
}

/// Serve until `shutdown` completes.
pub async fn serve(
    listener: TcpListener,
    config: ServerConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, app(config))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn websocket(upgrade: WebSocketUpgrade, State(hub): State<Arc<Hub>>) -> Response {
    let max = hub.config().max_message_bytes;
    upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| connection::serve(socket, hub))
}
