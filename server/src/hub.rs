//! Session registry and per-session actors.
//!
//! Each session is one task owning its `SessionCoordinator` and the outgoing
//! queues of its participants' connections. Connections talk to it over a
//! bounded channel, so a session never shares state or holds a lock across
//! I/O, and sessions don't block each other.

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use attune_protocol::{
    ClientMessage, CreateSession, ErrorCode, JOIN_CODE_ALPHABET, JOIN_CODE_LEN, JoinSession,
    ServerMessage,
    coordinator::{Outbound, Recipient, SessionCoordinator},
    normalize_join_code,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::{Duration, Instant, interval},
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{ServerConfig, limiter::JoinLimiter};

const SESSION_COMMAND_CAPACITY: usize = 64;
const EXPIRY_CHECK_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// An error reply for a failed create/join (boxed: `ServerMessage` is large).
pub(crate) type Rejection = Box<ServerMessage>;

fn rejection(code: ErrorCode, message: &str) -> Rejection {
    Box::new(ServerMessage::Error {
        code,
        message: message.to_owned(),
    })
}

/// A rejoin secret: 244 random bits from two v4 UUIDs, as hex.
fn new_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// A random join code. Bytes are drawn from v4 UUIDs and kept only below
/// the largest multiple of the alphabet size, so every letter is equally
/// likely.
fn random_code() -> String {
    let limit = (256 / JOIN_CODE_ALPHABET.len() * JOIN_CODE_ALPHABET.len()) as u8;
    let mut code = String::with_capacity(JOIN_CODE_LEN);
    while code.len() < JOIN_CODE_LEN {
        for byte in Uuid::new_v4().into_bytes() {
            if byte < limit && code.len() < JOIN_CODE_LEN {
                code.push(char::from(
                    JOIN_CODE_ALPHABET[usize::from(byte) % JOIN_CODE_ALPHABET.len()],
                ));
            }
        }
    }
    code
}

/// Short ID for log lines, so join codes don't end up in logs.
fn log_id() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_owned()
}

/// The write side of one WebSocket connection.
#[derive(Clone)]
pub(crate) struct ConnectionHandle {
    pub id: u64,
    pub outgoing: mpsc::Sender<ServerMessage>,
}

pub(crate) enum SessionCommand {
    Join {
        request: JoinSession,
        connection: ConnectionHandle,
        reply: oneshot::Sender<Result<(), Rejection>>,
    },
    Message {
        client_id: String,
        connection_id: u64,
        message: ClientMessage,
    },
    Disconnected {
        client_id: String,
        connection_id: u64,
    },
}

/// A connection's membership after a successful create/join.
pub(crate) struct Binding {
    /// For log lines only (never the join code).
    pub log_id: String,
    pub client_id: String,
    pub commands: mpsc::Sender<SessionCommand>,
}

/// A running session, as the hub sees it.
#[derive(Clone)]
struct SessionEntry {
    log_id: String,
    commands: mpsc::Sender<SessionCommand>,
}

pub(crate) struct Hub {
    config: ServerConfig,
    /// By join code.
    sessions: Mutex<HashMap<String, SessionEntry>>,
    limiter: JoinLimiter,
    next_connection_id: AtomicU64,
}

impl Hub {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            limiter: JoinLimiter::new(config.join_attempts_per_minute),
            config,
            sessions: Mutex::new(HashMap::new()),
            next_connection_id: AtomicU64::new(1),
        }
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn next_connection_id(&self) -> u64 {
        self.next_connection_id.fetch_add(1, Ordering::Relaxed)
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, SessionEntry>> {
        // A poisoned lock only means another task panicked mid-update of a
        // plain map; the map itself is still usable.
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn create(
        self: &Arc<Self>,
        request: CreateSession,
        connection: ConnectionHandle,
    ) -> Result<Binding, Rejection> {
        let mut sessions = self.sessions();
        if sessions.len() >= self.config.max_sessions {
            return Err(rejection(
                ErrorCode::ServerBusy,
                "the server is at its session limit",
            ));
        }

        // The session limit is far below the number of codes, so a free
        // one turns up within a few tries.
        let code = loop {
            let code = random_code();
            if !sessions.contains_key(&code) {
                break code;
            }
        };
        let log_id = log_id();
        let client_id = request.hello.client_id.clone();

        let (coordinator, outbound) = SessionCoordinator::create(
            code.clone(),
            new_secret(),
            request,
            self.config.max_participants,
            now_ms(),
        )
        .map_err(|error| Box::new(error.into_message()))?;

        let (commands, receiver) = mpsc::channel(SESSION_COMMAND_CAPACITY);
        sessions.insert(
            code,
            SessionEntry {
                log_id: log_id.clone(),
                commands: commands.clone(),
            },
        );
        drop(sessions);

        let mut actor = SessionActor {
            hub: self.clone(),
            log_id: log_id.clone(),
            coordinator,
            connections: HashMap::from([(client_id.clone(), connection)]),
            disconnected_since: HashMap::new(),
            grace: self.config.reconnect_grace,
        };
        info!(session = %log_id, %client_id, "Session created");
        actor.route(outbound);
        tokio::spawn(actor.run(receiver));

        Ok(Binding {
            log_id,
            client_id,
            commands,
        })
    }

    /// Join by code. Failed attempts count against `ip`'s limit.
    pub async fn join(
        &self,
        mut request: JoinSession,
        connection: ConnectionHandle,
        ip: IpAddr,
    ) -> Result<Binding, Rejection> {
        if !self.limiter.allows(ip) {
            return Err(rejection(
                ErrorCode::RateLimited,
                "too many failed join attempts; try again in a minute",
            ));
        }
        let invalid = || {
            self.limiter.failed(ip);
            rejection(
                ErrorCode::InvalidCredentials,
                "unknown join code or invalid rejoin secret",
            )
        };
        let entry = normalize_join_code(&request.session_id)
            .and_then(|code| {
                let entry = self.sessions().get(&code).cloned();
                request.session_id = code;
                entry
            })
            .ok_or_else(invalid)?;
        let client_id = request.hello.client_id.clone();

        let (reply, response) = oneshot::channel();
        entry
            .commands
            .send(SessionCommand::Join {
                request,
                connection,
                reply,
            })
            .await
            .map_err(|_| invalid())?;
        // A dropped reply means the session ended meanwhile.
        match response.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                if matches!(
                    *error,
                    ServerMessage::Error {
                        code: ErrorCode::InvalidCredentials,
                        ..
                    }
                ) {
                    self.limiter.failed(ip);
                }
                return Err(error);
            }
            Err(_) => return Err(invalid()),
        }

        Ok(Binding {
            log_id: entry.log_id,
            client_id,
            commands: entry.commands,
        })
    }

    fn remove(&self, code: &str) {
        self.sessions().remove(code);
    }
}

struct SessionActor {
    hub: Arc<Hub>,
    /// For log lines only (never the join code).
    log_id: String,
    coordinator: SessionCoordinator,
    /// Current connection of each connected participant.
    connections: HashMap<String, ConnectionHandle>,
    disconnected_since: HashMap<String, Instant>,
    grace: Duration,
}

impl SessionActor {
    async fn run(mut self, mut commands: mpsc::Receiver<SessionCommand>) {
        let code = self.coordinator.session_id().to_owned();
        let mut expiry = interval(EXPIRY_CHECK_INTERVAL);

        while !self.coordinator.is_empty() {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.handle(command),
                    None => break, // unreachable while the hub holds a sender
                },
                _ = expiry.tick() => self.expire_disconnected(),
            }
        }

        // Remove first so no new join reaches this actor; anything still
        // queued is dropped and its sender sees the session as gone.
        self.hub.remove(&code);
        info!(session = %self.log_id, "Session ended");
    }

    fn handle(&mut self, command: SessionCommand) {
        match command {
            SessionCommand::Join {
                request,
                connection,
                reply,
            } => {
                let client_id = request.hello.client_id.clone();
                match self.coordinator.join(request, new_secret(), now_ms()) {
                    Ok(outbound) => {
                        // A rejoin replaces the old connection; dropping its
                        // handle lets its writer finish and close the socket.
                        self.connections.insert(client_id.clone(), connection);
                        self.disconnected_since.remove(&client_id);
                        let _ = reply.send(Ok(()));
                        info!(session = %self.log_id, %client_id, "Participant joined");
                        self.route(outbound);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(Box::new(error.into_message())));
                    }
                }
            }

            SessionCommand::Message {
                client_id,
                connection_id,
                message,
            } => {
                if !self.is_current(&client_id, connection_id) {
                    debug!(%client_id, "Ignoring a message from a replaced connection");
                    return;
                }
                let outbound = self.coordinator.handle(&client_id, message, now_ms());
                self.route(outbound);
                if !self.coordinator.is_member(&client_id) {
                    // Left: the `session.left` reply is already queued.
                    self.connections.remove(&client_id);
                    info!(session = %self.log_id, %client_id, "Participant left");
                }
            }

            SessionCommand::Disconnected {
                client_id,
                connection_id,
            } => {
                if self.is_current(&client_id, connection_id) {
                    self.mark_disconnected(&client_id);
                }
            }
        }
    }

    fn is_current(&self, client_id: &str, connection_id: u64) -> bool {
        self.connections
            .get(client_id)
            .is_some_and(|connection| connection.id == connection_id)
    }

    fn mark_disconnected(&mut self, client_id: &str) {
        self.connections.remove(client_id);
        if !self.coordinator.is_member(client_id) {
            return;
        }
        debug!(session = %self.log_id, %client_id, "Participant disconnected");
        self.disconnected_since
            .insert(client_id.to_owned(), Instant::now());
        let outbound = self.coordinator.set_connected(client_id, false);
        self.route(outbound);
    }

    fn expire_disconnected(&mut self) {
        let expired: Vec<String> = self
            .disconnected_since
            .iter()
            .filter(|(_, since)| since.elapsed() >= self.grace)
            .map(|(client_id, _)| client_id.clone())
            .collect();
        for client_id in expired {
            self.disconnected_since.remove(&client_id);
            info!(session = %self.log_id, %client_id, "Reconnect grace period expired");
            let outbound = self.coordinator.remove(&client_id, now_ms());
            self.route(outbound);
        }
    }

    /// Deliver coordinator output. A participant whose queue is full is
    /// dropped as disconnected (it can rejoin and resync) instead of letting
    /// memory grow for a client that can't keep up.
    fn route(&mut self, outbound: Vec<Outbound>) {
        let mut queue: VecDeque<Outbound> = outbound.into();
        while let Some(Outbound { to, message }) = queue.pop_front() {
            let recipients: Vec<String> = self
                .connections
                .keys()
                .filter(|client_id| match &to {
                    Recipient::Client(id) => *client_id == id,
                    Recipient::AllExcept(id) => *client_id != id,
                    Recipient::All => true,
                })
                .cloned()
                .collect();

            for client_id in recipients {
                let Some(connection) = self.connections.get(&client_id) else {
                    continue;
                };
                if connection.outgoing.try_send(message.clone()).is_err() {
                    warn!(
                        session = %self.log_id,
                        %client_id,
                        "Dropping a connection that cannot keep up"
                    );
                    self.connections.remove(&client_id);
                    if self.coordinator.is_member(&client_id) {
                        self.disconnected_since
                            .insert(client_id.clone(), Instant::now());
                        queue.extend(self.coordinator.set_connected(&client_id, false));
                    }
                }
            }
        }
    }
}
