//! The real relay server driven by the real desktop `SessionClient`.

use std::time::Duration;

use attune_core::{
    Error,
    network::{
        JoinCredentials, ReconnectPolicy, SessionClient, SessionClientConfig, SessionEvent,
        SessionStart, SubmitOutcome,
    },
    protocol::{
        ErrorCode, ParticipantStatus, QueueEntry, ServerMessage, SessionMusicState,
        SessionOperation as Op, SessionPlayback, TrackDescriptor,
    },
};
use attune_server::{ServerConfig, serve};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, oneshot},
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;

const WAIT: Duration = Duration::from_secs(3);

struct Server {
    address: String,
    shutdown: Option<oneshot::Sender<()>>,
}

impl Server {
    async fn start(config: ServerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address").to_string();
        let (shutdown, stop) = oneshot::channel::<()>();
        tokio::spawn(serve(listener, config, async {
            let _ = stop.await;
        }));
        Self {
            address,
            shutdown: Some(shutdown),
        }
    }

    fn url(&self) -> String {
        format!("ws://{}/ws", self.address)
    }

    fn client(&self, client_id: &str) -> SessionClientConfig {
        let mut config =
            SessionClientConfig::new(self.url(), client_id, format!("User {client_id}"));
        config.request_timeout = Duration::from_secs(2);
        config.reconnect = ReconnectPolicy {
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(200),
        };
        config
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn entry(id: &str) -> QueueEntry {
    QueueEntry {
        entry_id: id.to_owned(),
        track: TrackDescriptor {
            title: format!("Song {id}"),
            artists: vec!["Artist".to_owned()],
            isrc: None,
            album: None,
            duration_ms: Some(200_000),
        },
    }
}

fn music(ids: &[&str]) -> SessionMusicState {
    SessionMusicState {
        queue: ids.iter().map(|id| entry(id)).collect(),
        current_entry_id: ids.first().map(|id| (*id).to_owned()),
        playback: SessionPlayback {
            is_playing: true,
            position_ms: 0,
            position_captured_at_ms: 0,
        },
    }
}

async fn wait_for(
    events: &mut broadcast::Receiver<SessionEvent>,
    predicate: impl Fn(&SessionEvent) -> bool,
) -> SessionEvent {
    timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("event channel open");
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .expect("expected event did not arrive")
}

fn participants_where(
    predicate: impl Fn(&[attune_core::protocol::Participant]) -> bool,
) -> impl Fn(&SessionEvent) -> bool {
    move |event| matches!(event, SessionEvent::Participants(p) if predicate(p))
}

async fn pair(server: &Server) -> (SessionClient, SessionStart, SessionClient, SessionStart) {
    let (a, a_start) = SessionClient::create(server.client("a"), music(&["s1", "s2", "s3"]))
        .await
        .expect("create");
    let (b, b_start) = SessionClient::join(server.client("b"), a_start.credentials.clone())
        .await
        .expect("join");
    (a, a_start, b, b_start)
}

#[tokio::test]
async fn health_endpoint_responds() {
    let server = Server::start(ServerConfig::default()).await;
    let mut stream = TcpStream::connect(&server.address).await.expect("connect");
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("ok"), "{response}");
}

#[tokio::test]
async fn two_clients_sync_through_the_server_without_echo() {
    let server = Server::start(ServerConfig::default()).await;
    let (a, mut a_start, _b, mut b_start) = pair(&server).await;

    let outcome = a
        .submit_operation(Op::PlaybackSetPlaying { is_playing: false }, 0)
        .await
        .expect("submit");
    assert!(matches!(
        outcome,
        SubmitOutcome::Applied { sequence: 1, .. }
    ));

    let event = wait_for(&mut b_start.events, |e| {
        matches!(e, SessionEvent::RemoteOperation { .. })
    })
    .await;
    assert!(matches!(
        event,
        SessionEvent::RemoteOperation { sequence: 1, ref origin_client_id, .. } if origin_client_id.as_deref() == Some("a")
    ));

    // A sees B join but never its own operation.
    let a_events = timeout(Duration::from_millis(300), async {
        loop {
            if let Ok(SessionEvent::RemoteOperation { .. }) = a_start.events.recv().await {
                return true;
            }
        }
    })
    .await;
    assert!(a_events.is_err(), "the sender received its own operation");
}

#[tokio::test]
async fn song_nobody_can_play_is_skipped_for_everyone() {
    let server = Server::start(ServerConfig::default()).await;
    let (a, mut a_start, b, mut b_start) = pair(&server).await;
    a.set_unavailable(vec!["s2".to_owned()])
        .await
        .expect("report a");
    b.set_unavailable(vec!["s2".to_owned()])
        .await
        .expect("report b");
    wait_for(
        &mut a_start.events,
        participants_where(|p| p.iter().all(|p| p.unavailable_entry_ids == ["s2"])),
    )
    .await;

    let advance = Op::PlaybackAdvance {
        target_entry_id: "s2".to_owned(),
        position_ms: 0,
        is_playing: true,
    };
    a.submit_operation(advance, 0).await.expect("advance");

    let skip_to_s3 = |event: &SessionEvent| {
        matches!(
            event,
            SessionEvent::RemoteOperation {
                sequence: 2,
                origin_client_id: None,
                operation: Op::PlaybackAdvance { target_entry_id, .. },
                ..
            } if target_entry_id == "s3"
        )
    };
    // The server's skip reaches the client whose change caused it...
    wait_for(&mut a_start.events, skip_to_s3).await;
    // ...and everyone else, after the original advance.
    let first = wait_for(&mut b_start.events, |e| {
        matches!(e, SessionEvent::RemoteOperation { .. })
    })
    .await;
    assert!(matches!(
        first,
        SessionEvent::RemoteOperation { sequence: 1, .. }
    ));
    wait_for(&mut b_start.events, skip_to_s3).await;
}

#[tokio::test]
async fn disconnected_participant_keeps_its_seat_until_the_grace_period_ends() {
    let config = ServerConfig {
        reconnect_grace: Duration::from_millis(400),
        ..ServerConfig::default()
    };
    let server = Server::start(config).await;
    let (_a, mut a_start, b, _b_start) = pair(&server).await;

    b.close().await;
    wait_for(
        &mut a_start.events,
        participants_where(|p| {
            p.len() == 2 && p.iter().any(|p| p.client_id == "b" && !p.connected)
        }),
    )
    .await;
    wait_for(&mut a_start.events, participants_where(|p| p.len() == 1)).await;
}

#[tokio::test]
async fn returning_within_the_grace_period_keeps_the_seat() {
    let server = Server::start(ServerConfig::default()).await;
    let (_a, mut a_start, b, b_start) = pair(&server).await;
    let credentials = b_start.credentials.clone();
    b.set_status(ParticipantStatus::Detached)
        .await
        .expect("status");

    b.close().await;
    wait_for(
        &mut a_start.events,
        participants_where(|p| p.iter().any(|p| !p.connected)),
    )
    .await;
    let (_b, _start) = SessionClient::join(server.client("b"), credentials)
        .await
        .expect("rejoin");

    let event = wait_for(
        &mut a_start.events,
        participants_where(|p| p.len() == 2 && p.iter().all(|p| p.connected)),
    )
    .await;
    let SessionEvent::Participants(participants) = event else {
        unreachable!()
    };
    assert_eq!(
        participants[1].status,
        ParticipantStatus::Detached,
        "stepped out is kept"
    );
}

#[tokio::test]
async fn session_ends_when_the_last_participant_leaves() {
    let server = Server::start(ServerConfig::default()).await;
    let (a, a_start) = SessionClient::create(server.client("a"), music(&["s1"]))
        .await
        .expect("create");
    a.leave().await.expect("leave");

    // Give the session task a moment to shut down.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let error = SessionClient::join(server.client("b"), a_start.credentials)
        .await
        .expect_err("session is gone");
    assert!(matches!(
        error,
        Error::Remote {
            code: ErrorCode::InvalidCredentials,
            ..
        }
    ));
}

#[tokio::test]
async fn session_limit_is_enforced() {
    let config = ServerConfig {
        max_sessions: 1,
        ..ServerConfig::default()
    };
    let server = Server::start(config).await;
    let _first = SessionClient::create(server.client("a"), music(&["s1"]))
        .await
        .expect("first create");
    let error = SessionClient::create(server.client("b"), music(&["s1"]))
        .await
        .expect_err("second create");
    assert!(matches!(
        error,
        Error::Remote {
            code: ErrorCode::ServerBusy,
            ..
        }
    ));
}

fn remote_code(error: Error) -> ErrorCode {
    match error {
        Error::Remote { code, .. } => code,
        other => panic!("expected a server error, got {other}"),
    }
}

#[tokio::test]
async fn codes_are_case_insensitive_and_wrong_ones_are_refused() {
    let server = Server::start(ServerConfig::default()).await;
    let (_a, a_start) = SessionClient::create(server.client("a"), music(&["s1"]))
        .await
        .expect("create");
    let code = a_start.credentials.code.clone();
    let typed = JoinCredentials::from_code(&code.to_lowercase()).expect("code");
    SessionClient::join(server.client("b"), typed)
        .await
        .expect("join with a lowercase code");

    let other = if code == "AAAA" { "BBBB" } else { "AAAA" };
    let error = SessionClient::join(
        server.client("c"),
        JoinCredentials::from_code(other).expect("code"),
    )
    .await
    .expect_err("unknown code");
    assert_eq!(remote_code(error), ErrorCode::InvalidCredentials);
}

#[tokio::test]
async fn guessing_codes_is_rate_limited_per_address() {
    let config = ServerConfig {
        join_attempts_per_minute: 3,
        ..ServerConfig::default()
    };
    let server = Server::start(config).await;
    let (_a, a_start) = SessionClient::create(server.client("a"), music(&["s1"]))
        .await
        .expect("create");
    let code = a_start.credentials.code.clone();
    let wrong = if code == "AAAA" { "BBBB" } else { "AAAA" };

    // A join that works doesn't count.
    SessionClient::join(server.client("b"), a_start.credentials.clone())
        .await
        .expect("join");
    for _ in 0..3 {
        let error = SessionClient::join(
            server.client("guess"),
            JoinCredentials::from_code(wrong).expect("code"),
        )
        .await
        .expect_err("wrong code");
        assert_eq!(remote_code(error), ErrorCode::InvalidCredentials);
    }
    // Now even the right code is refused until the limit refills.
    let error = SessionClient::join(server.client("c"), a_start.credentials.clone())
        .await
        .expect_err("limited");
    assert_eq!(remote_code(error), ErrorCode::RateLimited);
}

/// Raw WebSocket client for protocol-level checks.
async fn raw(
    server: &Server,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>> {
    let (socket, _) = tokio_tungstenite::connect_async(server.url())
        .await
        .expect("connect");
    socket
}

async fn next_server_message(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
) -> ServerMessage {
    timeout(WAIT, async {
        loop {
            match socket.next().await.expect("open").expect("frame") {
                Message::Text(text) => {
                    return serde_json::from_str(text.as_str()).expect("server message");
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("reply")
}

#[tokio::test]
async fn first_message_must_be_a_handshake() {
    let server = Server::start(ServerConfig::default()).await;
    let mut socket = raw(&server).await;
    socket
        .send(Message::text(r#"{"type":"session.leave"}"#))
        .await
        .expect("send");
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Error {
            code: ErrorCode::NotJoined,
            ..
        }
    ));
}

#[tokio::test]
async fn garbage_after_joining_gets_an_error_but_keeps_the_connection() {
    let server = Server::start(ServerConfig::default()).await;
    let (_a, a_start) = SessionClient::create(server.client("a"), music(&["s1"]))
        .await
        .expect("create");

    let mut socket = raw(&server).await;
    let join = serde_json::json!({
        "type": "session.join",
        "protocolVersion": 1,
        "clientId": "raw",
        "displayName": "Raw",
        "sessionId": a_start.credentials.code,
    });
    socket
        .send(Message::text(join.to_string()))
        .await
        .expect("join");
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Joined { .. }
    ));

    socket
        .send(Message::text("{garbage"))
        .await
        .expect("garbage");
    let error = loop {
        match next_server_message(&mut socket).await {
            ServerMessage::Participants { .. } => continue,
            other => break other,
        }
    };
    assert!(matches!(
        error,
        ServerMessage::Error {
            code: ErrorCode::InvalidMessage,
            ..
        }
    ));

    // Still in the session.
    socket
        .send(Message::text(
            r#"{"type":"session.snapshot.request","requestId":"r1"}"#,
        ))
        .await
        .expect("request");
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Snapshot { .. }
    ));
}
