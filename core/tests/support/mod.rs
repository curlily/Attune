//! Test-only WebSocket peer for `SessionClient` tests.
//!
//! Wraps the real `jam_protocol::coordinator::SessionCoordinator`, so client
//! tests run against the actual acceptance rules. It only does the minimum
//! routing needed for tests and is not a deployable server. It also exposes
//! hooks to drop connections, forget sessions and inject arbitrary frames.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use jam_core::protocol::{
    ClientMessage, ErrorCode, ServerMessage, SessionMusicState,
    coordinator::{Outbound, Recipient, SessionCoordinator},
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

const MAX_PARTICIPANTS: usize = 8;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

struct Connection {
    /// `(session_id, client_id)` once create/join succeeded.
    bound: Option<(String, String)>,
    outgoing: mpsc::UnboundedSender<Message>,
    tasks: Vec<JoinHandle<()>>,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, SessionCoordinator>,
    connections: HashMap<u64, Connection>,
    received: Vec<(String, ClientMessage)>,
    next_connection: u64,
}

impl Inner {
    fn send_to(&self, connection: &Connection, message: &ServerMessage) {
        let text = serde_json::to_string(message).expect("serialize server message");
        let _ = connection.outgoing.send(Message::text(text));
    }

    fn route(&self, session_id: &str, outbound: Vec<Outbound>) {
        for Outbound { to, message } in outbound {
            for connection in self.connections.values() {
                let Some((session, client)) = &connection.bound else {
                    continue;
                };
                if session != session_id {
                    continue;
                }
                let deliver = match &to {
                    Recipient::Client(id) => client == id,
                    Recipient::AllExcept(id) => client != id,
                    Recipient::All => true,
                };
                if deliver {
                    self.send_to(connection, &message);
                }
            }
        }
    }

    fn handle(&mut self, connection_id: u64, message: ClientMessage) {
        let now = now_ms();
        let Some(bound) = self
            .connections
            .get(&connection_id)
            .map(|c| c.bound.clone())
        else {
            return;
        };

        match (bound, message) {
            (Some((session_id, client_id)), message) => {
                self.received.push((client_id.clone(), message.clone()));
                let Some(coordinator) = self.sessions.get_mut(&session_id) else {
                    return;
                };
                let outbound = coordinator.handle(&client_id, message, now);
                self.route(&session_id, outbound);
            }

            (None, ClientMessage::Create(request)) => {
                let client_id = request.hello.client_id.clone();
                self.received
                    .push((client_id.clone(), ClientMessage::Create(request.clone())));
                let session_id = loop {
                    let code: String = uuid::Uuid::new_v4()
                        .into_bytes()
                        .iter()
                        .take(jam_core::protocol::JOIN_CODE_LEN)
                        .map(|byte| {
                            let alphabet = jam_core::protocol::JOIN_CODE_ALPHABET;
                            char::from(alphabet[usize::from(*byte) % alphabet.len()])
                        })
                        .collect();
                    if !self.sessions.contains_key(&code) {
                        break code;
                    }
                };
                let secret = uuid::Uuid::new_v4().simple().to_string();
                match SessionCoordinator::create(
                    session_id.clone(),
                    secret,
                    request,
                    MAX_PARTICIPANTS,
                    now,
                ) {
                    Ok((coordinator, outbound)) => {
                        self.sessions.insert(session_id.clone(), coordinator);
                        self.bind(connection_id, &session_id, &client_id);
                        self.route(&session_id, outbound);
                    }
                    Err(error) => self.reply(connection_id, error.into_message()),
                }
            }

            (None, ClientMessage::Join(request)) => {
                let client_id = request.hello.client_id.clone();
                self.received
                    .push((client_id.clone(), ClientMessage::Join(request.clone())));
                let session_id = request.session_id.clone();
                let Some(coordinator) = self.sessions.get_mut(&session_id) else {
                    return self.reply(
                        connection_id,
                        ServerMessage::Error {
                            code: ErrorCode::InvalidCredentials,
                            message: "unknown session or invalid invite".to_owned(),
                        },
                    );
                };
                match coordinator.join(request, uuid::Uuid::new_v4().simple().to_string(), now) {
                    Ok(outbound) => {
                        self.bind(connection_id, &session_id, &client_id);
                        self.route(&session_id, outbound);
                    }
                    Err(error) => self.reply(connection_id, error.into_message()),
                }
            }

            (None, _) => self.reply(
                connection_id,
                ServerMessage::Error {
                    code: ErrorCode::NotJoined,
                    message: "create or join first".to_owned(),
                },
            ),
        }
    }

    fn bind(&mut self, connection_id: u64, session_id: &str, client_id: &str) {
        if let Some(connection) = self.connections.get_mut(&connection_id) {
            connection.bound = Some((session_id.to_owned(), client_id.to_owned()));
        }
    }

    fn reply(&self, connection_id: u64, message: ServerMessage) {
        if let Some(connection) = self.connections.get(&connection_id) {
            self.send_to(connection, &message);
        }
    }

    fn disconnected(&mut self, connection_id: u64) {
        let Some(connection) = self.connections.remove(&connection_id) else {
            return;
        };
        let Some((session_id, client_id)) = connection.bound else {
            return;
        };
        let still_connected = self
            .connections
            .values()
            .any(|c| c.bound.as_ref() == Some(&(session_id.clone(), client_id.clone())));
        if still_connected {
            return;
        }
        if let Some(coordinator) = self.sessions.get_mut(&session_id) {
            let outbound = coordinator.set_connected(&client_id, false);
            self.route(&session_id, outbound);
        }
    }
}

pub struct TestServer {
    pub url: String,
    inner: Arc<Mutex<Inner>>,
    accept_task: JoinHandle<()>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let url = format!("ws://{}", listener.local_addr().expect("local addr"));
        let inner = Arc::new(Mutex::new(Inner::default()));

        let accept_inner = inner.clone();
        let accept_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let inner = accept_inner.clone();
                tokio::spawn(async move {
                    let Ok(socket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    serve(inner, socket);
                });
            }
        });

        Self {
            url,
            inner,
            accept_task,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("test server lock")
    }

    /// Drop every open connection, as if the network failed.
    pub fn kill_connections(&self) {
        let mut inner = self.lock();
        for (_, connection) in inner.connections.drain() {
            for task in connection.tasks {
                task.abort();
            }
        }
    }

    /// Stop accepting connections and drop the open ones.
    pub fn stop(&self) {
        self.accept_task.abort();
        self.kill_connections();
    }

    /// Lose all sessions, as if the server restarted.
    pub fn forget_sessions(&self) {
        self.lock().sessions.clear();
    }

    /// Send a message to every connection of `client_id`.
    pub fn inject(&self, client_id: &str, message: &ServerMessage) {
        self.inject_raw(
            client_id,
            &serde_json::to_string(message).expect("serialize"),
        );
    }

    /// Send an arbitrary text frame to every connection of `client_id`.
    pub fn inject_raw(&self, client_id: &str, text: &str) {
        let inner = self.lock();
        for connection in inner.connections.values() {
            if connection
                .bound
                .as_ref()
                .is_some_and(|(_, client)| client == client_id)
            {
                let _ = connection.outgoing.send(Message::text(text.to_owned()));
            }
        }
    }

    /// Everything `client_id` has sent, in order.
    pub fn received(&self, client_id: &str) -> Vec<ClientMessage> {
        self.lock()
            .received
            .iter()
            .filter(|(client, _)| client == client_id)
            .map(|(_, message)| message.clone())
            .collect()
    }

    pub fn session_state(&self, session_id: &str) -> SessionMusicState {
        self.lock().sessions[session_id].state().clone()
    }

    pub fn sequence(&self, session_id: &str) -> u64 {
        self.lock().sessions[session_id].sequence()
    }

    pub fn participant_count(&self, session_id: &str) -> usize {
        self.lock().sessions[session_id].participants().len()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(
    inner: Arc<Mutex<Inner>>,
    socket: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) {
    let (mut sink, mut stream) = socket.split();
    let (outgoing, mut queue) = mpsc::unbounded_channel::<Message>();

    let writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    let mut guard = inner.lock().expect("test server lock");
    let connection_id = guard.next_connection;
    guard.next_connection += 1;

    let reader_inner = inner.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(frame)) = stream.next().await {
            match frame {
                Message::Text(text) => {
                    let Ok(message) = serde_json::from_str::<ClientMessage>(text.as_str()) else {
                        continue;
                    };
                    reader_inner
                        .lock()
                        .expect("test server lock")
                        .handle(connection_id, message);
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        reader_inner
            .lock()
            .expect("test server lock")
            .disconnected(connection_id);
    });

    guard.connections.insert(
        connection_id,
        Connection {
            bound: None,
            outgoing,
            tasks: vec![reader, writer],
        },
    );
}
