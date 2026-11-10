mod common;

use common::{entry, hello, ids, state};
use jam_protocol::{
    ClientMessage, CreateSession, ErrorCode, JoinSession, OperationRequest, ParticipantStatus,
    RejectCode, ServerMessage, SessionMusicState, SessionOperation as Op,
    coordinator::{Outbound, Recipient, SessionCoordinator},
    state,
};

const SESSION: &str = "session-1";
const TOKEN: &str = "token-abcdef";
const T0: u64 = 1_000;

fn create_with(initial: SessionMusicState) -> (SessionCoordinator, Vec<Outbound>) {
    SessionCoordinator::create(
        SESSION.to_owned(),
        TOKEN.to_owned(),
        CreateSession {
            hello: hello("a"),
            initial_state: initial,
        },
        8,
        T0,
    )
    .expect("create session")
}

fn create() -> SessionCoordinator {
    create_with(state(&["s1", "s2", "s3"], true, 0, 0)).0
}

fn join_request(client: &str, token: &str) -> JoinSession {
    JoinSession {
        hello: hello(client),
        session_id: SESSION.to_owned(),
        join_token: token.to_owned(),
    }
}

fn join(session: &mut SessionCoordinator, client: &str) -> Vec<Outbound> {
    session.join(join_request(client, TOKEN), T0).expect("join")
}

fn op(operation_id: &str, base_sequence: u64, operation: Op) -> ClientMessage {
    ClientMessage::Operation(OperationRequest {
        operation_id: operation_id.to_owned(),
        base_sequence,
        operation,
    })
}

fn pause() -> Op {
    Op::PlaybackSetPlaying { is_playing: false }
}

fn only_to<'a>(out: &'a [Outbound], client: &str) -> &'a ServerMessage {
    assert_eq!(out.len(), 1, "expected one message, got {out:?}");
    assert_eq!(out[0].to, Recipient::Client(client.to_owned()));
    &out[0].message
}

fn participants(out: &[Outbound]) -> Vec<(String, ParticipantStatus, bool)> {
    out.iter()
        .find_map(|o| match &o.message {
            ServerMessage::Participants { participants } => {
                assert_eq!(o.to, Recipient::All);
                Some(
                    participants
                        .iter()
                        .map(|p| (p.client_id.clone(), p.status, p.connected))
                        .collect(),
                )
            }
            _ => None,
        })
        .expect("participants update")
}

// ---------------------------------------------------------------------------
// Create / join
// ---------------------------------------------------------------------------

#[test]
fn create_returns_invite_and_snapshot_at_sequence_zero() {
    let (session, out) = create_with(state(&["s1"], true, 4_000, 123_456_789));

    match &out[0] {
        Outbound {
            to: Recipient::Client(client),
            message:
                ServerMessage::Created {
                    session_id,
                    join_token,
                    snapshot,
                },
        } => {
            assert_eq!(client, "a");
            assert_eq!(session_id, SESSION);
            assert_eq!(join_token, TOKEN);
            assert_eq!(snapshot.revision, 0);
            assert_eq!(snapshot.protocol_version, 1);
            // Position kept, capture time restamped with the server clock.
            assert_eq!(snapshot.state.playback.position_ms, 4_000);
            assert_eq!(snapshot.state.playback.position_captured_at_ms, T0);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        participants(&out),
        [("a".to_owned(), ParticipantStatus::Synced, true)]
    );
    assert_eq!(session.sequence(), 0);
}

#[test]
fn create_rejects_bad_version_identity_and_state() {
    let attempt = |hello, initial_state| {
        SessionCoordinator::create(
            SESSION.to_owned(),
            TOKEN.to_owned(),
            CreateSession {
                hello,
                initial_state,
            },
            8,
            T0,
        )
        .map(|_| ())
        .map_err(|error| error.code)
    };

    let mut old = hello("a");
    old.protocol_version = 2;
    assert_eq!(
        attempt(old, state(&[], false, 0, 0)),
        Err(ErrorCode::UnsupportedVersion)
    );

    let mut nameless = hello("a");
    nameless.display_name = "   ".to_owned();
    assert_eq!(
        attempt(nameless, state(&[], false, 0, 0)),
        Err(ErrorCode::InvalidIdentity)
    );

    let mut long_name = hello("a");
    long_name.display_name = "x".repeat(33);
    assert_eq!(
        attempt(long_name, state(&[], false, 0, 0)),
        Err(ErrorCode::InvalidIdentity)
    );

    let mut bad_state = state(&["s1"], false, 0, 0);
    bad_state.current_entry_id = None;
    assert_eq!(attempt(hello("a"), bad_state), Err(ErrorCode::InvalidState));
}

#[test]
fn display_name_is_trimmed_and_empty_provider_dropped() {
    let mut h = hello("a");
    h.display_name = "  Lily  ".to_owned();
    h.provider = Some(" ".to_owned());
    let (session, _) = SessionCoordinator::create(
        SESSION.to_owned(),
        TOKEN.to_owned(),
        CreateSession {
            hello: h,
            initial_state: state(&[], false, 0, 0),
        },
        8,
        T0,
    )
    .expect("create");
    assert_eq!(session.participants()[0].display_name, "Lily");
    assert_eq!(session.participants()[0].provider, None);
}

#[test]
fn join_requires_matching_session_and_token() {
    let mut session = create();
    let error = session
        .join(join_request("b", "wrong"), T0)
        .expect_err("wrong token");
    assert_eq!(error.code, ErrorCode::InvalidCredentials);

    let mut other_session = join_request("b", TOKEN);
    other_session.session_id = "session-2".to_owned();
    let error = session.join(other_session, T0).expect_err("wrong session");
    assert_eq!(error.code, ErrorCode::InvalidCredentials);
    assert!(!error.message.contains(TOKEN));

    assert_eq!(session.participants().len(), 1);
}

#[test]
fn join_returns_snapshot_and_announces_participant() {
    let mut session = create();
    let out = join(&mut session, "b");

    match &out[0] {
        Outbound {
            to: Recipient::Client(client),
            message: ServerMessage::Joined { snapshot },
        } => {
            assert_eq!(client, "b");
            assert_eq!(snapshot.session_id, SESSION);
            assert_eq!(ids(&snapshot.state), ["s1", "s2", "s3"]);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        participants(&out),
        [
            ("a".to_owned(), ParticipantStatus::Synced, true),
            ("b".to_owned(), ParticipantStatus::Resyncing, true),
        ]
    );
}

#[test]
fn rejoin_reuses_participant_and_keeps_detached() {
    let mut session = create();
    join(&mut session, "b");
    session.handle(
        "b",
        ClientMessage::SetStatus {
            status: ParticipantStatus::Detached,
        },
        T0,
    );
    session.set_connected("b", false);

    let out = join(&mut session, "b");
    assert_eq!(
        participants(&out)[1],
        ("b".to_owned(), ParticipantStatus::Detached, true)
    );
    assert_eq!(session.participants().len(), 2);
}

#[test]
fn session_full_rejects_new_members_but_not_rejoins() {
    let (mut session, _) = SessionCoordinator::create(
        SESSION.to_owned(),
        TOKEN.to_owned(),
        CreateSession {
            hello: hello("a"),
            initial_state: state(&[], false, 0, 0),
        },
        2,
        T0,
    )
    .expect("create");
    join(&mut session, "b");
    let error = session
        .join(join_request("c", TOKEN), T0)
        .expect_err("full");
    assert_eq!(error.code, ErrorCode::SessionFull);
    assert!(session.join(join_request("b", TOKEN), T0).is_ok());
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

#[test]
fn accepted_operation_is_acked_to_sender_and_broadcast_to_others() {
    let mut session = create();
    join(&mut session, "b");

    let out = session.handle("a", op("op-1", 0, pause()), T0 + 500);
    assert_eq!(
        out,
        [
            Outbound {
                to: Recipient::Client("a".to_owned()),
                message: ServerMessage::OperationAck {
                    operation_id: "op-1".to_owned(),
                    sequence: 1,
                    applied: true,
                    accepted_at_ms: T0 + 500,
                },
            },
            Outbound {
                to: Recipient::AllExcept("a".to_owned()),
                message: ServerMessage::OperationAccepted {
                    sequence: 1,
                    origin_client_id: "a".to_owned(),
                    operation: pause(),
                    accepted_at_ms: T0 + 500,
                },
            },
        ]
    );
    assert_eq!(session.sequence(), 1);
    assert_eq!(session.snapshot(T0 + 500).revision, 1);
    assert!(!session.state().playback.is_playing);
}

#[test]
fn noop_operation_is_acked_without_sequence_or_broadcast() {
    let mut session = create();
    let out = session.handle(
        "a",
        op("op-1", 0, Op::PlaybackSetPlaying { is_playing: true }),
        T0,
    );
    assert_eq!(
        only_to(&out, "a"),
        &ServerMessage::OperationAck {
            operation_id: "op-1".to_owned(),
            sequence: 0,
            applied: false,
            accepted_at_ms: T0,
        }
    );
    assert_eq!(session.sequence(), 0);
}

#[test]
fn simultaneous_song_end_advances_converge() {
    let mut session = create();
    join(&mut session, "b");
    join(&mut session, "c");
    let advance = Op::PlaybackAdvance {
        target_entry_id: "s2".to_owned(),
        position_ms: 0,
        is_playing: true,
    };

    let first = session.handle("a", op("a-1", 0, advance.clone()), T0);
    assert_eq!(first.len(), 2, "first advance is accepted and broadcast");

    for client in ["b", "c"] {
        let out = session.handle(client, op("late", 0, advance.clone()), T0 + 300);
        assert!(
            matches!(
                only_to(&out, client),
                ServerMessage::OperationAck {
                    applied: false,
                    sequence: 1,
                    ..
                }
            ),
            "{client}: {out:?}"
        );
    }
    assert_eq!(session.sequence(), 1);
}

#[test]
fn stale_queue_replace_is_rejected() {
    let mut session = create();
    join(&mut session, "b");
    session.handle(
        "b",
        op(
            "b-1",
            0,
            Op::QueueRemove {
                entry_id: "s3".to_owned(),
            },
        ),
        T0,
    );

    let replace = Op::QueueReplace {
        entries: vec![entry("s1"), entry("new")],
        current_entry_id: Some("s1".to_owned()),
    };
    let out = session.handle("a", op("a-1", 0, replace), T0);
    assert_eq!(
        only_to(&out, "a"),
        &ServerMessage::OperationRejected {
            operation_id: "a-1".to_owned(),
            code: RejectCode::StaleQueue,
            message: "the queue changed after this edit was made".to_owned(),
            current_sequence: 1,
        }
    );
    assert_eq!(ids(session.state()), ["s1", "s2"]);
}

#[test]
fn playback_changes_by_others_do_not_make_queue_edits_stale() {
    let mut session = create();
    join(&mut session, "b");
    session.handle("b", op("b-1", 0, pause()), T0);
    session.handle(
        "b",
        op(
            "b-2",
            1,
            Op::PlaybackSeek {
                entry_id: "s1".to_owned(),
                position_ms: 60_000,
            },
        ),
        T0,
    );

    let replace = Op::QueueReplace {
        entries: vec![entry("s1"), entry("new")],
        current_entry_id: Some("s1".to_owned()),
    };
    let out = session.handle("a", op("a-1", 0, replace), T0);
    assert!(matches!(
        out[0].message,
        ServerMessage::OperationAck {
            sequence: 3,
            applied: true,
            ..
        }
    ));
    assert_eq!(ids(session.state()), ["s1", "new"]);
    assert_eq!(
        session.state().playback.position_ms,
        60_000,
        "seek survived"
    );
}

#[test]
fn non_replace_queue_operations_are_validated_against_current_state() {
    let mut session = create();
    join(&mut session, "b");
    session.handle(
        "b",
        op(
            "b-1",
            0,
            Op::QueueRemove {
                entry_id: "s3".to_owned(),
            },
        ),
        T0,
    );

    // Based on sequence 0 but still valid: accepted.
    let out = session.handle(
        "a",
        op(
            "a-1",
            0,
            Op::QueueRemove {
                entry_id: "s2".to_owned(),
            },
        ),
        T0,
    );
    assert!(matches!(
        out[0].message,
        ServerMessage::OperationAck { applied: true, .. }
    ));

    // Refers to an entry that is gone: rejected as invalid, sequence unchanged.
    let out = session.handle(
        "a",
        op(
            "a-2",
            0,
            Op::QueueRemove {
                entry_id: "s3".to_owned(),
            },
        ),
        T0,
    );
    assert!(matches!(
        only_to(&out, "a"),
        ServerMessage::OperationRejected {
            code: RejectCode::InvalidOperation,
            current_sequence: 2,
            ..
        }
    ));
    assert_eq!(session.sequence(), 2);
}

#[test]
fn base_sequence_from_the_future_is_rejected() {
    let mut session = create();
    let out = session.handle("a", op("a-1", 5, pause()), T0);
    assert!(matches!(
        only_to(&out, "a"),
        ServerMessage::OperationRejected {
            code: RejectCode::InvalidOperation,
            ..
        }
    ));
}

#[test]
fn invalid_operation_id_is_an_error() {
    let mut session = create();
    let out = session.handle("a", op("", 0, pause()), T0);
    assert!(matches!(
        only_to(&out, "a"),
        ServerMessage::Error {
            code: ErrorCode::InvalidMessage,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Membership and status
// ---------------------------------------------------------------------------

#[test]
fn non_members_and_repeated_handshakes_are_refused() {
    let mut session = create();
    let out = session.handle("stranger", op("x", 0, pause()), T0);
    assert!(matches!(
        only_to(&out, "stranger"),
        ServerMessage::Error {
            code: ErrorCode::NotJoined,
            ..
        }
    ));

    let out = session.handle("a", ClientMessage::Join(join_request("a", TOKEN)), T0);
    assert!(matches!(
        only_to(&out, "a"),
        ServerMessage::Error {
            code: ErrorCode::AlreadyJoined,
            ..
        }
    ));
}

#[test]
fn status_changes_are_broadcast_once() {
    let mut session = create();
    let set = |status| ClientMessage::SetStatus { status };

    let out = session.handle("a", set(ParticipantStatus::WaitingForNextSong), T0);
    assert_eq!(
        participants(&out),
        [("a".to_owned(), ParticipantStatus::WaitingForNextSong, true)]
    );
    assert!(
        session
            .handle("a", set(ParticipantStatus::WaitingForNextSong), T0)
            .is_empty()
    );
}

#[test]
fn connection_changes_and_removal_update_participants() {
    let mut session = create();
    join(&mut session, "b");

    let out = session.set_connected("b", false);
    assert_eq!(
        participants(&out)[1],
        ("b".to_owned(), ParticipantStatus::Resyncing, false)
    );
    assert!(session.set_connected("b", false).is_empty());

    let out = session.remove("b");
    assert_eq!(participants(&out).len(), 1);
    assert!(session.remove("b").is_empty());
}

#[test]
fn leave_confirms_and_removes_participant() {
    let mut session = create();
    join(&mut session, "b");
    let out = session.handle("b", ClientMessage::Leave, T0);
    assert_eq!(
        out[0],
        Outbound {
            to: Recipient::Client("b".to_owned()),
            message: ServerMessage::Left,
        }
    );
    assert_eq!(
        participants(&out),
        [("a".to_owned(), ParticipantStatus::Synced, true)]
    );
    assert!(!session.is_member("b"));
}

#[test]
fn snapshot_request_returns_extrapolated_state() {
    let mut session = create();
    let out = session.handle(
        "a",
        ClientMessage::SnapshotRequest {
            request_id: "r1".to_owned(),
        },
        T0 + 2_000,
    );
    match only_to(&out, "a") {
        ServerMessage::Snapshot {
            request_id,
            snapshot,
        } => {
            assert_eq!(request_id.as_deref(), Some("r1"));
            assert_eq!(snapshot.state.playback.position_ms, 2_000);
            assert_eq!(snapshot.state.playback.position_captured_at_ms, T0 + 2_000);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Client mirrors
// ---------------------------------------------------------------------------

/// Two clients follow the session by applying accepted operations with the
/// shared transition function; their mirrors must equal the server state.
#[test]
fn client_mirrors_match_server_state() {
    let mut session = create();
    let out = join(&mut session, "b");
    let mut mirror_b = match &out[0].message {
        ServerMessage::Joined { snapshot } => snapshot.state.clone(),
        other => panic!("unexpected {other:?}"),
    };
    let mut mirror_a = session.snapshot(T0).state;
    let mut seq = [0_u64; 2];

    let script: Vec<(&str, Op)> = vec![
        (
            "a",
            Op::QueueInsert {
                entry: entry("n1"),
                index: 1,
            },
        ),
        ("b", pause()),
        (
            "b",
            Op::QueueMove {
                entry_id: "s3".to_owned(),
                to_index: 1,
            },
        ),
        (
            "a",
            Op::PlaybackAdvance {
                target_entry_id: "s3".to_owned(),
                position_ms: 0,
                is_playing: true,
            },
        ),
        (
            "a",
            Op::PlaybackSeek {
                entry_id: "s3".to_owned(),
                position_ms: 30_000,
            },
        ),
    ];

    for (step, (sender, operation)) in script.into_iter().enumerate() {
        let now = T0 + 1_000 * (step as u64 + 1);
        let sender_index = usize::from(sender == "b");
        let base = seq[sender_index];
        let out = session.handle(
            sender,
            op(&format!("{sender}-{step}"), base, operation.clone()),
            now,
        );

        for outbound in out {
            match (outbound.to, outbound.message) {
                (
                    Recipient::Client(to),
                    ServerMessage::OperationAck {
                        sequence,
                        applied: true,
                        accepted_at_ms,
                        ..
                    },
                ) => {
                    assert_eq!(to, sender);
                    // The sender already has the change locally; it mirrors it
                    // at the server's accept time instead of receiving it back.
                    let mirror = if sender == "a" {
                        &mut mirror_a
                    } else {
                        &mut mirror_b
                    };
                    *mirror =
                        state::apply(mirror, &operation, accepted_at_ms).expect("sender mirror");
                    seq[sender_index] = sequence;
                }
                (
                    Recipient::AllExcept(except),
                    ServerMessage::OperationAccepted {
                        sequence,
                        operation,
                        accepted_at_ms,
                        ..
                    },
                ) => {
                    assert_eq!(except, sender);
                    let (mirror, index) = if sender == "a" {
                        (&mut mirror_b, 1)
                    } else {
                        (&mut mirror_a, 0)
                    };
                    assert_eq!(sequence, seq[index] + 1, "no gap");
                    *mirror =
                        state::apply(mirror, &operation, accepted_at_ms).expect("receiver mirror");
                    seq[index] = sequence;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    assert_eq!(&mirror_a, session.state());
    assert_eq!(&mirror_b, session.state());
    assert_eq!(seq, [5, 5]);
    assert_eq!(ids(session.state()), ["s3", "n1", "s2"]);
}
