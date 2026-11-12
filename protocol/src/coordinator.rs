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
//!
//! # Songs nobody can play
//!
//! Participants report which queue entries their provider could not find
//! (`participant.setUnavailable`). *Listeners* are participants who haven't
//! stepped out, including ones briefly disconnected. If there is at least one
//! listener and none of them can play the current entry, the coordinator
//! issues its own `playback.advance` to the next entry someone can play (or
//! pauses if there is none). Server-issued operations go to everyone with
//! `originClientId: null`. The check runs after every change that could
//! matter: queue changes, reports, status changes, and participants leaving.

use std::collections::{HashMap, HashSet};

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
    /// Each participant's rejoin secret, by client ID. Never logged.
    rejoin_secrets: HashMap<String, String>,
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
    /// Create a session. `session_id` (the join code) and the creator's
    /// `rejoin_secret` are generated by the caller (the server owns the
    /// randomness).
    pub fn create(
        session_id: String,
        rejoin_secret: String,
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
            rejoin_secrets: HashMap::from([(hello.client_id.clone(), rejoin_secret.clone())]),
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
                    rejoin_secret,
                    snapshot: coordinator.snapshot(now_ms),
                },
            ),
            coordinator.participants_update(),
        ];
        Ok((coordinator, outbound))
    }

    /// Join with the join code, or rejoin after a reconnect. A client ID that
    /// already has a seat must prove it with its rejoin secret, since client
    /// IDs are visible to everyone in the session. `new_secret` is used only
    /// for a new participant.
    pub fn join(
        &mut self,
        request: JoinSession,
        new_secret: String,
        now_ms: u64,
    ) -> Result<Vec<Outbound>, HandshakeError> {
        let hello = validate_hello(&request.hello)?;
        let invalid = || {
            HandshakeError::new(
                ErrorCode::InvalidCredentials,
                "unknown join code or invalid rejoin secret",
            )
        };
        if request.session_id != self.session_id {
            return Err(invalid());
        }

        let client_id = hello.client_id.clone();
        if self.is_member(&client_id) {
            let expected = self.rejoin_secrets.get(&client_id).map(String::as_bytes);
            let given = request.rejoin_secret.as_deref().map(str::as_bytes);
            let proven = matches!((expected, given), (Some(e), Some(g)) if constant_time_eq(e, g));
            if !proven {
                return Err(invalid());
            }
        }
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
                self.rejoin_secrets.insert(client_id.clone(), new_secret);
            }
        }

        let rejoin_secret = self.rejoin_secrets[&client_id].clone();
        Ok(vec![
            Outbound::to_client(
                &client_id,
                ServerMessage::Joined {
                    snapshot: self.snapshot(now_ms),
                    rejoin_secret,
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
                    // Stepping out can leave nobody able to play the song.
                    self.after_change(now_ms, true)
                } else {
                    Vec::new()
                }
            }
            ClientMessage::SetUnavailable { entry_ids } => {
                self.set_unavailable(client_id, entry_ids, now_ms)
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
            ClientMessage::TimePing { client_time_ms } => vec![Outbound::to_client(
                client_id,
                ServerMessage::TimePong {
                    client_time_ms,
                    server_time_ms: now_ms,
                },
            )],
            ClientMessage::Leave => {
                self.participants.retain(|p| p.client_id != client_id);
                self.rejoin_secrets.remove(client_id);
                let mut outbound = vec![Outbound::to_client(client_id, ServerMessage::Left)];
                outbound.extend(self.after_change(now_ms, true));
                outbound
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

        let sender = self
            .participants
            .iter()
            .find(|p| p.client_id == client_id)
            .map(|p| p.display_name.clone());
        let operation = state::stamp_added_by(&self.state, operation, sender.as_deref());

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

        let changes_queue = operation.changes_queue();
        self.sequence += 1;
        if changes_queue {
            self.last_queue_change_seq = self.sequence;
        }
        self.state = next;
        self.updated_at_ms = now_ms;

        let mut outbound = vec![
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
                    origin_client_id: Some(client_id.to_owned()),
                    operation,
                    accepted_at_ms: now_ms,
                },
            },
        ];
        if changes_queue {
            outbound.extend(self.after_change(now_ms, false));
        }
        outbound
    }

    /// Store a participant's complete unavailable set (deduplicated and
    /// limited to entries currently in the queue).
    fn set_unavailable(
        &mut self,
        client_id: &str,
        entry_ids: Vec<String>,
        now_ms: u64,
    ) -> Vec<Outbound> {
        if entry_ids.len() > state::MAX_QUEUE_ENTRIES || !entry_ids.iter().all(|id| valid_id(id)) {
            return vec![error_to(
                client_id,
                ErrorCode::InvalidMessage,
                "invalid unavailable entry list",
            )];
        }
        let in_queue: HashSet<&str> = self
            .state
            .queue
            .iter()
            .map(|e| e.entry_id.as_str())
            .collect();
        let mut seen = HashSet::new();
        let reported: Vec<String> = entry_ids
            .into_iter()
            .filter(|id| in_queue.contains(id.as_str()) && seen.insert(id.clone()))
            .collect();

        let changed = self.participant_mut(client_id).is_some_and(|p| {
            std::mem::replace(&mut p.unavailable_entry_ids, reported) != p.unavailable_entry_ids
        });
        self.after_change(now_ms, changed)
    }

    /// Follow-up after anything that may affect who can play what: skip a
    /// current song nobody can play, drop reports for entries that left the
    /// queue, and publish the participant list if it changed.
    fn after_change(&mut self, now_ms: u64, participants_changed: bool) -> Vec<Outbound> {
        let mut outbound = self.skip_unplayable(now_ms);
        let pruned = self.prune_unavailable();
        if participants_changed || pruned {
            outbound.push(self.participants_update());
        }
        outbound
    }

    fn prune_unavailable(&mut self) -> bool {
        let in_queue: HashSet<&str> = self
            .state
            .queue
            .iter()
            .map(|e| e.entry_id.as_str())
            .collect();
        let mut changed = false;
        for participant in &mut self.participants {
            let before = participant.unavailable_entry_ids.len();
            participant
                .unavailable_entry_ids
                .retain(|id| in_queue.contains(id.as_str()));
            changed |= participant.unavailable_entry_ids.len() != before;
        }
        changed
    }

    /// If no listener can play the current entry, advance to the next one
    /// someone can play, or pause when there is none.
    fn skip_unplayable(&mut self, now_ms: u64) -> Vec<Outbound> {
        let listeners: Vec<HashSet<&str>> = self
            .participants
            .iter()
            .filter(|p| p.status != ParticipantStatus::Detached)
            .map(|p| p.unavailable_entry_ids.iter().map(String::as_str).collect())
            .collect();
        if listeners.is_empty() {
            return Vec::new();
        }
        let unplayable =
            |entry_id: &str| listeners.iter().all(|missing| missing.contains(entry_id));

        let Some(current) = self.state.current_entry_id.as_deref() else {
            return Vec::new();
        };
        if !unplayable(current) {
            return Vec::new();
        }

        let next = self
            .state
            .queue
            .iter()
            .skip(1)
            .find(|entry| !unplayable(&entry.entry_id))
            .map(|entry| entry.entry_id.clone());
        let operation = match next {
            Some(target_entry_id) => SessionOperation::PlaybackAdvance {
                target_entry_id,
                position_ms: 0,
                is_playing: self.state.playback.is_playing,
            },
            None if self.state.playback.is_playing => {
                SessionOperation::PlaybackSetPlaying { is_playing: false }
            }
            None => return Vec::new(),
        };
        drop(listeners);

        let Ok(next_state) = state::apply(&self.state, &operation, now_ms) else {
            return Vec::new();
        };
        self.sequence += 1;
        if operation.changes_queue() {
            self.last_queue_change_seq = self.sequence;
        }
        self.state = next_state;
        self.updated_at_ms = now_ms;

        vec![Outbound {
            to: Recipient::All,
            message: ServerMessage::OperationAccepted {
                sequence: self.sequence,
                origin_client_id: None,
                operation,
                accepted_at_ms: now_ms,
            },
        }]
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
    pub fn remove(&mut self, client_id: &str, now_ms: u64) -> Vec<Outbound> {
        let before = self.participants.len();
        self.participants.retain(|p| p.client_id != client_id);
        self.rejoin_secrets.remove(client_id);
        if self.participants.len() != before {
            self.after_change(now_ms, true)
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
        unavailable_entry_ids: Vec::new(),
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
