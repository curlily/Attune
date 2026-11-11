mod common;

use common::{entry, hello, state};
use jam_protocol::{
    ClientMessage, CreateSession, ErrorCode, JoinSession, OperationRequest, Participant,
    ParticipantStatus, RejectCode, ServerMessage, SessionOperation, SessionSnapshot,
};
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
                session_id: "s1".to_owned(),
                join_token: "secret".to_owned(),
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
            ClientMessage::SnapshotRequest {
                request_id: "r1".to_owned(),
            },
            "session.snapshot.request",
        ),
        (ClientMessage::Leave, "session.leave"),
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
                join_token: "secret".to_owned(),
                snapshot: snapshot(),
            },
            "session.created",
        ),
        (
            ServerMessage::Joined {
                snapshot: snapshot(),
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
    ];
    for (message, tag) in messages {
        round_trip(&message, tag);
    }
}

#[test]
fn handshake_identity_is_flattened_into_the_message() {
    let message = ClientMessage::Join(JoinSession {
        hello: hello("b"),
        session_id: "s1".to_owned(),
        join_token: "secret".to_owned(),
    });
    assert_eq!(
        serde_json::to_value(message).expect("serialize"),
        json!({
            "type": "session.join",
            "protocolVersion": 1,
            "clientId": "b",
            "displayName": "User b",
            "provider": "tidal",
            "sessionId": "s1",
            "joinToken": "secret",
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
