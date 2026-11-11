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
mod limiter;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{ConnectInfo, State, WebSocketUpgrade},
    response::Response,
    routing::get,
    serve::ListenerExt,
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
    /// Failed join attempts allowed per IP address per minute (also the
    /// burst size). Join codes are short, so this is what stops guessing.
    pub join_attempts_per_minute: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_sessions: 100,
            max_participants: 16,
            reconnect_grace: Duration::from_secs(60),
            handshake_timeout: Duration::from_secs(10),
            max_message_bytes: 1024 * 1024,
            join_attempts_per_minute: 10,
        }
    }
}

/// The HTTP application. It needs the peer address, so serve it with
/// `into_make_service_with_connect_info::<SocketAddr>()`, as [`serve`] does.
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
    // Session messages are small and latency-sensitive: without TCP_NODELAY,
    // Nagle's algorithm held broadcasts back by 25-55 ms (measured live).
    let listener = listener.tap_io(|stream| {
        if let Err(error) = stream.set_nodelay(true) {
            tracing::warn!(%error, "Could not disable Nagle's algorithm");
        }
    });
    axum::serve(
        listener,
        app(config).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

async fn websocket(
    upgrade: WebSocketUpgrade,
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    let max = hub.config().max_message_bytes;
    upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| connection::serve(socket, hub, peer.ip()))
}
