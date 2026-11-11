//! WebSocket JSON messages (one message per text frame).
//!
//! Identity (`protocolVersion`, `clientId`, display name) is sent only in the
//! handshake (`session.create` / `session.join`). The server binds the
//! connection to that identity, so later messages carry no identity fields.
//! Keep-alive uses WebSocket Ping/Pong frames, not application messages.

use serde::{Deserialize, Serialize};

use crate::model::{SessionMusicState, SessionOperation, SessionSnapshot};

/// Longest accepted display name, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 32;
/// Longest accepted provider label, in characters.
pub const MAX_PROVIDER_LABEL_CHARS: usize = 32;
/// Longest accepted client, operation or request ID, in bytes.
pub const MAX_ID_LEN: usize = 64;

// ---------------------------------------------------------------------------
// Client -> server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    #[serde(rename = "session.create")]
    Create(CreateSession),
    #[serde(rename = "session.join")]
    Join(JoinSession),
    #[serde(rename = "session.operation")]
    Operation(OperationRequest),
    #[serde(rename = "participant.setStatus")]
    SetStatus { status: ParticipantStatus },
    /// The client's *complete* set of queue entries its provider could not
    /// find. Replaces its previous report, so resending is always safe.
    #[serde(rename = "participant.setUnavailable")]
    SetUnavailable { entry_ids: Vec<String> },
    #[serde(rename = "session.snapshot.request")]
    SnapshotRequest { request_id: String },
    #[serde(rename = "session.leave")]
    Leave,
}

/// Who is connecting. Shared by create and join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    pub protocol_version: u16,
    /// Random UUID per running app, stable across reconnects.
    pub client_id: String,
    pub display_name: String,
    /// Display-only label such as `"tidal"` or `"spotify"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSession {
    #[serde(flatten)]
    pub hello: ClientHello,
    pub initial_state: SessionMusicState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinSession {
    #[serde(flatten)]
    pub hello: ClientHello,
    pub session_id: String,
    /// Bearer secret from the invite. Never log or broadcast it.
    pub join_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRequest {
    /// Unique per submission; echoed back in the ack or rejection.
    pub operation_id: String,
    /// Last session sequence this client had received when it sent the operation.
    pub base_sequence: u64,
    pub operation: SessionOperation,
}

// ---------------------------------------------------------------------------
// Participants
// ---------------------------------------------------------------------------

/// Sync state reported by each client about itself. Display-only for the
/// server; it never changes how operations are accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParticipantStatus {
    /// Following the session.
    Synced,
    /// Applying a snapshot (joining, reconnecting, returning).
    Resyncing,
    /// Could not play the current song; rejoins automatically when the
    /// session moves to another song.
    WaitingForNextSong,
    /// Chose to step out; returns only when the user resyncs.
    Detached,
    /// Unexpected bridge failure; needs a manual resync.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub client_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub status: ParticipantStatus,
    /// Maintained by the server: false while the socket is gone (grace period).
    pub connected: bool,
    /// Entries still in the queue that this participant's provider could not
    /// find, as last reported. Empty until the participant reports.
    #[serde(default)]
    pub unavailable_entry_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Server -> client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    #[serde(rename = "session.created")]
    Created {
        session_id: String,
        /// Only ever sent to the creator.
        join_token: String,
        snapshot: SessionSnapshot,
    },
    #[serde(rename = "session.joined")]
    Joined { snapshot: SessionSnapshot },
    /// Sent only to the operation's sender.
    ///
    /// `applied == true`: `sequence` is the newly assigned sequence.
    /// `applied == false`: the operation changed nothing (e.g. everyone
    /// advancing to the same song) and `sequence` is the unchanged current one.
    ///
    /// `accepted_at_ms` is the server time the operation was evaluated at; the
    /// sender mirrors its own operation at that time so its state matches the
    /// server exactly.
    #[serde(rename = "operation.ack")]
    OperationAck {
        operation_id: String,
        sequence: u64,
        applied: bool,
        accepted_at_ms: u64,
    },
    /// Sent to every participant except the sender. Operations the server
    /// issues itself (e.g. skipping a song nobody can play) have no origin
    /// client and go to everyone.
    #[serde(rename = "operation.accepted")]
    OperationAccepted {
        sequence: u64,
        origin_client_id: Option<String>,
        operation: SessionOperation,
        /// Server clock; receivers use it as the playback capture time.
        accepted_at_ms: u64,
    },
    #[serde(rename = "operation.rejected")]
    OperationRejected {
        operation_id: String,
        code: RejectCode,
        /// Human-readable context; never use it for control flow.
        message: String,
        current_sequence: u64,
    },
    #[serde(rename = "session.snapshot")]
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        snapshot: SessionSnapshot,
    },
    #[serde(rename = "session.participants")]
    Participants { participants: Vec<Participant> },
    #[serde(rename = "session.left")]
    Left,
    #[serde(rename = "session.error")]
    Error { code: ErrorCode, message: String },
}

/// Why an operation was not accepted. The session continues either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RejectCode {
    /// A `queue.replace` was based on a queue that has changed since.
    StaleQueue,
    /// The operation does not fit the current state (unknown entry, bad index...).
    InvalidOperation,
}

/// Connection-level errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    UnsupportedVersion,
    /// The frame was not a valid message.
    InvalidMessage,
    /// Unknown session or wrong token; deliberately not distinguished.
    InvalidCredentials,
    /// Session-scoped message before a successful create/join.
    NotJoined,
    /// create/join on a connection that is already in a session.
    AlreadyJoined,
    /// Display name, provider label or IDs are outside the allowed limits.
    InvalidIdentity,
    /// Initial state failed validation.
    InvalidState,
    SessionFull,
    ServerBusy,
}
