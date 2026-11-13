//! `SessionClient` against the test-only peer in `support/` (which runs the
//! real session coordinator).

mod support;

use std::time::Duration;

use attune_core::{
    Error,
    network::{
        CloseReason, JoinCredentials, ReconnectPolicy, SessionClient, SessionClientConfig,
        SessionEvent, SessionStart, SubmitOutcome,
    },
    protocol::{
        ClientMessage, ErrorCode, PROTOCOL_VERSION, ParticipantStatus, QueueEntry, RejectCode,
        ServerMessage, SessionMusicState, SessionOperation as Op, SessionPlayback, TrackDescriptor,
        state,
    },
};
use support::TestServer;
use tokio::{sync::broadcast, time::timeout};

const WAIT: Duration = Duration::from_secs(3);
const QUIET: Duration = Duration::from_millis(300);

fn config(server: &TestServer, client_id: &str) -> SessionClientConfig {
    let mut config = SessionClientConfig::new(&server.url, client_id, format!("User {client_id}"));
    config.provider = Some("tidal".to_owned());
    config.request_timeout = Duration::from_secs(2);
    config.reconnect = ReconnectPolicy {
        initial_delay: Duration::from_millis(50),
        max_delay: Duration::from_millis(200),
    };
    config
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
            cover_url: None,
        },
        added_by: None,
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

fn pause() -> Op {
    Op::PlaybackSetPlaying { is_playing: false }
}

/// Wait for the first event matching `predicate`, skipping others.
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

/// Assert no event matching `predicate` arrives within `QUIET`.
async fn assert_quiet(
    events: &mut broadcast::Receiver<SessionEvent>,
    predicate: impl Fn(&SessionEvent) -> bool,
) {
    let result = timeout(QUIET, async {
        loop {
            match events.recv().await {
                Ok(event) if predicate(&event) => return event,
                Ok(_) => {}
                Err(_) => std::future::pending::<()>().await,
            }
        }
    })
    .await;
    if let Ok(event) = result {
        panic!("unexpected event {event:?}");
    }
}

fn is_remote(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::RemoteOperation { .. })
}

async fn create(
    server: &TestServer,
    client_id: &str,
    ids: &[&str],
) -> (SessionClient, SessionStart) {
    SessionClient::create(config(server, client_id), music(ids))
        .await
        .expect("create session")
}

async fn join(
    server: &TestServer,
    client_id: &str,
    credentials: &JoinCredentials,
) -> (SessionClient, SessionStart) {
    SessionClient::join(config(server, client_id), credentials.clone())
        .await
        .expect("join session")
}

/// A creates a session, B joins it.
async fn pair(server: &TestServer) -> (SessionClient, SessionStart, SessionClient, SessionStart) {
    let (a, a_start) = create(server, "a", &["s1", "s2", "s3"]).await;
    let (b, b_start) = join(server, "b", &a_start.credentials).await;
    (a, a_start, b, b_start)
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_and_join_exchange_snapshots_and_participants() {
    let server = TestServer::start().await;
    let (_a, mut a_start, _b, b_start) = pair(&server).await;

    let credentials = &a_start.credentials;
    assert_eq!(credentials.code.len(), 4);
    assert!(credentials.rejoin_secret.is_some());
    assert!(b_start.credentials.rejoin_secret.is_some());
    assert_ne!(b_start.credentials.rejoin_secret, credentials.rejoin_secret);
    assert_eq!(a_start.snapshot.revision, 0);
    assert_eq!(b_start.snapshot.session_id, credentials.code);
    assert_eq!(b_start.snapshot.state.queue.len(), 3);

    // A learns that B joined.
    let event = wait_for(
        &mut a_start.events,
        |e| matches!(e, SessionEvent::Participants(p) if p.len() == 2),
    )
    .await;
    let SessionEvent::Participants(participants) = event else {
        unreachable!()
    };
    assert_eq!(participants[1].client_id, "b");
    assert_eq!(participants[1].display_name, "User b");
    assert_eq!(participants[1].status, ParticipantStatus::Resyncing);

    // The handshake carried identity and version exactly once.
    match &server.received("b")[0] {
        ClientMessage::Join(join) => {
            assert_eq!(join.hello.protocol_version, PROTOCOL_VERSION);
            assert_eq!(join.hello.client_id, "b");
            assert_eq!(join.session_id, credentials.code);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn taking_a_seat_needs_its_secret_and_the_error_does_not_leak_it() {
    let server = TestServer::start().await;
    let (_a, a_start) = create(&server, "a", &["s1"]).await;
    let secret = a_start.credentials.rejoin_secret.clone().expect("secret");
    // Someone else using A's (visible) client ID with a guessed secret.
    let wrong = JoinCredentials {
        code: a_start.credentials.code.clone(),
        rejoin_secret: Some("not-the-secret".to_owned()),
        invite: None,
    };

    let error = SessionClient::join(config(&server, "a"), wrong)
        .await
        .expect_err("wrong secret");
    assert!(matches!(
        error,
        Error::Remote {
            code: ErrorCode::InvalidCredentials,
            ..
        }
    ));
    assert!(!error.to_string().contains(&secret));
}

#[tokio::test]
async fn the_clock_estimate_arrives_shortly_after_joining() {
    let server = TestServer::start().await;
    let (a, _a_start) = create(&server, "a", &["s1"]).await;
    timeout(WAIT, async {
        while a.clock().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("clock estimate");
    let clock = a.clock().expect("clock");
    // Same machine, same clock.
    assert!(clock.offset_ms.abs() < 50, "{clock:?}");
    assert!(
        server
            .received("a")
            .iter()
            .any(|message| matches!(message, ClientMessage::TimePing { .. }))
    );
}

#[tokio::test]
async fn unreachable_server_times_out_or_fails() {
    let server = TestServer::start().await;
    let url = server.url.clone();
    server.stop();
    drop(server);
    let mut config = SessionClientConfig::new(url, "a", "A");
    config.request_timeout = Duration::from_millis(500);
    assert!(SessionClient::create(config, music(&["s1"])).await.is_err());
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn operation_is_acked_to_sender_and_broadcast_to_others_without_echo() {
    let server = TestServer::start().await;
    let (a, mut a_start, _b, mut b_start) = pair(&server).await;

    let outcome = a.submit_operation(pause(), 0).await.expect("submit");
    let SubmitOutcome::Applied { sequence, .. } = outcome else {
        panic!("unexpected {outcome:?}")
    };
    assert_eq!(sequence, 1);

    let event = wait_for(&mut b_start.events, is_remote).await;
    match event {
        SessionEvent::RemoteOperation {
            sequence,
            origin_client_id,
            operation,
            ..
        } => {
            assert_eq!(sequence, 1);
            assert_eq!(origin_client_id.as_deref(), Some("a"));
            assert_eq!(operation, pause());
        }
        other => panic!("unexpected {other:?}"),
    }

    assert_quiet(&mut a_start.events, is_remote).await;

    // The request carried a fresh operation id and the given base sequence.
    let sent: Vec<_> = server
        .received("a")
        .into_iter()
        .filter_map(|m| match m {
            ClientMessage::Operation(request) => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].base_sequence, 0);
    assert!(uuid::Uuid::parse_str(&sent[0].operation_id).is_ok());
}

#[tokio::test]
async fn noop_and_stale_outcomes_are_reported() {
    let server = TestServer::start().await;
    let (a, _a_start, b, _b_start) = pair(&server).await;

    let unchanged = a
        .submit_operation(Op::PlaybackSetPlaying { is_playing: true }, 0)
        .await
        .expect("submit");
    assert_eq!(unchanged, SubmitOutcome::Unchanged { sequence: 0 });

    b.submit_operation(
        Op::QueueRemove {
            entry_id: "s3".to_owned(),
        },
        0,
    )
    .await
    .expect("remove");
    let replace = Op::QueueReplace {
        entries: vec![entry("s1"), entry("new")],
        current_entry_id: Some("s1".to_owned()),
        playback: None,
    };
    match a.submit_operation(replace, 0).await.expect("submit") {
        SubmitOutcome::Rejected {
            code,
            current_sequence,
            ..
        } => {
            assert_eq!(code, RejectCode::StaleQueue);
            assert_eq!(current_sequence, 1);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn duplicates_are_ignored_and_gaps_reported() {
    let server = TestServer::start().await;
    let (a, _a_start, _b, mut b_start) = pair(&server).await;

    a.submit_operation(pause(), 0).await.expect("submit");
    wait_for(&mut b_start.events, is_remote).await;

    let accepted = |sequence| ServerMessage::OperationAccepted {
        sequence,
        origin_client_id: Some("a".to_owned()),
        operation: pause(),
        accepted_at_ms: 0,
    };
    server.inject("b", &accepted(1)); // replay
    assert_quiet(&mut b_start.events, is_remote).await;

    server.inject("b", &accepted(3)); // 2 is missing
    let gap = wait_for(&mut b_start.events, |e| {
        matches!(e, SessionEvent::SequenceGap { .. })
    })
    .await;
    assert!(matches!(
        gap,
        SessionEvent::SequenceGap {
            expected: 2,
            received: 3
        }
    ));
}

#[tokio::test]
async fn snapshot_request_resets_the_sequence_after_a_gap() {
    let server = TestServer::start().await;
    let (a, _a_start, b, mut b_start) = pair(&server).await;

    a.submit_operation(pause(), 0).await.expect("submit 1");
    wait_for(&mut b_start.events, is_remote).await;
    server.inject(
        "b",
        &ServerMessage::OperationAccepted {
            sequence: 9,
            origin_client_id: Some("a".to_owned()),
            operation: pause(),
            accepted_at_ms: 0,
        },
    );
    wait_for(&mut b_start.events, |e| {
        matches!(e, SessionEvent::SequenceGap { .. })
    })
    .await;

    let snapshot = b.request_snapshot().await.expect("snapshot");
    assert_eq!(snapshot.revision, 1);
    assert!(!snapshot.state.playback.is_playing);

    // Following resumes from the snapshot's revision.
    a.submit_operation(Op::PlaybackSetPlaying { is_playing: true }, 1)
        .await
        .expect("submit 2");
    let event = wait_for(&mut b_start.events, is_remote).await;
    assert!(matches!(
        event,
        SessionEvent::RemoteOperation { sequence: 2, .. }
    ));
}

#[tokio::test]
async fn server_issued_operations_are_delivered_and_own_echoes_are_not() {
    let server = TestServer::start().await;
    let (_a, _a_start, _b, mut b_start) = pair(&server).await;

    let accepted = |sequence, origin: Option<&str>| ServerMessage::OperationAccepted {
        sequence,
        origin_client_id: origin.map(str::to_owned),
        operation: pause(),
        accepted_at_ms: 0,
    };

    server.inject("b", &accepted(1, None));
    let event = wait_for(&mut b_start.events, is_remote).await;
    assert!(matches!(
        event,
        SessionEvent::RemoteOperation {
            sequence: 1,
            origin_client_id: None,
            ..
        }
    ));

    // An (invalid) echo of B's own operation is not delivered but still
    // counts, so sequence 3 arrives without a gap.
    server.inject("b", &accepted(2, Some("b")));
    server.inject("b", &accepted(3, Some("a")));
    let event = wait_for(&mut b_start.events, |e| {
        is_remote(e) || matches!(e, SessionEvent::SequenceGap { .. })
    })
    .await;
    assert!(matches!(
        event,
        SessionEvent::RemoteOperation { sequence: 3, .. }
    ));
}

#[tokio::test]
async fn malformed_messages_are_ignored() {
    let server = TestServer::start().await;
    let (a, _a_start, _b, _b_start) = pair(&server).await;

    server.inject_raw("a", "{not json");
    server.inject_raw("a", r#"{"type":"session.teleport"}"#);
    server.inject_raw("a", r#"{"type":"operation.accepted","sequence":"one"}"#);

    let outcome = a.submit_operation(pause(), 0).await.expect("still works");
    assert!(matches!(
        outcome,
        SubmitOutcome::Applied { sequence: 1, .. }
    ));
}

#[tokio::test]
async fn oversized_messages_are_refused_locally() {
    let server = TestServer::start().await;
    let mut small = config(&server, "a");
    small.max_message_bytes = 2_048;
    let (a, _start) = SessionClient::create(small, music(&["s1"]))
        .await
        .expect("create");

    let huge = Op::QueueReplace {
        entries: (0..50).map(|i| entry(&format!("e{i}"))).collect(),
        current_entry_id: None,
        playback: None,
    };
    assert!(matches!(
        a.submit_operation(huge, 0).await,
        Err(Error::MessageTooLarge { max: 2_048, .. })
    ));
    // The connection is still usable.
    assert!(a.submit_operation(pause(), 0).await.is_ok());
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_rejoins_with_the_same_identity_and_resumes() {
    let server = TestServer::start().await;
    let (a, mut a_start, _b, _b_start) = pair(&server).await;
    let session_id = a_start.credentials.code.clone();

    server.kill_connections();
    wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Reconnecting { .. })
    })
    .await;
    let rejoined = wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Rejoined { .. })
    })
    .await;
    let SessionEvent::Rejoined { snapshot } = rejoined else {
        unreachable!()
    };
    assert_eq!(snapshot.session_id, session_id);

    assert_eq!(
        server.participant_count(&session_id),
        2,
        "rejoined, not a new seat"
    );
    let outcome = a
        .submit_operation(pause(), snapshot.revision)
        .await
        .expect("submit");
    assert!(matches!(outcome, SubmitOutcome::Applied { .. }));
}

#[tokio::test]
async fn requests_fail_fast_while_disconnected() {
    let server = TestServer::start().await;
    let (a, mut a_start) = create(&server, "a", &["s1"]).await;

    server.stop();
    wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Reconnecting { .. })
    })
    .await;
    let started = tokio::time::Instant::now();
    assert!(matches!(
        a.submit_operation(pause(), 0).await,
        Err(Error::TransportClosed)
    ));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "nothing is queued offline"
    );
}

#[tokio::test]
async fn forgotten_session_ends_the_client() {
    let server = TestServer::start().await;
    let (a, mut a_start) = create(&server, "a", &["s1"]).await;

    server.forget_sessions();
    server.kill_connections();
    let closed = wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Closed { .. })
    })
    .await;
    assert!(matches!(
        closed,
        SessionEvent::Closed {
            reason: CloseReason::SessionEnded
        }
    ));
    assert!(matches!(
        a.submit_operation(pause(), 0).await,
        Err(Error::SessionClosed)
    ));
    assert!(a.leave().await.is_ok(), "leaving an ended session is fine");
}

#[tokio::test]
async fn leave_confirms_and_updates_the_others() {
    let server = TestServer::start().await;
    let (_a, mut a_start, b, mut b_start) = pair(&server).await;
    wait_for(
        &mut a_start.events,
        |e| matches!(e, SessionEvent::Participants(p) if p.len() == 2),
    )
    .await;

    b.leave().await.expect("leave");
    wait_for(
        &mut a_start.events,
        |e| matches!(e, SessionEvent::Participants(p) if p.len() == 1),
    )
    .await;
    let closed = wait_for(&mut b_start.events, |e| {
        matches!(e, SessionEvent::Closed { .. })
    })
    .await;
    assert!(matches!(
        closed,
        SessionEvent::Closed {
            reason: CloseReason::Left
        }
    ));
}

#[tokio::test]
async fn leave_stops_reconnect_attempts() {
    let server = TestServer::start().await;
    let (a, mut a_start) = create(&server, "a", &["s1"]).await;

    server.stop();
    wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Reconnecting { .. })
    })
    .await;
    timeout(Duration::from_secs(1), a.leave())
        .await
        .expect("leave returns promptly")
        .expect("leave");

    wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Closed { .. })
    })
    .await;
    assert_quiet(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Reconnecting { .. })
    })
    .await;
}

#[tokio::test]
async fn close_keeps_the_seat_but_marks_it_disconnected() {
    let server = TestServer::start().await;
    let (_a, mut a_start, b, _b_start) = pair(&server).await;

    b.close().await;
    let event = wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Participants(p) if p.iter().any(|p| p.client_id == "b" && !p.connected))
    })
    .await;
    let SessionEvent::Participants(participants) = event else {
        unreachable!()
    };
    assert_eq!(participants.len(), 2);
}

#[tokio::test]
async fn status_reports_reach_the_other_participants() {
    let server = TestServer::start().await;
    let (_a, mut a_start, b, _b_start) = pair(&server).await;

    b.set_status(ParticipantStatus::WaitingForNextSong)
        .await
        .expect("set status");
    wait_for(&mut a_start.events, |e| {
        matches!(e, SessionEvent::Participants(p)
            if p.iter().any(|p| p.client_id == "b" && p.status == ParticipantStatus::WaitingForNextSong))
    })
    .await;
}

// ---------------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------------

/// Both clients follow the session by mirroring: their own operations at
/// the ack's server time, everyone else's from `RemoteOperation`. Both
/// mirrors must end up equal to the server's state.
#[tokio::test]
async fn two_clients_mirror_the_server_state() {
    let server = TestServer::start().await;
    let (a, a_start, b, b_start) = pair(&server).await;
    let session_id = a_start.credentials.code.clone();

    struct Follower {
        client: SessionClient,
        events: broadcast::Receiver<SessionEvent>,
        mirror: SessionMusicState,
        applied: u64,
        name: &'static str,
    }
    impl Follower {
        /// Apply every remote operation that has arrived so far.
        fn drain(&mut self) {
            while let Ok(event) = self.events.try_recv() {
                if let SessionEvent::RemoteOperation {
                    sequence,
                    operation,
                    accepted_at_ms,
                    ..
                } = event
                {
                    assert_eq!(sequence, self.applied + 1, "in order, no gaps");
                    self.mirror =
                        state::apply(&self.mirror, &operation, accepted_at_ms).expect("remote op");
                    self.applied = sequence;
                }
            }
        }

        async fn submit(&mut self, operation: Op) {
            self.drain();
            // As the runtime does: who queued what travels in the operation.
            let operation = state::stamp_added_by(&self.mirror, operation, Some(self.name));
            match self
                .client
                .submit_operation(operation.clone(), self.applied)
                .await
                .expect("submit")
            {
                SubmitOutcome::Applied {
                    sequence,
                    accepted_at_ms,
                } => {
                    self.mirror =
                        state::apply(&self.mirror, &operation, accepted_at_ms).expect("own op");
                    self.applied = sequence;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    let mut followers = [
        Follower {
            client: a,
            events: a_start.events,
            mirror: a_start.snapshot.state,
            applied: 0,
            name: "User a",
        },
        Follower {
            client: b,
            events: b_start.events,
            mirror: b_start.snapshot.state,
            applied: 0,
            name: "User b",
        },
    ];

    let script: Vec<(usize, Op)> = vec![
        (
            0,
            Op::QueueInsert {
                entry: entry("n1"),
                index: 1,
            },
        ),
        (1, pause()),
        (
            1,
            Op::QueueMove {
                entry_id: "s3".to_owned(),
                to_index: 1,
            },
        ),
        (
            0,
            Op::PlaybackAdvance {
                target_entry_id: "s3".to_owned(),
                position_ms: 0,
                is_playing: true,
            },
        ),
        (
            1,
            Op::PlaybackSeek {
                entry_id: "s3".to_owned(),
                position_ms: 30_000,
            },
        ),
    ];
    for (index, operation) in script {
        // Let the previous broadcast reach the other client first.
        tokio::time::sleep(Duration::from_millis(50)).await;
        followers[index].submit(operation).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    for follower in &mut followers {
        follower.drain();
    }

    let server_state = server.session_state(&session_id);
    assert_eq!(server.sequence(&session_id), 5);
    for follower in &followers {
        assert_eq!(follower.applied, 5);
        assert_eq!(follower.mirror, server_state);
    }
}
