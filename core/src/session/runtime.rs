//! Session runtime: keeps one provider in step with one session.
//!
//! A single worker task owns the [`SessionClient`] and a session *mirror*
//! (the session state as of the last applied sequence) and reacts to three
//! inputs: local provider events, session events, and user commands. All of
//! them are handled one at a time, so the provider sees changes strictly in
//! session order.
//!
//! # Ordering
//!
//! Accepted changes, both our own (from acks) and others' (broadcasts), go
//! into a buffer keyed by sequence and are applied only when they are next.
//! Our own changes update the mirror only, since the provider already has
//! them. Remote changes update the mirror and, while synced, the provider.
//!
//! # Songs this provider can't play
//!
//! The provider's queue is the session queue minus the entries this
//! provider couldn't find (`unavailable`). The runtime keeps that set,
//! reports it to the server, re-inserts those entries into local queue
//! edits, and waits instead of playing when one of them becomes current.
//!
//! # Timing
//!
//! The mirror is on the server's clock, the provider on the local one; the
//! client's [`ClockEstimate`] converts between them. Positions handed to the
//! provider are caught up to the moment they're applied (a seek that took
//! 80 ms to arrive lands 80 ms further on), and positions published are
//! moved ahead by the time they take to reach the server.

use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Instant, sleep},
};
use tracing::{debug, info, warn};

use crate::{
    Error, Result,
    bridge_controller::{ApplyOutcome, BridgeEvent},
    network::{
        ClockEstimate, CloseReason, JoinCredentials, SessionClient, SessionClientConfig,
        SessionEvent, SubmitOutcome,
    },
    protocol::{
        Participant, ParticipantStatus, QueueEntry, RejectCode, ReplacePlayback, SessionMusicState,
        SessionOperation, SessionSnapshot, state,
    },
    session::{
        bridge::ProviderBridge,
        status::{Notice, QueueItem, RuntimeStatus, SyncState, UnavailableWarning},
    },
};

const NOTICE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 8;
/// Accepted changes waiting for an earlier sequence. More means we are
/// badly out of step, and a snapshot is cheaper.
const MAX_PENDING: usize = 256;
/// How long the provider's state must stay unchanged before it is trusted.
/// After the bridge attaches, TIDAL briefly reports an empty queue and then
/// restores its real one.
const SETTLE_INTERVAL: Duration = Duration::from_millis(500);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(6);
/// Most a position is moved for delivery time. A change that took longer
/// than this to reach us is caught up by the next snapshot, not guessed at.
const MAX_CATCH_UP_MS: u64 = 10_000;
/// Loading a new list, the provider gets the current song and this many
/// songs it doesn't have yet at once; the rest is looked up beside other
/// changes and added afterwards (`defer_long_tail`). Looking up 80 songs
/// took 6–15 s (verified live), and every change waited for it.
const LOAD_NOW: usize = 8;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// `operation` with its playback position moved `by_ms` later, if it has one
/// and playback runs from it. Kept within the track when its length is known.
fn shift_position(
    operation: &SessionOperation,
    playing: bool,
    by_ms: u64,
    duration_ms: Option<u64>,
) -> SessionOperation {
    let shift = |position: u64| {
        let shifted = position.saturating_add(by_ms.min(MAX_CATCH_UP_MS));
        duration_ms.map_or(shifted, |duration| shifted.min(duration))
    };
    let mut operation = operation.clone();
    match &mut operation {
        SessionOperation::PlaybackSeek { position_ms, .. } if playing => {
            *position_ms = shift(*position_ms)
        }
        SessionOperation::PlaybackAdvance {
            position_ms,
            is_playing: true,
            ..
        }
        | SessionOperation::PlaybackSelect {
            position_ms,
            is_playing: true,
            ..
        } => *position_ms = shift(*position_ms),
        SessionOperation::QueueReplace {
            entries,
            current_entry_id,
            playback:
                Some(ReplacePlayback {
                    position_ms,
                    is_playing: true,
                }),
        } => {
            // The new current song's length, from the replace itself.
            let current = current_entry_id
                .as_deref()
                .and_then(|id| entries.iter().find(|entry| entry.entry_id == id))
                .or(entries.first());
            let shifted = position_ms.saturating_add(by_ms.min(MAX_CATCH_UP_MS));
            *position_ms = match current.and_then(|entry| entry.track.duration_ms) {
                Some(duration) => shifted.min(duration),
                None => shifted,
            };
        }
        _ => {}
    }
    operation
}

fn queue_ids(state: &SessionMusicState) -> Vec<&str> {
    state
        .queue
        .iter()
        .map(|entry| entry.entry_id.as_str())
        .collect()
}

/// Wait until the provider's queue and play state stop changing (and the
/// queue isn't empty, unless the timeout passes), then return it. After a
/// reload TIDAL restores its queue and then resumes playback by itself
/// (verified live); both must be over before its state is used.
async fn settled_state<B: ProviderBridge>(bridge: &B) -> Result<SessionMusicState> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    let mut previous: Option<(Vec<String>, Option<String>, bool)> = None;
    loop {
        let current = bridge.get_state().await?;
        let key = (
            queue_ids(&current).into_iter().map(str::to_owned).collect(),
            current.current_entry_id.clone(),
            current.playback.is_playing,
        );
        let stable = previous.as_ref() == Some(&key);
        if (stable && !current.queue.is_empty()) || Instant::now() >= deadline {
            return Ok(current);
        }
        previous = Some(key);
        sleep(SETTLE_INTERVAL).await;
    }
}

/// A queue change made in Attune's own UI. Deliberately only queue edits:
/// playback is controlled in the music client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueEdit {
    Move { entry_id: String, to_index: usize },
    Remove { entry_id: String },
}

impl QueueEdit {
    fn operation(self) -> SessionOperation {
        match self {
            Self::Move { entry_id, to_index } => SessionOperation::QueueMove { entry_id, to_index },
            Self::Remove { entry_id } => SessionOperation::QueueRemove { entry_id },
        }
    }
}

/// Switch off what a session can't follow; failures only cost that.
async fn prepare<B: ProviderBridge>(bridge: &B) -> Value {
    bridge.prepare_for_session().await.unwrap_or_else(|error| {
        warn!(%error, "Could not prepare the music client for the session");
        Value::Null
    })
}

/// Put the user's settings back after a session.
async fn restore<B: ProviderBridge>(bridge: &B, settings: &Value) {
    if settings.is_null() {
        return;
    }
    if let Err(error) = bridge.restore_after_session(settings).await {
        warn!(%error, "Could not restore the music client's settings");
    }
}

enum Command {
    StepOut,
    Resync,
    Edit(QueueEdit, oneshot::Sender<Result<()>>),
    Leave(oneshot::Sender<Result<()>>),
}

/// Handle to a running session. Dropping it closes the session connection
/// without leaving; call [`SessionRuntime::leave`] to leave.
pub struct SessionRuntime {
    commands: mpsc::Sender<Command>,
    status: watch::Receiver<RuntimeStatus>,
    notices: broadcast::Sender<Notice>,
    credentials: JoinCredentials,
    task: JoinHandle<()>,
}

impl SessionRuntime {
    /// Create a session from the provider's current state.
    pub async fn create<B: ProviderBridge>(
        bridge: Arc<B>,
        config: SessionClientConfig,
    ) -> Result<Self> {
        let settings = prepare(bridge.as_ref()).await;
        match Self::create_prepared(bridge.clone(), config, settings.clone()).await {
            Ok(runtime) => Ok(runtime),
            Err(error) => {
                restore(bridge.as_ref(), &settings).await;
                Err(error)
            }
        }
    }

    async fn create_prepared<B: ProviderBridge>(
        bridge: Arc<B>,
        config: SessionClientConfig,
        settings: Value,
    ) -> Result<Self> {
        let mut initial = settled_state(bridge.as_ref()).await?;
        initial.queue.truncate(state::MAX_QUEUE_ENTRIES);
        if initial.queue.is_empty() {
            initial.current_entry_id = None;
            initial.playback.is_playing = false;
            initial.playback.position_ms = 0;
        }
        let (client, start) = SessionClient::create(config, initial).await?;
        let mut worker = Worker::new(
            bridge,
            client,
            start.events,
            start.snapshot,
            SyncState::Synced,
        );
        worker.provider_settings = settings;
        Ok(Self::spawn(worker, start.credentials))
    }

    /// Join a session and apply its current state before returning.
    pub async fn join<B: ProviderBridge>(
        bridge: Arc<B>,
        config: SessionClientConfig,
        credentials: JoinCredentials,
    ) -> Result<Self> {
        let settings = prepare(bridge.as_ref()).await;
        match Self::join_prepared(bridge.clone(), config, credentials, settings.clone()).await {
            Ok(runtime) => Ok(runtime),
            Err(error) => {
                restore(bridge.as_ref(), &settings).await;
                Err(error)
            }
        }
    }

    async fn join_prepared<B: ProviderBridge>(
        bridge: Arc<B>,
        config: SessionClientConfig,
        credentials: JoinCredentials,
        settings: Value,
    ) -> Result<Self> {
        // Let a freshly attached provider finish restoring its own queue, so
        // that restore isn't published as a local edit later.
        settled_state(bridge.as_ref()).await?;
        let (client, start) = SessionClient::join(config, credentials).await?;
        let snapshot = start.snapshot.clone();
        let mut worker = Worker::new(
            bridge,
            client,
            start.events,
            start.snapshot,
            SyncState::Resyncing,
        );
        worker.provider_settings = settings;
        worker.apply_snapshot(snapshot).await;
        Ok(Self::spawn(worker, start.credentials))
    }

    fn spawn<B: ProviderBridge>(mut worker: Worker<B>, credentials: JoinCredentials) -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        worker.commands = Some(receiver);
        let status = worker.status.subscribe();
        let notices = worker.notices.clone();
        let task = tokio::spawn(worker.run());
        Self {
            commands,
            status,
            notices,
            credentials,
            task,
        }
    }

    pub fn credentials(&self) -> &JoinCredentials {
        &self.credentials
    }

    pub fn status(&self) -> RuntimeStatus {
        self.status.borrow().clone()
    }

    pub fn watch_status(&self) -> watch::Receiver<RuntimeStatus> {
        self.status.clone()
    }

    pub fn notices(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    /// Stop following until [`SessionRuntime::resync`].
    pub async fn step_out(&self) -> Result<()> {
        self.commands
            .send(Command::StepOut)
            .await
            .map_err(|_| Error::SessionClosed)
    }

    /// Apply the session's current state and follow again.
    pub async fn resync(&self) -> Result<()> {
        self.commands
            .send(Command::Resync)
            .await
            .map_err(|_| Error::SessionClosed)
    }

    /// Change the session queue. Works whatever the sync state: the queue
    /// belongs to the session, so even someone who stepped out (or can't play
    /// the song) can reorder or remove it. Returns once the server accepted
    /// it.
    pub async fn edit_queue(&self, edit: QueueEdit) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Edit(edit, reply))
            .await
            .map_err(|_| Error::SessionClosed)?;
        response.await.map_err(|_| Error::SessionClosed)?
    }

    /// Stop without leaving (the seat is kept for a rejoin, e.g. when Attune
    /// quits). Waits until the provider's settings are restored and the
    /// connection is closed.
    pub async fn close(self) {
        drop(self.commands);
        let _ = self.task.await;
    }

    /// Leave the session. The provider stays connected.
    pub async fn leave(self) -> Result<()> {
        let (reply, response) = oneshot::channel();
        if self.commands.send(Command::Leave(reply)).await.is_err() {
            return Ok(()); // already ended
        }
        let result = response.await.unwrap_or(Ok(()));
        let _ = self.task.await;
        result
    }
}

/// An accepted change waiting for its turn.
enum Pending {
    /// Ours: already in the provider; mirror only.
    Own(SessionOperation, u64),
    /// Someone else's (or the server's): mirror and, if synced, provider.
    Remote(SessionOperation, u64),
}

enum Flow {
    Continue,
    Stop,
}

struct Worker<B> {
    bridge: Arc<B>,
    client: SessionClient,
    client_events: broadcast::Receiver<SessionEvent>,
    bridge_events: broadcast::Receiver<BridgeEvent>,
    commands: Option<mpsc::Receiver<Command>>,
    status: watch::Sender<RuntimeStatus>,
    notices: broadcast::Sender<Notice>,

    session_id: String,
    mirror: SessionMusicState,
    applied: u64,
    pending: BTreeMap<u64, Pending>,
    sync: SyncState,
    /// Entry the provider couldn't play while waiting for the next song.
    waiting_for: Option<String>,
    /// Whether to return to SteppedOut after a reconnect.
    stepped_out_before_reconnect: bool,
    unavailable: HashSet<String>,
    /// Entries left out of the provider until a background lookup is done
    /// (`defer_long_tail`). Treated like `unavailable` for the provider's
    /// queue, but not reported: they are expected to play.
    deferred: HashSet<String>,
    /// Increased with every new deferral; a lookup for an older one is
    /// ignored.
    deferral: u64,
    prefetched_tx: mpsc::UnboundedSender<(u64, Result<ApplyOutcome>)>,
    prefetched_rx: mpsc::UnboundedReceiver<(u64, Result<ApplyOutcome>)>,
    reported_unavailable: Option<Vec<String>>,
    reported_status: Option<ParticipantStatus>,
    participants: Vec<Participant>,
    /// The user's provider settings from before the session, restored when
    /// it ends (however it ends).
    provider_settings: Value,
    /// The provider was interrupted (e.g. by an advertisement) and this
    /// runtime stepped out because of it; it rejoins when that is over.
    interrupted: bool,
}

impl<B: ProviderBridge> Worker<B> {
    fn new(
        bridge: Arc<B>,
        client: SessionClient,
        client_events: broadcast::Receiver<SessionEvent>,
        snapshot: SessionSnapshot,
        sync: SyncState,
    ) -> Self {
        let bridge_events = bridge.subscribe();
        let session_id = snapshot.session_id.clone();
        let initial_status = RuntimeStatus {
            session_id: session_id.clone(),
            client_id: client.client_id().to_owned(),
            sync,
            sequence: snapshot.revision,
            queue: Vec::new(),
            playback: snapshot.state.playback.clone(),
            participants: Vec::new(),
            warnings: Vec::new(),
        };
        let (status, _) = watch::channel(initial_status);
        let (notices, _) = broadcast::channel(NOTICE_CAPACITY);
        let (prefetched_tx, prefetched_rx) = mpsc::unbounded_channel();
        let worker = Self {
            bridge,
            client,
            client_events,
            bridge_events,
            commands: None,
            status,
            notices,
            session_id,
            applied: snapshot.revision,
            mirror: snapshot.state,
            pending: BTreeMap::new(),
            sync,
            waiting_for: None,
            stepped_out_before_reconnect: false,
            unavailable: HashSet::new(),
            deferred: HashSet::new(),
            deferral: 0,
            prefetched_tx,
            prefetched_rx,
            reported_unavailable: None,
            reported_status: sync.participant_status(),
            participants: Vec::new(),
            provider_settings: Value::Null,
            interrupted: false,
        };
        worker.publish();
        worker
    }

    fn client(&self) -> &SessionClient {
        &self.client
    }

    /// Until the first clock probe returns, assume the clocks agree.
    fn clock(&self) -> ClockEstimate {
        self.client.clock().unwrap_or_default()
    }

    fn server_now(&self) -> u64 {
        self.clock().server_time(now_ms())
    }

    /// Length of the track at `entry_id` in the mirror, if known.
    fn duration_of(&self, entry_id: &str) -> Option<u64> {
        self.mirror
            .queue
            .iter()
            .find(|entry| entry.entry_id == entry_id)
            .and_then(|entry| entry.track.duration_ms)
    }

    fn target_duration(&self, operation: &SessionOperation) -> Option<u64> {
        match operation {
            SessionOperation::PlaybackSeek { entry_id, .. }
            | SessionOperation::PlaybackSelect { entry_id, .. }
            | SessionOperation::PlaybackAdvance {
                target_entry_id: entry_id,
                ..
            } => self.duration_of(entry_id),
            _ => None,
        }
    }

    fn notify(&self, notice: Notice) {
        let _ = self.notices.send(notice);
    }

    async fn run(mut self) {
        let Some(mut commands) = self.commands.take() else {
            return;
        };
        let leave_reply = loop {
            let flow = tokio::select! {
                command = commands.recv() => match command {
                    None => break None, // handle dropped: close without leaving
                    Some(Command::Leave(reply)) => break Some(reply),
                    Some(Command::StepOut) => {
                        // Their own choice now: don't rejoin for them.
                        self.interrupted = false;
                        self.step_out().await;
                        Flow::Continue
                    }
                    Some(Command::Resync) => { self.resync().await; Flow::Continue }
                    Some(Command::Edit(edit, reply)) => {
                        let _ = reply.send(self.edit(edit).await);
                        Flow::Continue
                    }
                },
                Some((deferral, outcome)) = self.prefetched_rx.recv() => {
                    self.on_prefetched(deferral, outcome).await;
                    Flow::Continue
                }
                event = self.client_events.recv() => match event {
                    Ok(event) => self.on_session_event(event).await,
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        warn!(missed, "Session events lagged; resyncing");
                        self.resync().await;
                        Flow::Continue
                    }
                    Err(broadcast::error::RecvError::Closed) => Flow::Stop,
                },
                event = self.bridge_events.recv() => match event {
                    Ok(BridgeEvent::Operation(operation)) => { self.on_local(operation).await; Flow::Continue }
                    Ok(BridgeEvent::Interruption(interruption)) => {
                        self.on_interruption(interruption.active).await;
                        Flow::Continue
                    }
                    Ok(_) => Flow::Continue,
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        // Local changes were lost; the session's state wins.
                        warn!(missed, "Provider events lagged; resyncing");
                        self.resync().await;
                        Flow::Continue
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        self.notify(Notice::Desynchronized { message: "the music client disconnected".to_owned() });
                        self.set_sync(SyncState::Error).await;
                        Flow::Continue
                    }
                },
            };
            if let Flow::Stop = flow {
                break None;
            }
        };

        self.set_sync(SyncState::Ended).await;
        restore(self.bridge.as_ref(), &self.provider_settings).await;
        let notices = self.notices.clone();
        let session_id = self.session_id.clone();
        let client = self.client; // the worker is done with everything else
        match leave_reply {
            Some(reply) => {
                let _ = reply.send(client.leave().await);
                let _ = notices.send(Notice::Ended {
                    reason: "left".to_owned(),
                });
            }
            None => client.close().await,
        }
        info!(%session_id, "Session runtime stopped");
    }

    // -----------------------------------------------------------------------
    // State and reporting
    // -----------------------------------------------------------------------

    /// Move to `next` if the transition is legal; report it to the session.
    async fn set_sync(&mut self, next: SyncState) -> bool {
        if self.sync == next {
            return true;
        }
        if !self.sync.can_become(next) {
            warn!(from = ?self.sync, to = ?next, "Ignoring an illegal sync transition");
            return false;
        }
        debug!(from = ?self.sync, to = ?next, "Sync state");
        self.sync = next;
        if next != SyncState::WaitingForNextSong {
            self.waiting_for = None;
        }
        if let Some(status) = next.participant_status() {
            if self.reported_status != Some(status)
                && self.client().set_status(status).await.is_ok()
            {
                self.reported_status = Some(status);
            }
        }
        self.publish();
        true
    }

    async fn report_unavailable(&mut self) {
        let in_queue: HashSet<&str> = queue_ids(&self.mirror).into_iter().collect();
        self.unavailable.retain(|id| in_queue.contains(id.as_str()));
        self.deferred.retain(|id| in_queue.contains(id.as_str()));
        let mut report: Vec<String> = self.unavailable.iter().cloned().collect();
        report.sort();
        if self.reported_unavailable.as_ref() != Some(&report)
            && self.client().set_unavailable(report.clone()).await.is_ok()
        {
            self.reported_unavailable = Some(report);
        }
    }

    fn publish(&self) {
        let warnings = self
            .mirror
            .queue
            .iter()
            .enumerate()
            .filter_map(|(songs_ahead, entry)| {
                let missing_for: Vec<String> = self
                    .participants
                    .iter()
                    .filter(|p| p.unavailable_entry_ids.contains(&entry.entry_id))
                    .map(|p| p.display_name.clone())
                    .collect();
                (!missing_for.is_empty()).then(|| UnavailableWarning {
                    entry_id: entry.entry_id.clone(),
                    title: entry.track.title.clone(),
                    artists: entry.track.artists.clone(),
                    songs_ahead,
                    missing_for,
                })
            })
            .collect();
        let queue = self
            .mirror
            .queue
            .iter()
            .map(|entry| QueueItem {
                entry_id: entry.entry_id.clone(),
                track: entry.track.clone(),
                added_by: entry.added_by.clone(),
                playable_here: !self.unavailable.contains(&entry.entry_id),
            })
            .collect();
        let mut playback = self.mirror.playback.clone();
        playback.position_captured_at_ms =
            self.clock().local_time(playback.position_captured_at_ms);
        self.status.send_replace(RuntimeStatus {
            session_id: self.session_id.clone(),
            client_id: self.client.client_id().to_owned(),
            sync: self.sync,
            sequence: self.applied,
            queue,
            playback,
            participants: self.participants.clone(),
            warnings,
        });
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    /// Submit a queue edit from the UI. Once accepted it is applied like
    /// anyone else's change: to the mirror, and to the provider if synced.
    async fn edit(&mut self, edit: QueueEdit) -> Result<()> {
        let operation = edit.operation();
        match self
            .client()
            .submit_operation(operation.clone(), self.applied)
            .await?
        {
            SubmitOutcome::Applied {
                sequence,
                accepted_at_ms,
            } => {
                self.pending
                    .insert(sequence, Pending::Remote(operation, accepted_at_ms));
                self.drain().await;
                Ok(())
            }
            SubmitOutcome::Unchanged { .. } => Ok(()),
            SubmitOutcome::Rejected { code, message, .. } => {
                Err(Error::EditRejected { code, message })
            }
        }
    }

    /// The provider can't follow for a while (an advertisement), or can
    /// again. The session goes on meanwhile: step out, and rejoin after.
    /// Someone who stepped out themselves stays out.
    async fn on_interruption(&mut self, active: bool) {
        if active {
            if matches!(self.sync, SyncState::SteppedOut | SyncState::Ended) {
                return;
            }
            info!(session_id = %self.session_id, "The music client was interrupted; stepping out");
            self.interrupted = true;
            self.step_out().await;
        } else if std::mem::take(&mut self.interrupted) {
            info!(session_id = %self.session_id, "The interruption is over; rejoining");
            self.resync().await;
        }
    }

    async fn step_out(&mut self) {
        if self.sync == SyncState::Reconnecting {
            self.stepped_out_before_reconnect = true;
            return;
        }
        self.set_sync(SyncState::SteppedOut).await;
    }

    /// Fetch and apply the session's current state.
    ///
    /// Boxed because it is reachable from itself (apply_snapshot -> drain ->
    /// resync), and an async fn can't contain its own future.
    fn resync(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.sync == SyncState::Reconnecting {
                // Rejoining delivers a snapshot anyway.
                self.stepped_out_before_reconnect = false;
                return;
            }
            if !self.set_sync(SyncState::Resyncing).await {
                return;
            }
            match self.client().request_snapshot().await {
                Ok(snapshot) => self.apply_snapshot(snapshot).await,
                // A dropped connection reconnects and delivers a snapshot.
                Err(Error::TransportClosed) => {}
                Err(error) => {
                    warn!(%error, "Could not fetch a snapshot");
                    self.notify(Notice::Desynchronized {
                        message: error.to_string(),
                    });
                    self.set_sync(SyncState::Error).await;
                }
            }
        })
    }

    // -----------------------------------------------------------------------
    // Snapshots
    // -----------------------------------------------------------------------

    /// Whether the provider already shows `state` closely enough that
    /// reloading it would only interrupt playback.
    async fn provider_matches(&self, target: &SessionMusicState) -> bool {
        let Ok(local) = self.bridge.get_state().await else {
            return false;
        };
        let local_now = now_ms();
        let server_now = self.clock().server_time(local_now);
        let expected: Vec<&str> = queue_ids(target)
            .into_iter()
            .filter(|id| !self.lacks(id))
            .collect();
        queue_ids(&local) == expected
            && local.current_entry_id == target.current_entry_id
            && local.playback.is_playing == target.playback.is_playing
            && state::position_at(&local, local_now)
                .abs_diff(state::position_at(target, server_now))
                <= state::POSITION_TOLERANCE_MS
    }

    async fn apply_snapshot(&mut self, snapshot: SessionSnapshot) {
        let from_resync = self.sync == SyncState::Resyncing;
        self.mirror = snapshot.state.clone();
        self.applied = snapshot.revision;
        let revision = snapshot.revision;
        self.pending.retain(|sequence, _| *sequence > revision);
        self.set_sync(SyncState::Resyncing).await;
        self.reset_deferral();

        if self.mirror.queue.is_empty() {
            // Nothing to play. Adapters can't safely unload the active track,
            // so just stop.
            let _ = self.bridge.apply_operation(&pause()).await;
            self.unavailable.clear();
            self.set_sync(SyncState::Synced).await;
        } else if self.provider_matches(&self.mirror.clone()).await {
            debug!("Provider already matches the snapshot");
            self.set_sync(SyncState::Synced).await;
        } else {
            // Adapters apply the position as given, so give the current one,
            // stamped with the local clock.
            let mut current = snapshot.clone();
            let local_now = now_ms();
            current.state = state::at_time(&snapshot.state, self.clock().server_time(local_now));
            current.state.playback.position_captured_at_ms = local_now;
            current.state.queue = self.defer_long_tail(current.state.queue, 0).await;
            match self.bridge.apply_snapshot(&current).await {
                Ok(outcome) => {
                    self.unavailable = outcome.unavailable_entry_ids.into_iter().collect();
                    self.set_sync(SyncState::Synced).await;
                }
                Err(Error::TrackUnavailable { entry_ids, .. }) => {
                    self.unavailable = entry_ids.into_iter().collect();
                    let current = self.mirror.current_entry_id.clone();
                    self.wait_for_next_song(current).await;
                }
                Err(error) => {
                    warn!(%error, "Applying the session state failed");
                    self.notify(Notice::Desynchronized {
                        message: error.to_string(),
                    });
                    self.set_sync(SyncState::Error).await;
                }
            }
        }

        if from_resync && self.sync == SyncState::Synced {
            self.notify(Notice::Resynced);
        }
        self.report_unavailable().await;
        self.drain().await;
    }

    async fn wait_for_next_song(&mut self, entry_id: Option<String>) {
        if !self.set_sync(SyncState::WaitingForNextSong).await {
            return;
        }
        self.waiting_for = entry_id;
        let title = self
            .mirror
            .queue
            .first()
            .map(|entry| entry.track.title.clone())
            .unwrap_or_default();
        info!(session_id = %self.session_id, "Current song unavailable here; waiting for the next one");
        self.notify(Notice::SongUnavailable { title });
        // Don't keep playing something else while the others hear this song.
        let _ = self.bridge.apply_operation(&pause()).await;
    }

    // -----------------------------------------------------------------------
    // Session events
    // -----------------------------------------------------------------------

    async fn on_session_event(&mut self, event: SessionEvent) -> Flow {
        match event {
            SessionEvent::RemoteOperation {
                sequence,
                operation,
                accepted_at_ms,
                ..
            } => {
                if sequence > self.applied {
                    self.pending
                        .insert(sequence, Pending::Remote(operation, accepted_at_ms));
                }
                self.drain().await;
            }
            SessionEvent::SequenceGap { .. } => self.resync().await,
            SessionEvent::Participants(participants) => {
                self.participants = participants;
                self.publish();
            }
            SessionEvent::Reconnecting { .. } => {
                if self.sync != SyncState::Reconnecting {
                    self.stepped_out_before_reconnect = self.sync == SyncState::SteppedOut;
                    self.reported_status = None; // the server marks us disconnected
                    self.set_sync(SyncState::Reconnecting).await;
                }
            }
            SessionEvent::Rejoined { snapshot } => {
                // The server forgot nothing, but resend everything we report.
                self.reported_unavailable = None;
                if self.stepped_out_before_reconnect {
                    self.mirror = snapshot.state;
                    self.applied = snapshot.revision;
                    self.pending.clear();
                    self.set_sync(SyncState::SteppedOut).await;
                    self.report_unavailable().await;
                } else {
                    self.set_sync(SyncState::Resyncing).await;
                    self.apply_snapshot(snapshot).await;
                }
            }
            SessionEvent::ServerError { message, .. } => {
                self.notify(Notice::ServerError { message })
            }
            SessionEvent::Closed { reason } => {
                let reason = match reason {
                    CloseReason::Left => "left",
                    CloseReason::ClientClosed => "closed",
                    CloseReason::SessionEnded => "the session no longer exists",
                };
                self.notify(Notice::Ended {
                    reason: reason.to_owned(),
                });
                return Flow::Stop;
            }
        }
        Flow::Continue
    }

    /// Apply buffered changes that are next in sequence.
    async fn drain(&mut self) {
        if self.pending.len() > MAX_PENDING {
            warn!(pending = self.pending.len(), "Too far behind; resyncing");
            self.pending.clear();
            self.resync().await;
            return;
        }
        while let Some(change) = self.pending.remove(&(self.applied + 1)) {
            let sequence = self.applied + 1;
            let (operation, at, remote) = match change {
                Pending::Own(operation, at) => (operation, at, false),
                Pending::Remote(operation, at) => (operation, at, true),
            };
            match state::apply(&self.mirror, &operation, at) {
                Ok(next) => self.mirror = next,
                Err(error) => {
                    // Server and client apply the same rules; this means our
                    // mirror is wrong. Start over from a snapshot.
                    warn!(%error, sequence, "Accepted change does not apply to the mirror");
                    self.resync().await;
                    return;
                }
            }
            self.applied = sequence;
            if remote {
                self.follow(&operation, at).await;
            }
        }
        if self.sync == SyncState::Synced {
            self.ensure_current_playable().await;
        }
        self.report_unavailable().await;
        self.publish();
    }

    /// Bring the provider along for one remote change (already in the
    /// mirror), accepted by the server at `accepted_at_ms`.
    async fn follow(&mut self, operation: &SessionOperation, accepted_at_ms: u64) {
        match self.sync {
            SyncState::Synced => {}
            SyncState::WaitingForNextSong => {
                if self.mirror.current_entry_id != self.waiting_for {
                    info!(session_id = %self.session_id, "Session moved on; rejoining");
                    self.resync().await;
                }
                return;
            }
            _ => return, // mirror only
        }

        let current = self.mirror.current_entry_id.clone();
        if current
            .as_ref()
            .is_some_and(|id| self.unavailable.contains(id))
        {
            self.wait_for_next_song(current).await;
            return;
        }
        // A new list is deferred afresh. So is a song the provider hasn't
        // loaded yet becoming current: everything is loaded around it.
        let current_deferred = current
            .as_ref()
            .is_some_and(|id| self.deferred.contains(id));
        let replaces = matches!(operation, SessionOperation::QueueReplace { .. });
        if replaces || current_deferred {
            self.reset_deferral();
        }
        let operation = if current_deferred && !replaces {
            Some(self.provider_queue_replace(Some(ReplacePlayback {
                position_ms: state::position_at(&self.mirror, accepted_at_ms),
                is_playing: self.mirror.playback.is_playing,
            })))
        } else {
            self.provider_operation(operation)
        };
        let Some(operation) = operation else {
            return;
        };
        let operation = match operation {
            SessionOperation::QueueReplace {
                entries,
                current_entry_id,
                playback,
            } => {
                let start = current_entry_id
                    .as_deref()
                    .and_then(|id| entries.iter().position(|entry| entry.entry_id == id))
                    .unwrap_or(0);
                SessionOperation::QueueReplace {
                    entries: self.defer_long_tail(entries, start).await,
                    current_entry_id,
                    playback,
                }
            }
            other => other,
        };
        let in_transit = self.server_now().saturating_sub(accepted_at_ms);
        let operation = shift_position(
            &operation,
            self.mirror.playback.is_playing,
            in_transit,
            self.target_duration(&operation),
        );
        match self.bridge.apply_operation(&operation).await {
            Ok(outcome) => self.unavailable.extend(outcome.unavailable_entry_ids),
            Err(Error::TrackUnavailable { entry_ids, .. }) => {
                self.unavailable.extend(entry_ids);
                self.wait_for_next_song(current).await;
            }
            Err(error) => {
                warn!(%error, "Applying a remote change failed");
                self.notify(Notice::Desynchronized {
                    message: error.to_string(),
                });
                self.set_sync(SyncState::Error).await;
            }
        }
    }

    /// The operation to give the provider for a remote change, given that
    /// its queue lacks the unavailable entries. `None` if it doesn't concern
    /// the provider at all.
    fn provider_operation(&self, operation: &SessionOperation) -> Option<SessionOperation> {
        let unavailable = |id: &String| self.lacks(id);
        match operation {
            SessionOperation::QueueRemove { entry_id }
            | SessionOperation::QueueMove { entry_id, .. }
                if unavailable(entry_id) =>
            {
                None
            }
            SessionOperation::QueueInsert { entry, .. } if unavailable(&entry.entry_id) => None,
            // Session indices count entries the provider doesn't have.
            // Translate to the provider's queue rather than replacing it,
            // which would restart the current track (verified live on TIDAL).
            SessionOperation::QueueInsert { entry, .. } if self.has_missing() => Some(
                self.provider_index(&entry.entry_id)
                    .map(|index| SessionOperation::QueueInsert {
                        entry: entry.clone(),
                        index,
                    })
                    .unwrap_or_else(|| self.provider_queue_replace(None)),
            ),
            SessionOperation::QueueMove { entry_id, .. } if self.has_missing() => Some(
                self.provider_index(entry_id)
                    .map(|to_index| SessionOperation::QueueMove {
                        entry_id: entry_id.clone(),
                        to_index,
                    })
                    .unwrap_or_else(|| self.provider_queue_replace(None)),
            ),
            // Don't make the provider search again for entries it can't find.
            SessionOperation::QueueReplace { playback, .. } if self.has_missing() => {
                Some(self.provider_queue_replace(*playback))
            }
            other => Some(other.clone()),
        }
    }

    /// Whether the provider's queue leaves `entry_id` out: it can't find it,
    /// or it is still being looked up.
    fn lacks(&self, entry_id: &str) -> bool {
        self.unavailable.contains(entry_id) || self.deferred.contains(entry_id)
    }

    fn has_missing(&self) -> bool {
        !self.unavailable.is_empty() || !self.deferred.is_empty()
    }

    fn missing_here(&self) -> HashSet<String> {
        self.unavailable.union(&self.deferred).cloned().collect()
    }

    /// Forget a deferral; a lookup still running for it is ignored.
    fn reset_deferral(&mut self) {
        self.deferral += 1;
        self.deferred.clear();
    }

    /// Of the entries after `entries[start]` (the current one), keep those
    /// the provider already has and the first [`LOAD_NOW`] it doesn't, and
    /// look the others up in the background. They are added when that is
    /// done (`on_prefetched`); meanwhile the provider follows every other
    /// change. Returns the entries to load now.
    async fn defer_long_tail(&mut self, entries: Vec<QueueEntry>, start: usize) -> Vec<QueueEntry> {
        if entries.len() <= start + 1 + LOAD_NOW {
            return entries;
        }
        let known: HashSet<String> = match self.bridge.get_state().await {
            Ok(local) => local
                .queue
                .into_iter()
                .map(|entry| entry.entry_id)
                .collect(),
            Err(_) => HashSet::new(),
        };
        let mut now = Vec::with_capacity(start + 1 + LOAD_NOW);
        let mut later = Vec::new();
        let mut new_songs = 0;
        for (index, entry) in entries.into_iter().enumerate() {
            if index <= start
                || known.contains(&entry.entry_id)
                || self.unavailable.contains(&entry.entry_id)
            {
                now.push(entry);
            } else if new_songs < LOAD_NOW {
                new_songs += 1;
                now.push(entry);
            } else {
                later.push(entry);
            }
        }
        if later.is_empty() {
            return now;
        }
        debug!(
            now = now.len(),
            later = later.len(),
            "Loading the rest of a long list in the background"
        );
        self.deferred = later.iter().map(|entry| entry.entry_id.clone()).collect();
        let deferral = self.deferral;
        let bridge = self.bridge.clone();
        let done = self.prefetched_tx.clone();
        tokio::spawn(async move {
            let outcome = bridge.prefetch(later).await;
            let _ = done.send((deferral, outcome));
        });
        now
    }

    /// The background lookup for a deferral is done: add its songs.
    async fn on_prefetched(&mut self, deferral: u64, outcome: Result<ApplyOutcome>) {
        if deferral != self.deferral || self.deferred.is_empty() {
            return; // a newer list or a resync took over
        }
        match outcome {
            Ok(outcome) => self.unavailable.extend(outcome.unavailable_entry_ids),
            // The provider then looks them up while adding them.
            Err(error) => warn!(%error, "Looking up the rest of the list failed"),
        }
        self.deferred.clear();
        if self.sync == SyncState::Synced {
            // Same current song: the provider only adds to its queue.
            let operation = self.provider_queue_replace(None);
            match self.bridge.apply_operation(&operation).await {
                Ok(outcome) => self.unavailable.extend(outcome.unavailable_entry_ids),
                Err(Error::TrackUnavailable { entry_ids, .. }) => {
                    self.unavailable.extend(entry_ids);
                    let current = self.mirror.current_entry_id.clone();
                    self.wait_for_next_song(current).await;
                }
                Err(error) => {
                    warn!(%error, "Adding the rest of the list failed");
                    self.notify(Notice::Desynchronized {
                        message: error.to_string(),
                    });
                    self.set_sync(SyncState::Error).await;
                }
            }
        }
        // Otherwise the next resync loads everything.
        self.report_unavailable().await;
        self.publish();
    }

    /// Position of `entry_id` in the provider's queue: the number of entries
    /// before it in the (already updated) mirror that the provider has.
    fn provider_index(&self, entry_id: &str) -> Option<usize> {
        let position = self
            .mirror
            .queue
            .iter()
            .position(|entry| entry.entry_id == entry_id)?;
        Some(
            self.mirror.queue[..position]
                .iter()
                .filter(|entry| !self.lacks(&entry.entry_id))
                .count(),
        )
    }

    fn provider_queue_replace(&self, playback: Option<ReplacePlayback>) -> SessionOperation {
        SessionOperation::QueueReplace {
            entries: self
                .mirror
                .queue
                .iter()
                .filter(|entry| !self.lacks(&entry.entry_id))
                .cloned()
                .collect(),
            current_entry_id: self.mirror.current_entry_id.clone(),
            playback,
        }
    }

    async fn ensure_current_playable(&mut self) {
        let current = self.mirror.current_entry_id.clone();
        if current
            .as_ref()
            .is_some_and(|id| self.unavailable.contains(id))
        {
            self.wait_for_next_song(current).await;
        }
    }

    // -----------------------------------------------------------------------
    // Local changes
    // -----------------------------------------------------------------------

    async fn on_local(&mut self, operation: SessionOperation) {
        if self.sync != SyncState::Synced {
            return; // not following: local changes stay local
        }
        // Translate first: the provider's view lacks the entries it couldn't
        // find, so its echo of a remote change only matches the session once
        // they are put back (verified live).
        let operation = match self.for_empty_session(operation).await {
            Some(operation) => operation,
            None => return,
        };
        let operation = self.session_operation(operation);
        if state::is_noop(&self.mirror, &operation, self.server_now()) {
            debug!("Dropping a local change that matches the session (echo)");
            return;
        }
        // An entry the session already has (published with the provider's
        // whole queue, see `for_empty_session`) can't be inserted again;
        // the server would refuse it and the refusal cost a resync.
        if let SessionOperation::QueueInsert { entry, .. } = &operation {
            if self
                .mirror
                .queue
                .iter()
                .any(|e| e.entry_id == entry.entry_id)
            {
                debug!("Dropping a local insert of an entry the session has");
                return;
            }
        }
        let operation = self.with_actual_playback(operation).await;
        self.submit_local(operation).await;
    }

    /// While the session has nothing to play, what the provider plays
    /// becomes its queue. A provider that joined an empty session keeps
    /// its own queue, so its later changes (inserts, a play) refer to songs
    /// the session doesn't have: the server refused each, and each refusal
    /// resynced to the empty session, which pauses (verified live: Spotify
    /// starting a playlist was paused five times in a row and nothing was
    /// shared). `None`: nothing to publish.
    async fn for_empty_session(&self, operation: SessionOperation) -> Option<SessionOperation> {
        if !self.mirror.queue.is_empty()
            || matches!(operation, SessionOperation::QueueReplace { .. })
        {
            return Some(operation);
        }
        let local = self.bridge.get_state().await.ok()?;
        if local.queue.is_empty() {
            return None;
        }
        let mut entries = local.queue;
        entries.truncate(state::MAX_QUEUE_ENTRIES);
        let current_entry_id = local
            .current_entry_id
            .filter(|id| entries.iter().any(|entry| &entry.entry_id == id));
        Some(SessionOperation::QueueReplace {
            entries,
            current_entry_id,
            playback: None, // filled in by `with_actual_playback`
        })
    }

    /// A replace that changes the current song carries where the provider
    /// really is in it (protocol 2). Without it the session starts the song
    /// at 0, playing, but Spotify resumes an album or playlist part-way
    /// (verified live: 48 s and 164 s in), and a provider sharing its queue
    /// into an empty session may be paused. The others then played that far
    /// behind, and the next resync threw this provider back to the
    /// session's position, which sounds like the song restarting. (A seek
    /// sent after the replace reached the others only once they had looked
    /// up the whole new list: 8 s for 50 songs on YouTube Music.)
    async fn with_actual_playback(&self, operation: SessionOperation) -> SessionOperation {
        let SessionOperation::QueueReplace {
            entries,
            current_entry_id,
            playback: None,
        } = operation
        else {
            return operation;
        };
        let new_current = current_entry_id
            .clone()
            .or_else(|| entries.first().map(|entry| entry.entry_id.clone()));
        let mut playback = None;
        if new_current.is_some() && new_current != self.mirror.current_entry_id {
            if let Ok(local) = self.bridge.get_state().await {
                if local.current_entry_id == new_current {
                    playback = Some(ReplacePlayback {
                        position_ms: state::position_at(&local, now_ms()),
                        is_playing: local.playback.is_playing,
                    });
                }
            }
        }
        SessionOperation::QueueReplace {
            entries,
            current_entry_id,
            playback,
        }
    }

    /// Submit a change made in the provider (already translated). Returns
    /// whether the server applied it.
    async fn submit_local(&mut self, operation: SessionOperation) -> bool {
        // The server takes the position as of when it accepts the change.
        let operation = shift_position(
            &operation,
            self.mirror.playback.is_playing,
            self.clock().one_way_ms(),
            self.target_duration(&operation),
        );

        let outcome = self
            .client()
            .submit_operation(operation.clone(), self.applied)
            .await;
        match outcome {
            Ok(SubmitOutcome::Applied {
                sequence,
                accepted_at_ms,
            }) => {
                self.pending
                    .insert(sequence, Pending::Own(operation, accepted_at_ms));
                self.drain().await;
                return true;
            }
            Ok(SubmitOutcome::Unchanged { .. }) => {}
            Ok(SubmitOutcome::Rejected { code, .. }) => {
                if code == RejectCode::StaleQueue {
                    self.notify(Notice::QueueEditUndone);
                }
                // Either way the provider now differs from the session.
                self.resync().await;
            }
            // Connection trouble: reconnecting delivers a snapshot.
            Err(error) => debug!(%error, "Submitting a local change failed"),
        }
        false
    }

    /// Turn a provider-reported change into the session change it means.
    fn session_operation(&self, operation: SessionOperation) -> SessionOperation {
        // The provider doesn't keep who queued what; take it from the
        // session, as the server will.
        let own_name = self
            .participants
            .iter()
            .find(|p| p.client_id == self.client.client_id())
            .map(|p| p.display_name.as_str());
        let operation = state::stamp_added_by(&self.mirror, operation, own_name);
        match operation {
            // The provider's queue lacks our unavailable entries; publishing
            // it as-is would delete them for everyone.
            SessionOperation::QueueReplace {
                entries,
                current_entry_id,
                playback,
            } => SessionOperation::QueueReplace {
                entries: state::reinsert_unavailable(
                    &entries,
                    &self.mirror.queue,
                    &self.missing_here(),
                ),
                current_entry_id,
                playback,
            },
            // At song end the provider skips straight to the next song it
            // has. For the session, the song that ended is followed by the
            // one we're missing; advance to that and let it play for others.
            SessionOperation::PlaybackAdvance {
                target_entry_id,
                position_ms,
                is_playing,
            } => {
                let skipped = self
                    .mirror
                    .queue
                    .iter()
                    .position(|entry| entry.entry_id == target_entry_id)
                    .filter(|index| *index > 1)
                    .map(|index| &self.mirror.queue[1..index])
                    .filter(|skipped| {
                        skipped
                            .iter()
                            .all(|entry| self.unavailable.contains(&entry.entry_id))
                    });
                match skipped {
                    Some(skipped) => SessionOperation::PlaybackAdvance {
                        target_entry_id: skipped[0].entry_id.clone(),
                        position_ms: 0,
                        is_playing,
                    },
                    None => SessionOperation::PlaybackAdvance {
                        target_entry_id,
                        position_ms,
                        is_playing,
                    },
                }
            }
            other => other,
        }
    }
}

fn pause() -> SessionOperation {
    SessionOperation::PlaybackSetPlaying { is_playing: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seek(position_ms: u64) -> SessionOperation {
        SessionOperation::PlaybackSeek {
            entry_id: "e".to_owned(),
            position_ms,
        }
    }

    #[test]
    fn positions_move_on_only_while_playing() {
        assert_eq!(shift_position(&seek(1_000), true, 80, None), seek(1_080));
        assert_eq!(shift_position(&seek(1_000), false, 80, None), seek(1_000));

        let advance = |position_ms, is_playing| SessionOperation::PlaybackAdvance {
            target_entry_id: "e".to_owned(),
            position_ms,
            is_playing,
        };
        // An advance says itself whether playback runs from it.
        assert_eq!(
            shift_position(&advance(0, true), false, 50, None),
            advance(50, true)
        );
        assert_eq!(
            shift_position(&advance(0, false), true, 50, None),
            advance(0, false)
        );
        // Operations without a position are untouched.
        assert_eq!(shift_position(&pause(), true, 50, None), pause());
    }

    #[test]
    fn shifts_stay_within_the_track_and_the_catch_up_limit() {
        assert_eq!(
            shift_position(&seek(179_900), true, 500, Some(180_000)),
            seek(180_000)
        );
        assert_eq!(
            shift_position(&seek(0), true, 60_000, None),
            seek(MAX_CATCH_UP_MS)
        );
    }
}
