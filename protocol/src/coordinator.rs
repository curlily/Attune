//! Authoritative per-session logic, without I/O.
//!
//! The relay server owns one [`SessionCoordinator`] per session and feeds it
//! messages together with the current server time. The coordinator returns the
//! messages to send ([`Outbound`]); delivering them is the caller's job. This
//! keeps all sequencing and acceptance rules testable without sockets.
//!
//! # Acceptance rules
//!
//! 1. Operations that change nothing ([`state::is_noop`]) are acked with
//!    `applied: false` and get no sequence, e.g. every client reporting the
//!    same song-end advance.
//! 2. Only `queue.replace` is checked for staleness: it is rejected if the
//!    queue changed after its `baseSequence`. Pauses and seeks by others never
//!    invalidate it.
//! 3. Everything else is validated against the current state
//!    ([`state::apply`]); if it still makes sense it is accepted.
//!
//! Accepted operations are acked to the sender and broadcast to everyone
//! else, so the sender never receives its own operation back.

use crate::{
    PROTOCOL_VERSION,
    messages::{
        ClientHello, ClientMessage, CreateSession, ErrorCode, JoinSession, MAX_DISPLAY_NAME_CHARS,
        MAX_ID_LEN, MAX_PROVIDER_LABEL_CHARS, OperationRequest, Participant, ParticipantStatus,
        RejectCode, ServerMessage,
    },
    model::{SessionMusicState, SessionOperation, SessionSnapshot},
    state,
};

/// Who a message is for. `All` and `AllExcept` mean all *connected*
/// participants; the caller resolves them to sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    Client(String),
    AllExcept(String),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    pub to: Recipient,
    pub message: ServerMessage,
}

impl Outbound {
    fn to_client(client_id: &str, message: ServerMessage) -> Self {
        Self {
            to: Recipient::Client(client_id.to_owned()),
            message,
        }
    }
}

/// A failed `session.create` / `session.join`. The connection stays unbound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HandshakeError {
    pub code: ErrorCode,
    pub message: String,
}

impl HandshakeError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn into_message(self) -> ServerMessage {
        ServerMessage::Error {
            code: self.code,
            message: self.message,
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && !id.chars().any(char::is_control)
}

fn clean_label(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    let ok = !value.is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control);
    ok.then(|| value.to_owned())
}

/// Check the protocol version and normalize identity fields (trimmed names,
/// empty provider label becomes `None`).
pub fn validate_hello(hello: &ClientHello) -> Result<ClientHello, HandshakeError> {
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(HandshakeError::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "protocol version {} is not supported; this server speaks {PROTOCOL_VERSION}",
                hello.protocol_version
            ),
        ));
    }
    if !valid_id(&hello.client_id) {
        return Err(HandshakeError::new(
            ErrorCode::InvalidIdentity,
            "invalid client id",
        ));
    }
    let display_name =
        clean_label(&hello.display_name, MAX_DISPLAY_NAME_CHARS).ok_or_else(|| {
            HandshakeError::new(
                ErrorCode::InvalidIdentity,
                format!("display name must be 1-{MAX_DISPLAY_NAME_CHARS} characters"),
            )
        })?;
    let provider = match hello.provider.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(label) => Some(clean_label(label, MAX_PROVIDER_LABEL_CHARS).ok_or_else(|| {
            HandshakeError::new(ErrorCode::InvalidIdentity, "invalid provider label")
        })?),
    };
    Ok(ClientHello {
        protocol_version: hello.protocol_version,
        client_id: hello.client_id.clone(),
        display_name,
        provider,
    })
}

/// Compare secrets without an early exit on the first differing byte.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |acc, (l, r)| acc | (l ^ r))
            == 0
}

#[derive(Debug)]
pub struct SessionCoordinator {
    session_id: String,
    join_token: String,
    max_participants: usize,
    sequence: u64,
    /// Sequence of the last accepted operation that changed queue structure.
    last_queue_change_seq: u64,
    state: SessionMusicState,
    updated_at_ms: u64,
    /// In join order.
    participants: Vec<Participant>,
}

impl SessionCoordinator {
    /// Create a session. `session_id` and `join_token` are generated by the
    /// caller (the server owns the randomness).
    pub fn create(
        session_id: String,
        join_token: String,
        request: CreateSession,
        max_participants: usize,
        now_ms: u64,
    ) -> Result<(Self, Vec<Outbound>), HandshakeError> {
        let hello = validate_hello(&request.hello)?;
        state::validate_state(&request.initial_state)
            .map_err(|error| HandshakeError::new(ErrorCode::InvalidState, error.to_string()))?;

        // Keep the creator's reported position but restamp it with the server
        // clock; client clocks are not comparable with each other.
        let mut initial_state = request.initial_state;
        initial_state.playback.position_captured_at_ms = now_ms;

        let creator_id = hello.client_id.clone();
        let mut coordinator = Self {
            session_id,
            join_token,
            max_participants: max_participants.max(1),
            sequence: 0,
            last_queue_change_seq: 0,
            state: initial_state,
            updated_at_ms: now_ms,
            participants: Vec::new(),
        };
        coordinator
            .participants
            .push(participant(hello, ParticipantStatus::Synced));

        let outbound = vec![
            Outbound::to_client(
                &creator_id,
                ServerMessage::Created {
                    session_id: coordinator.session_id.clone(),
                    join_token: coordinator.join_token.clone(),
                    snapshot: coordinator.snapshot(now_ms),
                },
            ),
            coordinator.participants_update(),
        ];
        Ok((coordinator, outbound))
    }

    /// Join (or rejoin after a reconnect) with an invite.
    pub fn join(
        &mut self,
        request: JoinSession,
        now_ms: u64,
    ) -> Result<Vec<Outbound>, HandshakeError> {
        let hello = validate_hello(&request.hello)?;
        let credentials_ok =
            constant_time_eq(request.session_id.as_bytes(), self.session_id.as_bytes())
                & constant_time_eq(request.join_token.as_bytes(), self.join_token.as_bytes());
        if !credentials_ok {
            return Err(HandshakeError::new(
                ErrorCode::InvalidCredentials,
                "unknown session or invalid invite",
            ));
        }

        let client_id = hello.client_id.clone();
        match self.participant_mut(&client_id) {
            Some(existing) => {
                existing.display_name = hello.display_name;
                existing.provider = hello.provider;
                existing.connected = true;
                // A returning client that had stepped out stays stepped out
                // until it reports otherwise.
                if existing.status != ParticipantStatus::Detached {
                    existing.status = ParticipantStatus::Resyncing;
                }
            }
            None => {
                if self.participants.len() >= self.max_participants {
                    return Err(HandshakeError::new(
                        ErrorCode::SessionFull,
                        "session is full",
                    ));
                }
                self.participants
                    .push(participant(hello, ParticipantStatus::Resyncing));
            }
        }

        Ok(vec![
            Outbound::to_client(
                &client_id,
                ServerMessage::Joined {
                    snapshot: self.snapshot(now_ms),
                },
            ),
            self.participants_update(),
        ])
    }

    /// Handle a message from a connection already bound to `client_id`.
    pub fn handle(
        &mut self,
        client_id: &str,
        message: ClientMessage,
        now_ms: u64,
    ) -> Vec<Outbound> {
        if !self.is_member(client_id) {
            return vec![error_to(
                client_id,
                ErrorCode::NotJoined,
                "not a member of this session",
            )];
        }

        match message {
            ClientMessage::Create(_) | ClientMessage::Join(_) => {
                vec![error_to(
                    client_id,
                    ErrorCode::AlreadyJoined,
                    "connection is already in a session",
                )]
            }
            ClientMessage::Operation(request) => self.handle_operation(client_id, request, now_ms),
            ClientMessage::SetStatus { status } => {
                let changed = self
                    .participant_mut(client_id)
                    .is_some_and(|p| std::mem::replace(&mut p.status, status) != status);
                if changed {
                    vec![self.participants_update()]
                } else {
                    Vec::new()
                }
            }
            ClientMessage::SnapshotRequest { request_id } => {
                if !valid_id(&request_id) {
                    return vec![error_to(
                        client_id,
                        ErrorCode::InvalidMessage,
                        "invalid request id",
                    )];
                }
                vec![Outbound::to_client(
                    client_id,
                    ServerMessage::Snapshot {
                        request_id: Some(request_id),
                        snapshot: self.snapshot(now_ms),
                    },
                )]
            }
            ClientMessage::Leave => {
                self.participants.retain(|p| p.client_id != client_id);
                vec![
                    Outbound::to_client(client_id, ServerMessage::Left),
                    self.participants_update(),
                ]
            }
        }
    }

    fn handle_operation(
        &mut self,
        client_id: &str,
        request: OperationRequest,
        now_ms: u64,
    ) -> Vec<Outbound> {
        let OperationRequest {
            operation_id,
            base_sequence,
            operation,
        } = request;

        if !valid_id(&operation_id) {
            return vec![error_to(
                client_id,
                ErrorCode::InvalidMessage,
                "invalid operation id",
            )];
        }
        if base_sequence > self.sequence {
            return vec![self.reject(
                client_id,
                operation_id,
                RejectCode::InvalidOperation,
                "baseSequence is ahead of the session".to_owned(),
            )];
        }

        if state::is_noop(&self.state, &operation, now_ms) {
            return vec![Outbound::to_client(
                client_id,
                ServerMessage::OperationAck {
                    operation_id,
                    sequence: self.sequence,
                    applied: false,
                    accepted_at_ms: now_ms,
                },
            )];
        }

        if matches!(operation, SessionOperation::QueueReplace { .. })
            && base_sequence < self.last_queue_change_seq
        {
            return vec![self.reject(
                client_id,
                operation_id,
                RejectCode::StaleQueue,
                "the queue changed after this edit was made".to_owned(),
            )];
        }

        let next = match state::apply(&self.state, &operation, now_ms) {
            Ok(next) => next,
            Err(error) => {
                return vec![self.reject(
                    client_id,
                    operation_id,
                    RejectCode::InvalidOperation,
                    error.to_string(),
                )];
            }
        };

        self.sequence += 1;
        if operation.changes_queue() {
            self.last_queue_change_seq = self.sequence;
        }
        self.state = next;
        self.updated_at_ms = now_ms;

        vec![
            Outbound::to_client(
                client_id,
                ServerMessage::OperationAck {
                    operation_id,
                    sequence: self.sequence,
                    applied: true,
                    accepted_at_ms: now_ms,
                },
            ),
            Outbound {
                to: Recipient::AllExcept(client_id.to_owned()),
                message: ServerMessage::OperationAccepted {
                    sequence: self.sequence,
                    origin_client_id: client_id.to_owned(),
                    operation,
                    accepted_at_ms: now_ms,
                },
            },
        ]
    }

    fn reject(
        &self,
        client_id: &str,
        operation_id: String,
        code: RejectCode,
        message: String,
    ) -> Outbound {
        Outbound::to_client(
            client_id,
            ServerMessage::OperationRejected {
                operation_id,
                code,
                message,
                current_sequence: self.sequence,
            },
        )
    }

    /// Mark a participant's socket as gone (`false`) or back (`true`).
    /// Returns a participants update if anything changed.
    pub fn set_connected(&mut self, client_id: &str, connected: bool) -> Vec<Outbound> {
        let changed = self
            .participant_mut(client_id)
            .is_some_and(|p| std::mem::replace(&mut p.connected, connected) != connected);
        if changed {
            vec![self.participants_update()]
        } else {
            Vec::new()
        }
    }

    /// Remove a participant whose reconnect grace period expired.
    pub fn remove(&mut self, client_id: &str) -> Vec<Outbound> {
        let before = self.participants.len();
        self.participants.retain(|p| p.client_id != client_id);
        if self.participants.len() != before {
            vec![self.participants_update()]
        } else {
            Vec::new()
        }
    }

    pub fn snapshot(&self, now_ms: u64) -> SessionSnapshot {
        SessionSnapshot {
            protocol_version: PROTOCOL_VERSION,
            session_id: self.session_id.clone(),
            revision: self.sequence,
            updated_at_ms: self.updated_at_ms,
            state: state::at_time(&self.state, now_ms),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn state(&self) -> &SessionMusicState {
        &self.state
    }

    pub fn participants(&self) -> &[Participant] {
        &self.participants
    }

    pub fn is_member(&self, client_id: &str) -> bool {
        self.participants.iter().any(|p| p.client_id == client_id)
    }

    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    fn participant_mut(&mut self, client_id: &str) -> Option<&mut Participant> {
        self.participants
            .iter_mut()
            .find(|p| p.client_id == client_id)
    }

    fn participants_update(&self) -> Outbound {
        Outbound {
            to: Recipient::All,
            message: ServerMessage::Participants {
                participants: self.participants.clone(),
            },
        }
    }
}

fn participant(hello: ClientHello, status: ParticipantStatus) -> Participant {
    Participant {
        client_id: hello.client_id,
        display_name: hello.display_name,
        provider: hello.provider,
        status,
        connected: true,
    }
}

fn error_to(client_id: &str, code: ErrorCode, message: &str) -> Outbound {
    Outbound::to_client(
        client_id,
        ServerMessage::Error {
            code,
            message: message.to_owned(),
        },
    )
}
