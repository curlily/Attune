mod common;

use attune_protocol::{
    ClientMessage, CreateSession, ErrorCode, INVITE_SECRET_LEN, JoinSession, OperationRequest,
    PROTOCOL_VERSION, Participant, ParticipantStatus, RejectCode, ReplacePlayback, ServerMessage,
    SessionInvite, SessionOperation, SessionSnapshot, is_invite_secret, normalize_join_code,
};
use common::{entry, hello, state};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn assert_camel_case_keys(value: &Value, path: &str) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                assert!(!key.contains('_'), "non-camelCase key {path}.{key}");
                assert_camel_case_keys(child, &format!("{path}.{key}"));
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| assert_camel_case_keys(item, path)),
        _ => {}
    }
}

/// Serialize, check the tag and key casing, deserialize, compare.
fn round_trip<T>(value: &T, expected_type: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).expect("serialize");
    assert_eq!(json["type"], expected_type, "tag of {json}");
    assert_camel_case_keys(&json, "$");
    let back: T = serde_json::from_value(json).expect("deserialize");
    assert_eq!(&back, value);
}

fn all_operations() -> Vec<(SessionOperation, &'static str)> {
    vec![
        (
            SessionOperation::QueueReplace {
                entries: vec![entry("a"), entry("b")],
                current_entry_id: Some("a".to_owned()),
                playback: None,
            },
            "queue.replace",
        ),
        (
            SessionOperation::QueueInsert {
                entry: entry("c"),
                index: 1,
            },
            "queue.insert",
        ),
        (
            SessionOperation::QueueRemove {
                entry_id: "a".to_owned(),
            },
            "queue.remove",
        ),
        (
            SessionOperation::QueueMove {
                entry_id: "b".to_owned(),
                to_index: 0,
            },
            "queue.move",
        ),
        (
            SessionOperation::PlaybackSelect {
                entry_id: "a".to_owned(),
                position_ms: 10,
                is_playing: true,
            },
            "playback.select",
        ),
        (
            SessionOperation::PlaybackAdvance {
                target_entry_id: "b".to_owned(),
                position_ms: 0,
                is_playing: true,
            },
            "playback.advance",
        ),
        (
            SessionOperation::PlaybackSeek {
                entry_id: "a".to_owned(),
                position_ms: 5_000,
            },
            "playback.seek",
        ),
        (
            SessionOperation::PlaybackSetPlaying { is_playing: false },
            "playback.setPlaying",
        ),
    ]
}

fn snapshot() -> SessionSnapshot {
    SessionSnapshot {
        protocol_version: 1,
        session_id: "s1".to_owned(),
        revision: 3,
        updated_at_ms: 1_000,
        state: state(&["a", "b"], true, 500, 1_000),
    }
}

#[test]
fn every_operation_round_trips_with_stable_tag() {
    for (operation, tag) in all_operations() {
        round_trip(&operation, tag);
    }
}

#[test]
fn operation_wire_format_is_pinned() {
    let operation = SessionOperation::PlaybackAdvance {
        target_entry_id: "entry-1".to_owned(),
        position_ms: 2500,
        is_playing: true,
    };
    assert_eq!(
        serde_json::to_value(operation).expect("serialize"),
        json!({"type": "playback.advance", "targetEntryId": "entry-1", "positionMs": 2500, "isPlaying": true})
    );
}

#[test]
fn every_client_message_round_trips() {
    let messages = vec![
        (
            ClientMessage::Create(CreateSession {
                hello: hello("a"),
                initial_state: state(&["x"], false, 0, 0),
            }),
            "session.create",
        ),
        (
            ClientMessage::Join(JoinSession {
                hello: hello("b"),
                session_id: "KWPZ".to_owned(),
                rejoin_secret: Some("secret".to_owned()),
            }),
            "session.join",
        ),
        (
            ClientMessage::Operation(OperationRequest {
                operation_id: "op-1".to_owned(),
                base_sequence: 4,
                operation: SessionOperation::PlaybackSetPlaying { is_playing: true },
            }),
            "session.operation",
        ),
        (
            ClientMessage::SetStatus {
                status: ParticipantStatus::WaitingForNextSong,
            },
            "participant.setStatus",
        ),
        (
            ClientMessage::SetUnavailable {
                entry_ids: vec!["e1".to_owned(), "e2".to_owned()],
            },
            "participant.setUnavailable",
        ),
        (
            ClientMessage::SnapshotRequest {
                request_id: "r1".to_owned(),
            },
            "session.snapshot.request",
        ),
        (ClientMessage::Leave, "session.leave"),
        (
            ClientMessage::TimePing {
                client_time_ms: 1_234,
            },
            "time.ping",
        ),
    ];
    for (message, tag) in messages {
        round_trip(&message, tag);
    }
}

#[test]
fn every_server_message_round_trips() {
    let messages = vec![
        (
            ServerMessage::Created {
                session_id: "s1".to_owned(),
                rejoin_secret: "secret".to_owned(),
                snapshot: snapshot(),
                invite: Some(SessionInvite {
                    secret: "a".repeat(INVITE_SECRET_LEN),
                    party_id: "p1".to_owned(),
                    max_participants: 16,
                }),
            },
            "session.created",
        ),
        (
            ServerMessage::Joined {
                snapshot: snapshot(),
                rejoin_secret: "secret".to_owned(),
                invite: None,
            },
            "session.joined",
        ),
        (
            ServerMessage::OperationAck {
                operation_id: "op-1".to_owned(),
                sequence: 4,
                applied: true,
                accepted_at_ms: 9,
            },
            "operation.ack",
        ),
        (
            ServerMessage::OperationAccepted {
                sequence: 4,
                origin_client_id: Some("a".to_owned()),
                operation: SessionOperation::PlaybackSetPlaying { is_playing: false },
                accepted_at_ms: 9,
            },
            "operation.accepted",
        ),
        (
            ServerMessage::OperationRejected {
                operation_id: "op-2".to_owned(),
                code: RejectCode::StaleQueue,
                message: "stale".to_owned(),
                current_sequence: 4,
            },
            "operation.rejected",
        ),
        (
            ServerMessage::Snapshot {
                request_id: Some("r1".to_owned()),
                snapshot: snapshot(),
            },
            "session.snapshot",
        ),
        (
            ServerMessage::Participants {
                participants: vec![Participant {
                    client_id: "a".to_owned(),
                    display_name: "Lily".to_owned(),
                    provider: Some("spotify".to_owned()),
                    status: ParticipantStatus::Detached,
                    connected: false,
                    unavailable_entry_ids: vec!["e1".to_owned()],
                }],
            },
            "session.participants",
        ),
        (ServerMessage::Left, "session.left"),
        (
            ServerMessage::Error {
                code: ErrorCode::InvalidCredentials,
                message: "no".to_owned(),
            },
            "session.error",
        ),
        (
            ServerMessage::TimePong {
                client_time_ms: 1_234,
                server_time_ms: 5_678,
            },
            "time.pong",
        ),
    ];
    for (message, tag) in messages {
        round_trip(&message, tag);
    }
}

#[test]
fn handshake_identity_is_flattened_into_the_message() {
    let message = ClientMessage::Join(JoinSession {
        hello: hello("b"),
        session_id: "KWPZ".to_owned(),
        rejoin_secret: None,
    });
    assert_eq!(
        serde_json::to_value(message).expect("serialize"),
        json!({
            "type": "session.join",
            "protocolVersion": PROTOCOL_VERSION,
            "clientId": "b",
            "displayName": "User b",
            "provider": "tidal",
            "sessionId": "KWPZ",
        })
    );
}

#[test]
fn enum_values_use_camel_case_strings() {
    assert_eq!(
        json!(ParticipantStatus::WaitingForNextSong),
        json!("waitingForNextSong")
    );
    assert_eq!(json!(RejectCode::StaleQueue), json!("staleQueue"));
    assert_eq!(
        json!(ErrorCode::InvalidCredentials),
        json!("invalidCredentials")
    );
}

#[test]
fn unknown_message_and_operation_types_are_rejected() {
    let unknown_message = json!({"type": "session.teleport"});
    assert!(serde_json::from_value::<ClientMessage>(unknown_message).is_err());

    let unknown_operation = json!({"type": "playback.toggle"});
    assert!(serde_json::from_value::<SessionOperation>(unknown_operation).is_err());
}

#[test]
fn snapshot_has_no_client_origin_and_uses_revision() {
    let json = serde_json::to_value(snapshot()).expect("serialize");
    assert_eq!(json["revision"], 3);
    assert!(json.get("originClientId").is_none());
    assert_eq!(json["state"]["currentEntryId"], "a");
    assert_eq!(json["state"]["playback"]["positionCapturedAtMs"], 1_000);
}

#[test]
fn a_replace_carries_the_new_songs_playback_only_when_given() {
    let with = SessionOperation::QueueReplace {
        entries: vec![entry("e1")],
        current_entry_id: Some("e1".to_owned()),
        playback: Some(ReplacePlayback {
            position_ms: 48_000,
            is_playing: false,
        }),
    };
    let value = serde_json::to_value(&with).expect("serialize");
    assert_eq!(
        value["playback"],
        json!({ "positionMs": 48_000, "isPlaying": false })
    );
    assert_eq!(
        serde_json::from_value::<SessionOperation>(value).expect("deserialize"),
        with
    );

    // Without it (as protocol 1 sent it) the field is absent and reads back as None.
    let without = json!({
        "type": "queue.replace",
        "entries": [serde_json::to_value(entry("e1")).unwrap()],
        "currentEntryId": "e1",
    });
    let parsed: SessionOperation = serde_json::from_value(without.clone()).expect("deserialize");
    assert!(matches!(
        parsed,
        SessionOperation::QueueReplace { playback: None, .. }
    ));
    assert_eq!(serde_json::to_value(parsed).unwrap(), without);
}

#[test]
fn invite_secrets_are_never_join_codes() {
    let secret = "0123456789abcdef".repeat(4);
    assert!(is_invite_secret(&secret));
    assert!(normalize_join_code(&secret).is_none());
    for not_secret in [
        "KWPZ",
        &secret[..63],
        &secret.to_uppercase(),
        &format!("{}g", &secret[..63]),
    ] {
        assert!(!is_invite_secret(not_secret), "{not_secret}");
    }
}

#[test]
fn invite_debug_hides_the_secret() {
    let invite = SessionInvite {
        secret: "b".repeat(INVITE_SECRET_LEN),
        party_id: "party".to_owned(),
        max_participants: 16,
    };
    assert!(!format!("{invite:?}").contains("bbbb"));
}

#[test]
fn replies_without_an_invite_still_parse() {
    // From a server that predates invites.
    let json = r#"{"type":"session.joined","snapshot":SNAPSHOT,"rejoinSecret":"s"}"#
        .replace("SNAPSHOT", &serde_json::to_string(&snapshot()).unwrap());
    let ServerMessage::Joined { invite, .. } = serde_json::from_str(&json).unwrap() else {
        panic!("expected session.joined");
    };
    assert_eq!(invite, None);
}
