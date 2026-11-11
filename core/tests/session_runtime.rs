//! `SessionRuntime` with fake providers against the test peer (real coordinator).

mod support;

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jam_core::{
    Error, Result,
    bridge_controller::{ApplyOutcome, BridgeEvent},
    network::{ReconnectPolicy, SessionClientConfig},
    protocol::{
        ParticipantStatus, QueueEntry, SessionMusicState, SessionOperation as Op, SessionPlayback,
        SessionSnapshot, TrackDescriptor, state,
    },
    session::{Notice, ProviderBridge, SessionRuntime, SyncState},
};
use support::TestServer;
use tokio::{sync::broadcast, time::timeout};

const WAIT: Duration = Duration::from_secs(5);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            position_captured_at_ms: now_ms(),
        },
    }
}

// ---------------------------------------------------------------------------
// Fake provider
// ---------------------------------------------------------------------------

/// Behaves like the adapters: loads what it can find, reports the rest,
/// refuses an unfindable current track without changing anything, and
/// emits local changes as `BridgeEvent::Operation`.
struct FakeProvider {
    state: Mutex<SessionMusicState>,
    missing_titles: Mutex<HashSet<String>>,
    fail_next: AtomicBool,
    events: broadcast::Sender<BridgeEvent>,
    loads: Mutex<usize>,
}

impl FakeProvider {
    fn new(ids: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(music(ids)),
            missing_titles: Mutex::default(),
            fail_next: AtomicBool::new(false),
            events: broadcast::channel(64).0,
            loads: Mutex::new(0),
        })
    }

    fn missing(&self, id: &str) {
        self.missing_titles
            .lock()
            .unwrap()
            .insert(format!("Song {id}"));
    }

    fn snapshot(&self) -> SessionMusicState {
        self.state.lock().unwrap().clone()
    }

    fn ids(&self) -> Vec<String> {
        self.snapshot()
            .queue
            .into_iter()
            .map(|e| e.entry_id)
            .collect()
    }

    fn current(&self) -> Option<String> {
        self.snapshot().current_entry_id
    }

    fn playing(&self) -> bool {
        self.snapshot().playback.is_playing
    }

    fn loads(&self) -> usize {
        *self.loads.lock().unwrap()
    }

    /// The user changes something in the music client.
    fn local(&self, operation: Op) {
        let next =
            state::apply(&self.snapshot(), &operation, now_ms()).expect("local change applies");
        *self.state.lock().unwrap() = next;
        let _ = self.events.send(BridgeEvent::Operation(operation));
    }

    /// The music client reports a change without it being applied here.
    fn emit(&self, operation: Op) {
        let _ = self.events.send(BridgeEvent::Operation(operation));
    }

    fn take_failure(&self, method: &'static str) -> Result<()> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(Error::BridgeCallFailed {
                method,
                message: "injected failure".to_owned(),
            });
        }
        Ok(())
    }

    fn load(&self, target: SessionMusicState) -> Result<ApplyOutcome> {
        let missing = self.missing_titles.lock().unwrap().clone();
        let Some(current) = target.queue.first() else {
            *self.state.lock().unwrap() = target;
            return Ok(ApplyOutcome::default());
        };
        if missing.contains(&current.track.title) {
            return Err(Error::TrackUnavailable {
                entry_ids: vec![current.entry_id.clone()],
                message: format!("{} unavailable", current.track.title),
            });
        }
        let (kept, skipped): (Vec<_>, Vec<_>) = target
            .queue
            .into_iter()
            .partition(|e| !missing.contains(&e.track.title));
        *self.state.lock().unwrap() = SessionMusicState {
            current_entry_id: kept.first().map(|e| e.entry_id.clone()),
            queue: kept,
            playback: target.playback,
        };
        *self.loads.lock().unwrap() += 1;
        Ok(ApplyOutcome {
            unavailable_entry_ids: skipped.into_iter().map(|e| e.entry_id).collect(),
        })
    }
}

impl ProviderBridge for FakeProvider {
    async fn get_state(&self) -> Result<SessionMusicState> {
        Ok(self.snapshot())
    }

    async fn apply_snapshot(&self, snapshot: &SessionSnapshot) -> Result<ApplyOutcome> {
        self.take_failure("applySnapshot")?;
        self.load(snapshot.state.clone())
    }

    async fn apply_operation(&self, operation: &Op) -> Result<ApplyOutcome> {
        self.take_failure("applyOperation")?;
        let next = state::apply(&self.snapshot(), operation, now_ms()).map_err(|error| {
            Error::BridgeCallFailed {
                method: "applyOperation",
                message: error.to_string(),
            }
        })?;
        match operation {
            Op::PlaybackSetPlaying { .. } | Op::PlaybackSeek { .. } => {
                *self.state.lock().unwrap() = next;
                Ok(ApplyOutcome::default())
            }
            _ => self.load(next),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.events.subscribe()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn config(server: &TestServer, client_id: &str) -> SessionClientConfig {
    let mut config = SessionClientConfig::new(&server.url, client_id, format!("User {client_id}"));
    config.request_timeout = Duration::from_secs(2);
    config.reconnect = ReconnectPolicy {
        initial_delay: Duration::from_millis(50),
        max_delay: Duration::from_millis(200),
    };
    config
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    let result = timeout(WAIT, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(result.is_ok(), "timed out waiting until {what}");
}

async fn sync_is(runtime: &SessionRuntime, expected: SyncState) {
    eventually(&format!("sync is {expected:?}"), || {
        runtime.status().sync == expected
    })
    .await;
}

struct Session {
    server: TestServer,
    a: SessionRuntime,
    a_provider: Arc<FakeProvider>,
    b: SessionRuntime,
    b_provider: Arc<FakeProvider>,
}

/// A creates from `[s1, s2, s3]`; B joins with a provider missing `b_missing`.
async fn session(b_missing: &[&str]) -> Session {
    let server = TestServer::start().await;
    let a_provider = FakeProvider::new(&["s1", "s2", "s3"]);
    let a = SessionRuntime::create(a_provider.clone(), config(&server, "a"))
        .await
        .expect("create");
    let b_provider = FakeProvider::new(&["x1"]);
    for id in b_missing {
        b_provider.missing(id);
    }
    let b = SessionRuntime::join(
        b_provider.clone(),
        config(&server, "b"),
        a.credentials().clone(),
    )
    .await
    .expect("join");
    Session {
        server,
        a,
        a_provider,
        b,
        b_provider,
    }
}

fn advance(target: &str) -> Op {
    Op::PlaybackAdvance {
        target_entry_id: target.to_owned(),
        position_ms: 0,
        is_playing: true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn joining_applies_the_session_and_changes_flow_both_ways() {
    let s = session(&[]).await;
    assert_eq!(s.b_provider.ids(), ["s1", "s2", "s3"]);
    assert_eq!(s.b.status().sync, SyncState::Synced);
    assert_eq!(
        s.a_provider.loads(),
        0,
        "the creator's provider is the seed, not reloaded"
    );

    s.a_provider
        .local(Op::PlaybackSetPlaying { is_playing: false });
    eventually("B paused", || !s.b_provider.playing()).await;

    s.b_provider.local(Op::QueueRemove {
        entry_id: "s3".to_owned(),
    });
    eventually("A removed s3", || s.a_provider.ids() == ["s1", "s2"]).await;

    s.a_provider.local(advance("s2"));
    eventually("B advanced", || {
        s.b_provider.current().as_deref() == Some("s2")
    })
    .await;

    let session_id = s.a.credentials().session_id.clone();
    eventually("both at the server's sequence", || {
        let sequence = s.server.sequence(&session_id);
        s.a.status().sequence == sequence && s.b.status().sequence == sequence
    })
    .await;
    assert_eq!(
        s.server
            .session_state(&session_id)
            .current_entry_id
            .as_deref(),
        Some("s2")
    );
    assert_eq!(
        s.a_provider.loads(),
        1,
        "A's own changes are not applied back to A"
    );
}

#[tokio::test]
async fn local_echoes_of_the_session_state_are_not_published() {
    let s = session(&[]).await;
    let session_id = s.a.credentials().session_id.clone();
    // Already playing: reporting "playing" again changes nothing.
    s.a_provider
        .emit(Op::PlaybackSetPlaying { is_playing: true });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.server.sequence(&session_id), 0);
}

#[tokio::test]
async fn missing_upcoming_songs_are_reported_and_shown_to_others() {
    let s = session(&["s2"]).await;
    assert_eq!(s.b_provider.ids(), ["s1", "s3"]);

    eventually("A sees B's warning for s2", || {
        s.a.status()
            .warnings
            .iter()
            .any(|w| w.entry_id == "s2" && w.missing_for == ["User b"] && w.songs_ahead == 1)
    })
    .await;
}

#[tokio::test]
async fn waits_on_a_missing_song_and_rejoins_at_the_next() {
    let s = session(&["s2"]).await;
    let mut notices = s.b.notices();

    s.a_provider.local(advance("s2"));
    sync_is(&s.b, SyncState::WaitingForNextSong).await;
    assert!(
        !s.b_provider.playing(),
        "B pauses instead of playing something else"
    );
    let notice = timeout(WAIT, notices.recv())
        .await
        .expect("notice")
        .expect("open");
    assert_eq!(
        notice,
        Notice::SongUnavailable {
            title: "Song s2".to_owned()
        }
    );
    eventually("A sees B waiting", || {
        s.a.status()
            .participants
            .iter()
            .any(|p| p.client_id == "b" && p.status == ParticipantStatus::WaitingForNextSong)
    })
    .await;

    s.a_provider.local(advance("s3"));
    sync_is(&s.b, SyncState::Synced).await;
    assert_eq!(s.b_provider.current().as_deref(), Some("s3"));
}

#[tokio::test]
async fn local_song_end_does_not_skip_a_song_others_can_play() {
    let s = session(&["s2"]).await;

    // B's provider has no s2, so at the end of s1 it jumps straight to s3.
    s.b_provider.local(advance("s3"));

    eventually("A plays s2", || {
        s.a_provider.current().as_deref() == Some("s2")
    })
    .await;
    sync_is(&s.b, SyncState::WaitingForNextSong).await;
}

#[tokio::test]
async fn local_queue_edits_keep_songs_this_provider_lacks() {
    let s = session(&["s2"]).await;

    // B's provider shows [s1, s3]; the user adds a song at the end.
    s.b_provider.local(Op::QueueReplace {
        entries: vec![entry("s1"), entry("s3"), entry("n1")],
        current_entry_id: Some("s1".to_owned()),
    });
    eventually("A has the new song and still s2", || {
        s.a_provider.ids() == ["s1", "s2", "s3", "n1"]
    })
    .await;
}

#[tokio::test]
async fn song_nobody_listening_can_play_is_skipped() {
    let s = session(&["s2"]).await;
    // Only B is listening now, and B can't play s2.
    s.a.step_out().await.expect("step out");
    sync_is(&s.a, SyncState::SteppedOut).await;

    s.b_provider.local(advance("s3"));

    // B publishes the advance to s2; the server skips it to s3 at once.
    eventually("B plays s3", || {
        s.b.status().sync == SyncState::Synced && s.b_provider.current().as_deref() == Some("s3")
    })
    .await;
    assert_eq!(
        s.a_provider.current().as_deref(),
        Some("s1"),
        "stepped out: A is untouched"
    );
}

#[tokio::test]
async fn stepping_out_stops_following_until_resync() {
    let s = session(&[]).await;
    s.b.step_out().await.expect("step out");
    sync_is(&s.b, SyncState::SteppedOut).await;
    eventually("A sees B stepped out", || {
        s.a.status()
            .participants
            .iter()
            .any(|p| p.client_id == "b" && p.status == ParticipantStatus::Detached)
    })
    .await;

    // B's own changes stay local while stepped out.
    s.b_provider.local(Op::QueueRemove {
        entry_id: "s3".to_owned(),
    });
    s.a_provider
        .local(Op::PlaybackSetPlaying { is_playing: false });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(s.b_provider.playing(), "not following");
    assert_eq!(s.a_provider.ids(), ["s1", "s2", "s3"], "not publishing");

    s.b.resync().await.expect("resync");
    sync_is(&s.b, SyncState::Synced).await;
    assert!(!s.b_provider.playing());
    assert_eq!(s.b_provider.ids(), ["s1", "s2", "s3"]);
}

#[tokio::test]
async fn provider_failure_stops_following_until_resync() {
    let s = session(&[]).await;
    s.b_provider.fail_next.store(true, Ordering::SeqCst);

    s.a_provider
        .local(Op::PlaybackSetPlaying { is_playing: false });
    sync_is(&s.b, SyncState::Error).await;

    s.a_provider.local(Op::QueueRemove {
        entry_id: "s3".to_owned(),
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        s.b_provider.ids(),
        ["s1", "s2", "s3"],
        "later changes are not applied"
    );

    s.b.resync().await.expect("resync");
    sync_is(&s.b, SyncState::Synced).await;
    assert_eq!(s.b_provider.ids(), ["s1", "s2"]);
    assert!(!s.b_provider.playing());
}

#[tokio::test]
async fn reconnecting_resyncs_and_keeps_stepped_out() {
    let s = session(&[]).await;
    s.b.step_out().await.expect("step out");
    sync_is(&s.b, SyncState::SteppedOut).await;

    s.server.kill_connections();
    sync_is(&s.a, SyncState::Reconnecting).await;
    sync_is(&s.a, SyncState::Synced).await;
    sync_is(&s.b, SyncState::SteppedOut).await;

    s.a_provider
        .local(Op::PlaybackSetPlaying { is_playing: false });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        s.b_provider.playing(),
        "still stepped out after reconnecting"
    );
}

#[tokio::test]
async fn leaving_ends_the_runtime_but_not_the_provider() {
    let s = session(&[]).await;
    let status = s.b.watch_status();
    s.b.leave().await.expect("leave");
    assert_eq!(status.borrow().sync, SyncState::Ended);
    eventually("A sees B gone", || s.a.status().participants.len() == 1).await;
    // The provider is still usable by whoever owns it.
    assert!(s.b_provider.get_state().await.is_ok());
}
