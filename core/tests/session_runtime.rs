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

use attune_core::{
    Error, Result,
    bridge_controller::{ApplyOutcome, BridgeEvent, Interruption},
    network::{ReconnectPolicy, SessionClientConfig},
    protocol::{
        ParticipantStatus, QueueEntry, SessionMusicState, SessionOperation as Op, SessionPlayback,
        SessionSnapshot, TrackDescriptor, state,
    },
    session::{Notice, ProviderBridge, QueueEdit, SessionRuntime, SyncState},
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
    /// Operation types applied from the session, in order.
    applied: Mutex<Vec<String>>,
    /// prepare/restore calls, in order, with the settings passed around.
    session_settings: Mutex<Vec<String>>,
}

impl FakeProvider {
    fn new(ids: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(music(ids)),
            missing_titles: Mutex::default(),
            fail_next: AtomicBool::new(false),
            events: broadcast::channel(64).0,
            loads: Mutex::new(0),
            applied: Mutex::default(),
            session_settings: Mutex::default(),
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

    /// The music client is taken over by an advertisement, or released.
    fn interruption(&self, active: bool) {
        let _ = self.events.send(BridgeEvent::Interruption(Interruption {
            active,
            reason: "advertisement".to_owned(),
        }));
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
        let kind = serde_json::to_value(operation)
            .ok()
            .and_then(|value| value["type"].as_str().map(str::to_owned))
            .unwrap_or_default();
        self.applied.lock().unwrap().push(kind);
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

    async fn prepare_for_session(&self) -> Result<serde_json::Value> {
        self.session_settings
            .lock()
            .unwrap()
            .push("prepare".to_owned());
        Ok(serde_json::json!({ "shuffle": true }))
    }

    async fn restore_after_session(&self, previous: &serde_json::Value) -> Result<()> {
        self.session_settings
            .lock()
            .unwrap()
            .push(format!("restore {previous}"));
        Ok(())
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

    let session_id = s.a.credentials().code.clone();
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
    let session_id = s.a.credentials().code.clone();
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
async fn an_interruption_steps_out_and_rejoins_when_it_is_over() {
    let s = session(&[]).await;
    s.b_provider.interruption(true);
    sync_is(&s.b, SyncState::SteppedOut).await;

    // The session goes on without B.
    s.a_provider
        .local(Op::PlaybackSetPlaying { is_playing: false });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        s.b_provider.playing(),
        "not following during the interruption"
    );

    s.b_provider.interruption(false);
    sync_is(&s.b, SyncState::Synced).await;
    assert!(!s.b_provider.playing(), "caught up afterwards");
}

#[tokio::test]
async fn an_interruption_does_not_rejoin_someone_who_stepped_out() {
    let s = session(&[]).await;
    s.b.step_out().await.expect("step out");
    sync_is(&s.b, SyncState::SteppedOut).await;

    s.b_provider.interruption(true);
    s.b_provider.interruption(false);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.b.status().sync, SyncState::SteppedOut);
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

#[tokio::test]
async fn index_changes_are_translated_for_a_provider_missing_songs() {
    let s = session(&["s2"]).await; // B has [s1, s3]

    // Session [s1, s2, s3]: insert n1 at 3, which is position 2 for B.
    s.a_provider.local(Op::QueueInsert {
        entry: entry("n1"),
        index: 3,
    });
    eventually("B has n1 at the end", || {
        s.b_provider.ids() == ["s1", "s3", "n1"]
    })
    .await;

    // Session [s1, s2, s3, n1]: move n1 to 1, which is position 1 for B.
    s.a_provider.local(Op::QueueMove {
        entry_id: "n1".to_owned(),
        to_index: 1,
    });
    eventually("B moved n1 forward", || {
        s.b_provider.ids() == ["s1", "n1", "s3"]
    })
    .await;

    let applied = s.b_provider.applied.lock().unwrap().clone();
    assert_eq!(
        applied,
        ["queue.insert", "queue.move"],
        "no queue replacement (it restarts the song)"
    );
}

#[tokio::test]
async fn echoes_from_a_provider_missing_songs_are_recognised() {
    let s = session(&["s2"]).await; // B has [s1, s3]
    let session_id = s.a.credentials().code.clone();
    let sequence = s.server.sequence(&session_id);

    // B's provider reports its own queue, which lacks s2: that is the
    // session's queue, not an edit.
    s.b_provider.emit(Op::QueueReplace {
        entries: vec![entry("s1"), entry("s3")],
        current_entry_id: Some("s1".to_owned()),
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        s.server.sequence(&session_id),
        sequence,
        "nothing was published"
    );
    assert_eq!(s.a_provider.ids(), ["s1", "s2", "s3"]);
}

fn status_ids(runtime: &SessionRuntime) -> Vec<String> {
    runtime
        .status()
        .queue
        .into_iter()
        .map(|item| item.entry_id)
        .collect()
}

#[tokio::test]
async fn queue_edits_from_the_ui_reach_everyone_including_this_provider() {
    let s = session(&[]).await;
    s.b.edit_queue(QueueEdit::Move {
        entry_id: "s3".to_owned(),
        to_index: 1,
    })
    .await
    .expect("move");
    // Applied before edit_queue returns.
    assert_eq!(status_ids(&s.b), ["s1", "s3", "s2"]);
    eventually("B's provider moved it", || {
        s.b_provider.ids() == ["s1", "s3", "s2"]
    })
    .await;
    eventually("A's provider moved it", || {
        s.a_provider.ids() == ["s1", "s3", "s2"]
    })
    .await;
    assert_eq!(*s.b_provider.applied.lock().unwrap(), ["queue.move"]);

    s.a.edit_queue(QueueEdit::Remove {
        entry_id: "s2".to_owned(),
    })
    .await
    .expect("remove");
    eventually("both removed it", || {
        s.a_provider.ids() == ["s1", "s3"] && s.b_provider.ids() == ["s1", "s3"]
    })
    .await;
    // The providers' echoes of these edits are not published again.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.server.sequence(&s.a.credentials().code), 2);
}

#[tokio::test]
async fn songs_can_be_edited_while_stepped_out_or_missing_here() {
    let s = session(&["s2"]).await; // B has [s1, s3]
    assert!(!s.b.status().queue[1].playable_here);
    s.b.step_out().await.expect("step out");
    sync_is(&s.b, SyncState::SteppedOut).await;

    // B can't play s2 and isn't following, but can still remove it.
    s.b.edit_queue(QueueEdit::Remove {
        entry_id: "s2".to_owned(),
    })
    .await
    .expect("remove");
    assert_eq!(status_ids(&s.b), ["s1", "s3"]);
    eventually("A removed it", || s.a_provider.ids() == ["s1", "s3"]).await;
    assert!(
        s.b_provider.applied.lock().unwrap().is_empty(),
        "a stepped-out provider is left alone"
    );
}

#[tokio::test]
async fn an_edit_the_session_refuses_is_reported() {
    let s = session(&[]).await;
    let error =
        s.b.edit_queue(QueueEdit::Remove {
            entry_id: "gone".to_owned(),
        })
        .await
        .expect_err("unknown entry");
    assert!(
        matches!(error, Error::EditRejected { .. }),
        "unexpected {error}"
    );
    assert_eq!(status_ids(&s.b), ["s1", "s2", "s3"]);
}

fn settings_log(provider: &FakeProvider) -> Vec<String> {
    provider.session_settings.lock().unwrap().clone()
}

#[tokio::test]
async fn provider_settings_are_switched_off_for_a_session_and_restored_after() {
    let s = session(&[]).await;
    assert_eq!(settings_log(&s.a_provider), ["prepare"]);
    assert_eq!(settings_log(&s.b_provider), ["prepare"]);

    // Leaving restores what prepare returned.
    s.b.leave().await.expect("leave");
    assert_eq!(
        settings_log(&s.b_provider),
        ["prepare", r#"restore {"shuffle":true}"#]
    );

    // So does closing Attune without leaving (the handle is dropped).
    drop(s.a);
    eventually("A restored after being dropped", || {
        settings_log(&s.a_provider).len() == 2
    })
    .await;
}

#[tokio::test]
async fn a_failed_join_restores_the_settings_it_changed() {
    let server = TestServer::start().await;
    let provider = FakeProvider::new(&["s1"]);
    let code = attune_core::network::JoinCredentials::from_code("ZZZZ").expect("code");
    SessionRuntime::join(provider.clone(), config(&server, "b"), code)
        .await
        .err()
        .expect("no such session");
    assert_eq!(
        settings_log(&provider),
        ["prepare", r#"restore {"shuffle":true}"#]
    );
}

#[tokio::test]
async fn a_new_song_resumed_part_way_is_published_at_its_position() {
    // Spotify resumes an album or playlist where it was left; the session
    // starts a new current song at 0 unless it is told otherwise.
    let s = session(&[]).await;
    let session_id = s.a.credentials().code.clone();
    let mut resumed = music(&["n1", "n2"]);
    resumed.playback.position_ms = 48_000;
    resumed.playback.position_captured_at_ms = now_ms();
    *s.a_provider.state.lock().unwrap() = resumed.clone();
    s.a_provider.emit(Op::QueueReplace {
        entries: resumed.queue.clone(),
        current_entry_id: Some("n1".to_owned()),
    });

    let near = |position: u64| position.abs_diff(48_000) < 5_000;
    eventually("the session has the resumed position", || {
        let state = s.server.session_state(&session_id);
        state.current_entry_id.as_deref() == Some("n1")
            && near(state::position_at(&state, now_ms()))
    })
    .await;
    eventually("B plays n1 where A is", || {
        let state = s.b_provider.snapshot();
        state.current_entry_id.as_deref() == Some("n1")
            && near(state::position_at(&state, now_ms()))
    })
    .await;
}
