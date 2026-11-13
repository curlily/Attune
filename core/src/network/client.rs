//! WebSocket session client.
//!
//! One background task (the *actor*) owns the socket. [`SessionClient`]
//! methods send it commands over a bounded channel and wait for a `oneshot`
//! reply, so requests are matched to responses by ID, never by timing.
//! Everything the server pushes (remote operations, participant updates,
//! reconnects) is published as [`SessionEvent`]s.
//!
//! # Sequences
//!
//! The actor tracks the last session sequence it has *received* and checks
//! each `operation.accepted` against it: duplicates are dropped, the next
//! sequence is delivered, and anything further ahead is reported as a
//! [`SessionEvent::SequenceGap`] instead of being delivered. The runtime
//! then requests a snapshot. The client never invents sequence numbers.
//!
//! # Reconnecting
//!
//! When the connection drops, pending requests fail with
//! [`Error::TransportClosed`] (their outcome is unknown) and the actor
//! rejoins with the same in-memory credentials, using exponential backoff.
//! Nothing is queued while offline: requests made then fail immediately, and
//! after rejoining the server's snapshot ([`SessionEvent::Rejoined`]) wins.
//!
//! # Clock
//!
//! Server timestamps (`acceptedAtMs`, snapshot capture times) are on the
//! server's clock. The actor sends `time.ping` a few times after connecting
//! and then every ping interval, and publishes a [`ClockEstimate`] from the
//! fastest recent round trip: the slower a reply, the more its timing could
//! be off.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc, oneshot, watch},
    time::{Instant, sleep, sleep_until, timeout},
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    Error, Result,
    protocol::{
        ClientHello, ClientMessage, CreateSession, ErrorCode, JoinSession, OperationRequest,
        PROTOCOL_VERSION, Participant, ParticipantStatus, RejectCode, ServerMessage,
        SessionControl, SessionInvite, SessionMusicState, SessionOperation, SessionSnapshot,
        is_invite_secret, normalize_join_code, state,
    },
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Default cap for one WebSocket message in either direction. A full queue of
/// [`state::MAX_QUEUE_ENTRIES`] entries is roughly 250 KiB of JSON.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 256;
const MAX_PENDING_REQUESTS: usize = 32;
/// Server messages that arrive before the create/join reply and are replayed.
const MAX_EARLY_MESSAGES: usize = 16;
/// Clock probes sent right after connecting, one per `CLOCK_PROBE_SPACING`;
/// after that, one per ping interval.
const CLOCK_PROBES_AT_START: u32 = 5;
const CLOCK_PROBE_SPACING: Duration = Duration::from_secs(1);
/// Round trips kept for the estimate.
const CLOCK_SAMPLES: usize = 8;
/// Replies slower than this say little about the clock and are ignored.
const MAX_CLOCK_ROUND_TRIP_MS: u64 = 5_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// How this computer's clock relates to the server's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockEstimate {
    /// Server clock minus local clock.
    pub offset_ms: i64,
    /// Round trip of the sample the offset came from.
    pub round_trip_ms: u64,
}

impl ClockEstimate {
    /// Estimate from one probe sent at `sent_ms` and answered at
    /// `received_ms` (both local), stamped `server_ms` by the server. The
    /// server is assumed to have answered halfway through the round trip.
    pub fn from_probe(sent_ms: u64, server_ms: u64, received_ms: u64) -> Option<Self> {
        let round_trip_ms = received_ms.checked_sub(sent_ms)?;
        let midpoint = sent_ms + round_trip_ms / 2;
        Some(Self {
            offset_ms: server_ms as i64 - midpoint as i64,
            round_trip_ms,
        })
    }

    pub fn server_time(&self, local_ms: u64) -> u64 {
        local_ms.saturating_add_signed(self.offset_ms)
    }

    pub fn local_time(&self, server_ms: u64) -> u64 {
        server_ms.saturating_add_signed(-self.offset_ms)
    }

    /// Estimated time for a message to reach the server.
    pub fn one_way_ms(&self) -> u64 {
        self.round_trip_ms / 2
    }
}

/// Recent probe results; the estimate is the fastest one.
#[derive(Debug, Default)]
struct ClockSamples {
    samples: VecDeque<ClockEstimate>,
}

impl ClockSamples {
    fn add(&mut self, sample: ClockEstimate) -> Option<ClockEstimate> {
        if sample.round_trip_ms > MAX_CLOCK_ROUND_TRIP_MS {
            return self.best();
        }
        if self.samples.len() == CLOCK_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        self.best()
    }

    fn best(&self) -> Option<ClockEstimate> {
        self.samples
            .iter()
            .min_by_key(|sample| sample.round_trip_ms)
            .copied()
    }
}

/// A fresh random client ID. Generate it once per app run and reuse it for
/// every session, so reconnects are recognised as the same participant.
pub fn new_client_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl ReconnectPolicy {
    /// Delay before reconnect attempt `attempt` (1-based): doubles from
    /// `initial_delay`, capped at `max_delay`, plus up to 20% random jitter
    /// so several clients don't all retry at the same instant.
    pub fn delay(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let base = self
            .initial_delay
            .saturating_mul(1 << exponent)
            .min(self.max_delay);
        let jitter_fraction = (Uuid::new_v4().as_u128() % 1000) as f64 / 5000.0; // 0..0.2
        base + base.mul_f64(jitter_fraction)
    }
}

#[derive(Debug, Clone)]
pub struct SessionClientConfig {
    /// `ws://` (local testing) or `wss://` (hosted) URL of the relay endpoint.
    pub server_url: String,
    pub client_id: String,
    pub display_name: String,
    /// Display-only provider label, e.g. `"tidal"`.
    pub provider: Option<String>,
    /// Limit for connecting, the handshake, and each request's reply.
    pub request_timeout: Duration,
    /// WebSocket ping interval; the connection counts as lost after two
    /// intervals without any traffic from the server.
    pub ping_interval: Duration,
    pub max_message_bytes: usize,
    pub reconnect: ReconnectPolicy,
    /// "Allow others to change what's playing", sent when creating a
    /// session (as its first host).
    pub others_can_control: bool,
}

impl SessionClientConfig {
    pub fn new(
        server_url: impl Into<String>,
        client_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        Self {
            server_url: server_url.into(),
            client_id: client_id.into(),
            display_name: display_name.into(),
            provider: None,
            request_timeout: Duration::from_secs(10),
            ping_interval: Duration::from_secs(30),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            reconnect: ReconnectPolicy::default(),
            others_can_control: true,
        }
    }

    fn hello(&self) -> ClientHello {
        ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_id: self.client_id.clone(),
            display_name: self.display_name.clone(),
            provider: self.provider.clone(),
        }
    }
}

/// How to get into a session: its join code, plus this participant's rejoin
/// secret once it has a seat. The secrets must never be logged; `Debug`
/// redacts them.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCredentials {
    /// The four-letter join code, which is also the session ID. Before a
    /// join by invite it holds the invite secret; the join replaces it with
    /// the code.
    pub code: String,
    /// Proves on reconnect that this client is the participant it was.
    /// Only valid together with the same client ID.
    pub rejoin_secret: Option<String>,
    /// For inviting others (Discord), once in the session and if the server
    /// offers it.
    pub invite: Option<SessionInvite>,
}

impl fmt::Debug for JoinCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinCredentials")
            .field("code", &self.code)
            .field(
                "rejoin_secret",
                &self.rejoin_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("invite", &self.invite)
            .finish()
    }
}

impl JoinCredentials {
    /// A join code as typed by the user, e.g. `"kwpz"`.
    pub fn from_code(code: &str) -> Result<Self> {
        Ok(Self {
            code: normalize_join_code(code).ok_or(Error::InvalidJoinCode)?,
            rejoin_secret: None,
            invite: None,
        })
    }

    /// An invite secret, as Discord hands it over after "Join".
    pub fn from_invite(secret: &str) -> Result<Self> {
        if !is_invite_secret(secret) {
            return Err(Error::InvalidJoinCode);
        }
        Ok(Self {
            code: secret.to_owned(),
            rejoin_secret: None,
            invite: None,
        })
    }
}

/// What happened to a submitted operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Accepted as `sequence`. Mirror it at `accepted_at_ms`.
    Applied { sequence: u64, accepted_at_ms: u64 },
    /// Changed nothing on the server (e.g. someone else already advanced).
    Unchanged { sequence: u64 },
    /// Not accepted; the session continues. On `StaleQueue`, resync.
    Rejected {
        code: RejectCode,
        message: String,
        current_sequence: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// [`SessionClient::leave`] was called.
    Left,
    /// [`SessionClient::close`] was called or the client was dropped.
    ClientClosed,
    /// The server no longer knows the session (e.g. it restarted).
    SessionEnded,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// The next accepted operation from someone else (or the server, when
    /// `origin_client_id` is `None`). Delivered strictly in sequence order.
    RemoteOperation {
        sequence: u64,
        origin_client_id: Option<String>,
        operation: SessionOperation,
        accepted_at_ms: u64,
    },
    /// Operations were missed. Stop applying and request a snapshot.
    SequenceGap {
        expected: u64,
        received: u64,
    },
    Participants(Vec<Participant>),
    /// Who hosts, and whether others may change what's playing. Follows
    /// `Participants` whenever either changes.
    Control(SessionControl),
    /// The connection dropped; attempt `attempt` follows after `retry_in`.
    Reconnecting {
        attempt: u32,
        retry_in: Duration,
    },
    /// Back in the session. Apply this snapshot before following again.
    Rejoined {
        snapshot: SessionSnapshot,
    },
    /// A connection-level error reported by the server.
    ServerError {
        code: ErrorCode,
        message: String,
    },
    /// The client stopped; no further events follow.
    Closed {
        reason: CloseReason,
    },
}

/// Returned by [`SessionClient::create`] and [`SessionClient::join`].
#[derive(Debug)]
pub struct SessionStart {
    pub credentials: JoinCredentials,
    /// Authoritative state to start from (revision = current sequence).
    pub snapshot: SessionSnapshot,
    /// Subscribed before the client started reading, so no event is missed.
    pub events: broadcast::Receiver<SessionEvent>,
}

enum Command {
    Submit {
        request: OperationRequest,
        reply: oneshot::Sender<Result<SubmitOutcome>>,
    },
    Snapshot {
        reply: oneshot::Sender<Result<SessionSnapshot>>,
    },
    /// A message with no reply from the server (status, availability).
    Notify {
        message: ClientMessage,
        reply: oneshot::Sender<Result<()>>,
    },
    Leave {
        reply: oneshot::Sender<Result<()>>,
    },
    Close,
}

impl Command {
    /// Answer a command that cannot be carried out.
    fn fail(self, error: fn() -> Error) {
        match self {
            Self::Submit { reply, .. } => drop(reply.send(Err(error()))),
            Self::Snapshot { reply } => drop(reply.send(Err(error()))),
            Self::Notify { reply, .. } | Self::Leave { reply } => drop(reply.send(Err(error()))),
            Self::Close => {}
        }
    }
}

/// Handle to a joined session. Dropping it closes the connection without
/// leaving (the server keeps the seat for its reconnect grace period).
pub struct SessionClient {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<SessionEvent>,
    credentials: JoinCredentials,
    client_id: String,
    request_timeout: Duration,
    clock: watch::Receiver<Option<ClockEstimate>>,
    task: tokio::task::JoinHandle<()>,
}

impl fmt::Debug for SessionClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionClient")
            .field("code", &self.credentials.code)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl SessionClient {
    /// Create a new session seeded with `initial_state`.
    pub async fn create(
        config: SessionClientConfig,
        initial_state: SessionMusicState,
    ) -> Result<(Self, SessionStart)> {
        let request = ClientMessage::Create(CreateSession {
            hello: config.hello(),
            initial_state,
            others_can_control: Some(config.others_can_control),
        });
        let (socket, reply, early) = handshake(&config, &request).await?;
        let ServerMessage::Created {
            session_id,
            rejoin_secret,
            snapshot,
            invite,
        } = reply
        else {
            return Err(Error::ProtocolViolation {
                reason: "expected session.created",
            });
        };
        if normalize_join_code(&session_id).as_ref() != Some(&session_id)
            || rejoin_secret.is_empty()
        {
            return Err(Error::ProtocolViolation {
                reason: "session.created has invalid credentials",
            });
        }
        if snapshot.revision != 0 {
            return Err(Error::ProtocolViolation {
                reason: "a new session must start at sequence 0",
            });
        }
        validate_snapshot(&snapshot, &session_id)?;
        let credentials = JoinCredentials {
            code: session_id,
            rejoin_secret: Some(rejoin_secret),
            invite,
        };
        info!(client_id = %config.client_id, "Created session");
        Ok(Self::start(config, credentials, socket, snapshot, early))
    }

    /// Join an existing session by its code. With a rejoin secret (from an
    /// earlier run with the same client ID) this takes the old seat back.
    pub async fn join(
        config: SessionClientConfig,
        mut credentials: JoinCredentials,
    ) -> Result<(Self, SessionStart)> {
        let request = join_message(&config, &credentials);
        let (socket, reply, early) = handshake(&config, &request).await?;
        let ServerMessage::Joined {
            snapshot,
            rejoin_secret,
            invite,
        } = reply
        else {
            return Err(Error::ProtocolViolation {
                reason: "expected session.joined",
            });
        };
        // Joined by invite: the session's code comes with the snapshot, and
        // is what reconnects and the saved seat use from now on.
        if is_invite_secret(&credentials.code) {
            if normalize_join_code(&snapshot.session_id).as_ref() != Some(&snapshot.session_id) {
                return Err(Error::ProtocolViolation {
                    reason: "session.joined has an invalid session ID",
                });
            }
            credentials.code = snapshot.session_id.clone();
        }
        validate_snapshot(&snapshot, &credentials.code)?;
        credentials.rejoin_secret = Some(rejoin_secret);
        credentials.invite = invite;
        info!(
            client_id = %config.client_id,
            sequence = snapshot.revision,
            "Joined session"
        );
        Ok(Self::start(config, credentials, socket, snapshot, early))
    }

    fn start(
        config: SessionClientConfig,
        credentials: JoinCredentials,
        socket: Socket,
        snapshot: SessionSnapshot,
        early: Vec<ServerMessage>,
    ) -> (Self, SessionStart) {
        let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (events_tx, events_rx) = broadcast::channel(EVENT_CAPACITY);
        let client_id = config.client_id.clone();
        let request_timeout = config.request_timeout;
        let (clock_tx, clock_rx) = watch::channel(None);

        let actor = Actor {
            config,
            credentials: credentials.clone(),
            commands: commands_rx,
            events: events_tx.clone(),
            last_sequence: snapshot.revision,
            pending_operations: HashMap::new(),
            pending_snapshots: HashMap::new(),
            clock_samples: ClockSamples::default(),
            clock: clock_tx,
        };
        let task = tokio::spawn(actor.run(socket, early));

        let client = Self {
            commands: commands_tx,
            events: events_tx,
            credentials: credentials.clone(),
            client_id,
            request_timeout,
            clock: clock_rx,
            task,
        };
        let start = SessionStart {
            credentials,
            snapshot,
            events: events_rx,
        };
        (client, start)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    pub fn credentials(&self) -> &JoinCredentials {
        &self.credentials
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The current clock estimate; `None` until the first probe returns.
    pub fn clock(&self) -> Option<ClockEstimate> {
        *self.clock.borrow()
    }

    /// Submit a local change. `base_sequence` must be the last sequence the
    /// caller has *applied* (not merely received), so the server can tell
    /// whether a queue edit was based on an outdated queue.
    pub async fn submit_operation(
        &self,
        operation: SessionOperation,
        base_sequence: u64,
    ) -> Result<SubmitOutcome> {
        let request = OperationRequest {
            operation_id: Uuid::new_v4().to_string(),
            base_sequence,
            operation,
        };
        self.request(|reply| Command::Submit { request, reply })
            .await
    }

    /// Fetch the current authoritative snapshot.
    pub async fn request_snapshot(&self) -> Result<SessionSnapshot> {
        self.request(|reply| Command::Snapshot { reply }).await
    }

    /// Report this client's sync status to the other participants.
    pub async fn set_status(&self, status: ParticipantStatus) -> Result<()> {
        let message = ClientMessage::SetStatus { status };
        self.request(|reply| Command::Notify { message, reply })
            .await
    }

    /// As host, allow or stop others changing what's playing. The server
    /// refuses it from anyone else.
    pub async fn set_control(&self, others_can_control: bool) -> Result<()> {
        let message = ClientMessage::SetControl { others_can_control };
        self.request(|reply| Command::Notify { message, reply })
            .await
    }

    /// Report the *complete* set of queue entries this client's provider
    /// could not find. Replaces the previous report; resend after rejoining.
    pub async fn set_unavailable(&self, entry_ids: Vec<String>) -> Result<()> {
        let message = ClientMessage::SetUnavailable { entry_ids };
        self.request(|reply| Command::Notify { message, reply })
            .await
    }

    /// Leave the session and stop the client. Also stops reconnect attempts.
    pub async fn leave(self) -> Result<()> {
        let result = self.request(|reply| Command::Leave { reply }).await;
        // The actor exits right after answering a leave.
        let _ = self.task.await;
        match result {
            // Already stopped (e.g. the session ended): nothing left to leave.
            Err(Error::SessionClosed) => Ok(()),
            other => other,
        }
    }

    /// Close the connection without leaving; the server keeps this seat
    /// for its reconnect grace period.
    pub async fn close(self) {
        let _ = self.commands.send(Command::Close).await;
        let _ = self.task.await;
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
    ) -> Result<T> {
        let (reply, response) = oneshot::channel();
        let exchange = async {
            self.commands
                .send(command(reply))
                .await
                .map_err(|_| Error::SessionClosed)?;
            // A dropped reply means the actor stopped or the connection was
            // lost before the server answered.
            response.await.map_err(|_| Error::TransportClosed)?
        };
        timeout(self.request_timeout, exchange)
            .await
            .map_err(|_| Error::Timeout {
                context: "receive a reply from the session server",
            })?
    }
}

fn join_message(config: &SessionClientConfig, credentials: &JoinCredentials) -> ClientMessage {
    ClientMessage::Join(JoinSession {
        hello: config.hello(),
        session_id: credentials.code.clone(),
        rejoin_secret: credentials.rejoin_secret.clone(),
    })
}

/// A snapshot is only trusted if it is for this session, speaks our
/// protocol version and satisfies the state invariants.
fn validate_snapshot(snapshot: &SessionSnapshot, session_id: &str) -> Result<()> {
    if snapshot.protocol_version != PROTOCOL_VERSION {
        return Err(Error::UnsupportedProtocolVersion {
            found: snapshot.protocol_version,
        });
    }
    if snapshot.session_id != session_id {
        return Err(Error::ProtocolViolation {
            reason: "snapshot belongs to another session",
        });
    }
    state::validate_state(&snapshot.state).map_err(Error::InvalidRemoteState)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

fn tls_connector() -> Result<Connector> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for error in &loaded.errors {
        warn!(%error, "Could not load a system root certificate");
    }
    let (added, _ignored) = roots.add_parsable_certificates(loaded.certs);
    if added == 0 {
        return Err(Error::NoRootCertificates);
    }

    // Pick the provider explicitly: more than one may be compiled in through
    // other dependencies, and rustls refuses to guess.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(Error::Tls)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

async fn connect(config: &SessionClientConfig) -> Result<Socket> {
    let url = config.server_url.trim();
    let connector = if url.starts_with("wss://") {
        Some(tls_connector()?)
    } else if url.starts_with("ws://") {
        None
    } else {
        return Err(Error::InvalidServerUrl {
            reason: "must start with ws:// or wss://",
        });
    };

    let websocket_config = WebSocketConfig::default()
        .max_message_size(Some(config.max_message_bytes))
        .max_frame_size(Some(config.max_message_bytes));
    let (socket, _response) = tokio_tungstenite::connect_async_tls_with_config(
        url,
        Some(websocket_config),
        true,
        connector,
    )
    .await?;
    Ok(socket)
}

async fn send(socket: &mut Socket, message: &ClientMessage, max_bytes: usize) -> Result<()> {
    let text = serde_json::to_string(message)?;
    if text.len() > max_bytes {
        return Err(Error::MessageTooLarge {
            len: text.len(),
            max: max_bytes,
        });
    }
    socket.send(Message::text(text)).await?;
    Ok(())
}

fn parse(text: &str) -> Result<ServerMessage> {
    serde_json::from_str(text).map_err(Error::InvalidServerMessage)
}

async fn next_message(socket: &mut Socket) -> Result<ServerMessage> {
    loop {
        match socket.next().await {
            None | Some(Ok(Message::Close(_))) => return Err(Error::TransportClosed),
            Some(Err(error)) => return Err(error.into()),
            Some(Ok(Message::Text(text))) => return parse(text.as_str()),
            Some(Ok(_)) => {} // ping/pong/binary
        }
    }
}

/// Connect, send create/join, and wait for the matching reply. Other server
/// messages that arrive first are returned for replay.
async fn handshake(
    config: &SessionClientConfig,
    request: &ClientMessage,
) -> Result<(Socket, ServerMessage, Vec<ServerMessage>)> {
    let exchange = async {
        let mut socket = connect(config).await?;
        send(&mut socket, request, config.max_message_bytes).await?;
        let mut early = Vec::new();
        loop {
            match next_message(&mut socket).await? {
                reply @ (ServerMessage::Created { .. } | ServerMessage::Joined { .. }) => {
                    return Ok((socket, reply, early));
                }
                ServerMessage::Error { code, message } => {
                    return Err(Error::Remote { code, message });
                }
                other if early.len() < MAX_EARLY_MESSAGES => early.push(other),
                _ => {
                    return Err(Error::ProtocolViolation {
                        reason: "too many messages before the handshake reply",
                    });
                }
            }
        }
    };
    timeout(config.request_timeout, exchange)
        .await
        .map_err(|_| Error::Timeout {
            context: "complete the session handshake",
        })?
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

enum Exit {
    Stop(CloseReason),
    ConnectionLost,
}

struct Actor {
    config: SessionClientConfig,
    credentials: JoinCredentials,
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<SessionEvent>,
    /// Last session sequence received in order.
    last_sequence: u64,
    pending_operations: HashMap<String, oneshot::Sender<Result<SubmitOutcome>>>,
    pending_snapshots: HashMap<String, oneshot::Sender<Result<SessionSnapshot>>>,
    clock_samples: ClockSamples,
    clock: watch::Sender<Option<ClockEstimate>>,
}

impl Actor {
    fn emit(&self, event: SessionEvent) {
        // No subscribers is fine; the runtime may not be listening yet.
        let _ = self.events.send(event);
    }

    async fn run(mut self, mut socket: Socket, early: Vec<ServerMessage>) {
        for message in early {
            self.on_message(message);
        }
        loop {
            match self.drive(&mut socket).await {
                Exit::Stop(reason) => {
                    let _ = socket.close(None).await;
                    self.fail_pending();
                    info!(?reason, "Session client stopped");
                    self.emit(SessionEvent::Closed { reason });
                    return;
                }
                Exit::ConnectionLost => {
                    self.fail_pending();
                    match self.reconnect().await {
                        Ok(new_socket) => socket = new_socket,
                        Err(reason) => {
                            info!(?reason, "Session client stopped");
                            self.emit(SessionEvent::Closed { reason });
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Serve one connection until it ends.
    async fn drive(&mut self, socket: &mut Socket) -> Exit {
        let mut ping = tokio::time::interval(self.config.ping_interval);
        ping.tick().await; // the first tick completes immediately
        let mut last_heard = Instant::now();
        let mut probes_sent = 0;
        let mut next_probe = Instant::now();

        loop {
            tokio::select! {
                () = sleep_until(next_probe) => {
                    let probe = ClientMessage::TimePing { client_time_ms: now_ms() };
                    if send(socket, &probe, self.config.max_message_bytes).await.is_err() {
                        return Exit::ConnectionLost;
                    }
                    probes_sent += 1;
                    next_probe += if probes_sent < CLOCK_PROBES_AT_START {
                        CLOCK_PROBE_SPACING
                    } else {
                        self.config.ping_interval
                    };
                },

                command = self.commands.recv() => match command {
                    None | Some(Command::Close) => return Exit::Stop(CloseReason::ClientClosed),
                    Some(Command::Leave { reply }) => {
                        let _ = reply.send(self.leave(socket).await);
                        return Exit::Stop(CloseReason::Left);
                    }
                    Some(command) => {
                        if let Err(error) = self.send_command(socket, command).await {
                            warn!(%error, "Sending to the session server failed");
                            return Exit::ConnectionLost;
                        }
                    }
                },

                frame = socket.next() => {
                    last_heard = Instant::now();
                    match frame {
                        None | Some(Ok(Message::Close(_))) => return Exit::ConnectionLost,
                        Some(Err(error)) => {
                            warn!(%error, "Session connection failed");
                            return Exit::ConnectionLost;
                        }
                        Some(Ok(Message::Text(text))) => match parse(text.as_str()) {
                            Ok(message) => {
                                if let Some(exit) = self.on_message(message) {
                                    return exit;
                                }
                            }
                            // Never log the payload: it can contain listening data.
                            Err(error) => warn!(%error, "Ignoring an unparsable server message"),
                        },
                        Some(Ok(_)) => {} // ping/pong/binary
                    }
                }

                _ = ping.tick() => {
                    if last_heard.elapsed() > self.config.ping_interval * 2 {
                        warn!("Session server stopped responding");
                        return Exit::ConnectionLost;
                    }
                    if socket.send(Message::Ping(Default::default())).await.is_err() {
                        return Exit::ConnectionLost;
                    }
                    // Forget requests whose caller already gave up (timeout).
                    self.pending_operations.retain(|_, reply| !reply.is_closed());
                    self.pending_snapshots.retain(|_, reply| !reply.is_closed());
                }
            }
        }
    }

    /// Send a command's message. `Err` means the connection is unusable;
    /// request-level problems are answered on the command's reply instead.
    async fn send_command(&mut self, socket: &mut Socket, command: Command) -> Result<()> {
        if self.pending_operations.len() + self.pending_snapshots.len() >= MAX_PENDING_REQUESTS {
            command.fail(|| Error::TooManyPendingRequests);
            return Ok(());
        }
        let max = self.config.max_message_bytes;

        match command {
            Command::Submit { request, reply } => {
                let operation_id = request.operation_id.clone();
                match send(socket, &ClientMessage::Operation(request), max).await {
                    Ok(()) => {
                        self.pending_operations.insert(operation_id, reply);
                    }
                    Err(error @ Error::MessageTooLarge { .. }) => drop(reply.send(Err(error))),
                    Err(error) => {
                        let _ = reply.send(Err(Error::TransportClosed));
                        return Err(error);
                    }
                }
            }
            Command::Snapshot { reply } => {
                let request_id = Uuid::new_v4().to_string();
                let message = ClientMessage::SnapshotRequest {
                    request_id: request_id.clone(),
                };
                if let Err(error) = send(socket, &message, max).await {
                    let _ = reply.send(Err(Error::TransportClosed));
                    return Err(error);
                }
                self.pending_snapshots.insert(request_id, reply);
            }
            Command::Notify { message, reply } => match send(socket, &message, max).await {
                Ok(()) => drop(reply.send(Ok(()))),
                Err(error @ Error::MessageTooLarge { .. }) => drop(reply.send(Err(error))),
                Err(error) => {
                    let _ = reply.send(Err(Error::TransportClosed));
                    return Err(error);
                }
            },
            Command::Leave { .. } | Command::Close => {} // handled by `drive`
        }
        Ok(())
    }

    /// Send `session.leave` and wait for the confirmation.
    async fn leave(&mut self, socket: &mut Socket) -> Result<()> {
        send(socket, &ClientMessage::Leave, self.config.max_message_bytes).await?;
        let confirmation = async {
            loop {
                if let ServerMessage::Left = next_message(socket).await? {
                    return Ok(());
                }
            }
        };
        timeout(self.config.request_timeout, confirmation)
            .await
            .map_err(|_| Error::Timeout {
                context: "receive the leave confirmation",
            })?
    }

    fn on_message(&mut self, message: ServerMessage) -> Option<Exit> {
        match message {
            ServerMessage::OperationAck {
                operation_id,
                sequence,
                applied,
                accepted_at_ms,
            } => {
                let Some(reply) = self.pending_operations.remove(&operation_id) else {
                    debug!(%operation_id, "Ack for an operation that is no longer pending");
                    return None;
                };
                let outcome = if applied {
                    self.advance_sequence(sequence);
                    SubmitOutcome::Applied {
                        sequence,
                        accepted_at_ms,
                    }
                } else {
                    SubmitOutcome::Unchanged { sequence }
                };
                let _ = reply.send(Ok(outcome));
            }

            ServerMessage::OperationAccepted {
                sequence,
                origin_client_id,
                operation,
                accepted_at_ms,
            } => {
                if origin_client_id.as_deref() == Some(self.config.client_id.as_str()) {
                    // The server must not echo our own operations; we already
                    // have them. Keep the sequence in step, deliver nothing.
                    warn!(sequence, "Server echoed this client's own operation");
                    self.advance_sequence(sequence);
                } else if self.advance_sequence(sequence) {
                    self.emit(SessionEvent::RemoteOperation {
                        sequence,
                        origin_client_id,
                        operation,
                        accepted_at_ms,
                    });
                }
            }

            ServerMessage::OperationRejected {
                operation_id,
                code,
                message,
                current_sequence,
            } => {
                if let Some(reply) = self.pending_operations.remove(&operation_id) {
                    let _ = reply.send(Ok(SubmitOutcome::Rejected {
                        code,
                        message,
                        current_sequence,
                    }));
                }
            }

            ServerMessage::Snapshot {
                request_id,
                snapshot,
            } => {
                let reply = request_id.and_then(|id| self.pending_snapshots.remove(&id));
                let Some(reply) = reply else {
                    debug!("Snapshot for a request that is no longer pending");
                    return None;
                };
                let result = validate_snapshot(&snapshot, &self.credentials.code).map(|()| {
                    // Operations after this snapshot build on its revision.
                    self.last_sequence = snapshot.revision;
                    snapshot
                });
                let _ = reply.send(result);
            }

            ServerMessage::Participants {
                participants,
                control,
            } => {
                self.emit(SessionEvent::Participants(participants));
                if let Some(control) = control {
                    self.emit(SessionEvent::Control(control));
                }
            }

            ServerMessage::Error { code, message } => {
                warn!(?code, "Session server reported an error");
                self.emit(SessionEvent::ServerError { code, message });
            }

            ServerMessage::Left => return Some(Exit::Stop(CloseReason::Left)),

            ServerMessage::TimePong {
                client_time_ms,
                server_time_ms,
            } => {
                if let Some(sample) =
                    ClockEstimate::from_probe(client_time_ms, server_time_ms, now_ms())
                {
                    let best = self.clock_samples.add(sample);
                    self.clock.send_if_modified(|current| {
                        let changed = *current != best;
                        *current = best;
                        changed
                    });
                }
            }

            ServerMessage::Created { .. } | ServerMessage::Joined { .. } => {
                warn!("Ignoring an unexpected handshake reply");
            }
        }
        None
    }

    /// Accept `sequence` if it is the next one. Returns whether it was
    /// delivered; duplicates are dropped and gaps reported.
    fn advance_sequence(&mut self, sequence: u64) -> bool {
        let expected = self.last_sequence + 1;
        if sequence == expected {
            self.last_sequence = sequence;
            true
        } else if sequence < expected {
            debug!(sequence, "Ignoring an already received sequence");
            false
        } else {
            warn!(expected, received = sequence, "Session sequence gap");
            self.emit(SessionEvent::SequenceGap {
                expected,
                received: sequence,
            });
            false
        }
    }

    fn fail_pending(&mut self) {
        for (_, reply) in self.pending_operations.drain() {
            let _ = reply.send(Err(Error::TransportClosed));
        }
        for (_, reply) in self.pending_snapshots.drain() {
            let _ = reply.send(Err(Error::TransportClosed));
        }
    }

    /// Rejoin with backoff until it works, the user leaves or closes, or
    /// the server says the session is gone.
    async fn reconnect(&mut self) -> std::result::Result<Socket, CloseReason> {
        let code = self.credentials.code.clone();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let retry_in = self.config.reconnect.delay(attempt);
            info!(attempt, ?retry_in, "Reconnecting to the session server");
            self.emit(SessionEvent::Reconnecting { attempt, retry_in });

            // Wait, still answering commands: nothing is queued offline.
            let wait = sleep(retry_in);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    () = &mut wait => break,
                    command = self.commands.recv() => match command {
                        None | Some(Command::Close) => return Err(CloseReason::ClientClosed),
                        Some(Command::Leave { reply }) => {
                            // Not connected: the server drops us after its grace period.
                            let _ = reply.send(Ok(()));
                            return Err(CloseReason::Left);
                        }
                        Some(other) => other.fail(|| Error::TransportClosed),
                    },
                }
            }

            let request = join_message(&self.config, &self.credentials);
            let rejoin = match handshake(&self.config, &request).await {
                Ok((
                    socket,
                    ServerMessage::Joined {
                        snapshot,
                        rejoin_secret,
                        invite,
                    },
                    early,
                )) => validate_snapshot(&snapshot, &code).map(|()| {
                    self.credentials.rejoin_secret = Some(rejoin_secret);
                    if invite.is_some() {
                        self.credentials.invite = invite;
                    }
                    (socket, snapshot, early)
                }),
                Ok(_) => Err(Error::ProtocolViolation {
                    reason: "expected session.joined",
                }),
                Err(error) => Err(error),
            };

            match rejoin {
                Ok((socket, snapshot, early)) => {
                    info!(sequence = snapshot.revision, "Rejoined session");
                    self.last_sequence = snapshot.revision;
                    self.emit(SessionEvent::Rejoined { snapshot });
                    for message in early {
                        self.on_message(message);
                    }
                    return Ok(socket);
                }
                Err(Error::Remote {
                    code: ErrorCode::InvalidCredentials,
                    ..
                }) => {
                    warn!("Session no longer exists on the server");
                    return Err(CloseReason::SessionEnded);
                }
                Err(error) => warn!(attempt, %error, "Reconnect attempt failed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SessionPlayback;

    fn snapshot(session_id: &str) -> SessionSnapshot {
        SessionSnapshot {
            protocol_version: PROTOCOL_VERSION,
            session_id: session_id.to_owned(),
            revision: 0,
            updated_at_ms: 0,
            state: SessionMusicState {
                queue: Vec::new(),
                current_entry_id: None,
                playback: SessionPlayback {
                    is_playing: false,
                    position_ms: 0,
                    position_captured_at_ms: 0,
                },
            },
        }
    }

    #[test]
    fn join_codes_are_normalized_and_checked() {
        let credentials = JoinCredentials::from_code(" kwpz ").expect("parse");
        assert_eq!(credentials.code, "KWPZ");
        assert_eq!(credentials.rejoin_secret, None);

        for invalid in ["", "KWP", "KWPZA", "KWP0", "abc.def"] {
            assert!(matches!(
                JoinCredentials::from_code(invalid),
                Err(Error::InvalidJoinCode)
            ));
        }
    }

    #[test]
    fn credentials_debug_redacts_the_secret() {
        let credentials = JoinCredentials {
            code: "KWPZ".to_owned(),
            rejoin_secret: Some("s3cr3t".to_owned()),
            invite: Some(SessionInvite {
                secret: "f".repeat(64),
                party_id: "party".to_owned(),
                max_participants: 16,
            }),
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains(&"f".repeat(64)), "{debug}");
        assert!(debug.contains("KWPZ"));
        assert!(!debug.contains("s3cr3t"));
    }

    #[test]
    fn clock_estimate_assumes_the_reply_left_halfway() {
        // Sent at 1000, answered at 1100: the server stamped 5050 at about 1050.
        let estimate = ClockEstimate::from_probe(1_000, 5_050, 1_100).expect("estimate");
        assert_eq!(estimate.offset_ms, 4_000);
        assert_eq!(estimate.round_trip_ms, 100);
        assert_eq!(estimate.server_time(2_000), 6_000);
        assert_eq!(estimate.local_time(6_000), 2_000);
        assert_eq!(estimate.one_way_ms(), 50);

        // A server clock behind ours.
        let behind = ClockEstimate::from_probe(10_000, 9_020, 10_040).expect("estimate");
        assert_eq!(behind.offset_ms, -1_000);
        // A reply "before" the probe was sent (local clock jumped) is unusable.
        assert_eq!(ClockEstimate::from_probe(1_000, 5_000, 900), None);
    }

    #[test]
    fn clock_uses_the_fastest_recent_round_trip() {
        let sample = |offset_ms, round_trip_ms| ClockEstimate {
            offset_ms,
            round_trip_ms,
        };
        let mut samples = ClockSamples::default();
        assert_eq!(samples.add(sample(300, 120)), Some(sample(300, 120)));
        assert_eq!(samples.add(sample(250, 40)), Some(sample(250, 40)));
        assert_eq!(samples.add(sample(900, 400)), Some(sample(250, 40)));
        // Far too slow to trust: ignored.
        assert_eq!(samples.add(sample(0, 60_000)), Some(sample(250, 40)));

        // The fast sample ages out after CLOCK_SAMPLES newer ones.
        for _ in 0..CLOCK_SAMPLES {
            samples.add(sample(260, 80));
        }
        assert_eq!(samples.best(), Some(sample(260, 80)));
    }

    #[test]
    fn snapshots_are_validated() {
        assert!(validate_snapshot(&snapshot("s1"), "s1").is_ok());
        assert!(matches!(
            validate_snapshot(&snapshot("other"), "s1"),
            Err(Error::ProtocolViolation { .. })
        ));

        let mut wrong_version = snapshot("s1");
        wrong_version.protocol_version = 1;
        assert!(matches!(
            validate_snapshot(&wrong_version, "s1"),
            Err(Error::UnsupportedProtocolVersion { found: 1 })
        ));

        let mut invalid_state = snapshot("s1");
        invalid_state.state.playback.is_playing = true;
        assert!(matches!(
            validate_snapshot(&invalid_state, "s1"),
            Err(Error::InvalidRemoteState(_))
        ));
    }

    #[test]
    fn reconnect_delay_doubles_up_to_the_cap() {
        let policy = ReconnectPolicy::default();
        let within = |attempt, base_ms: u64| {
            let delay = policy.delay(attempt);
            let base = Duration::from_millis(base_ms);
            assert!(
                delay >= base && delay <= base.mul_f64(1.2),
                "attempt {attempt}: {delay:?}"
            );
        };
        within(1, 500);
        within(2, 1_000);
        within(3, 2_000);
        within(6, 10_000);
        within(50, 10_000);
    }

    #[test]
    fn server_url_must_be_a_websocket_url() {
        let config = SessionClientConfig::new("http://example.com", "c", "n");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert!(matches!(
            runtime.block_on(connect(&config)),
            Err(Error::InvalidServerUrl { .. })
        ));
    }
}
