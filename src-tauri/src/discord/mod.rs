//! Discord Rich Presence: "Listening to TIDAL" (or whichever music app) and
//! the song on your Discord profile
//! while you're in a session, through the Discord desktop client's local
//! RPC connection (see `ipc`). No Discord account is linked and nothing
//! is sent anywhere else.
//!
//! It shows whatever your music app is playing, in a session or not (in
//! one, that's the session's song while you're hearing it). With **Listen
//! along** on, the activity also has a button for others that opens the
//! download page on the session's join code, while you're in one.
//!
//! One background task owns the connection. The app tells it what should
//! show (the session status, and whether the settings are on); the task
//! connects only while there is something to show, keeps the activity up
//! to date within Discord's rate limit, and disconnects (which clears it)
//! once there isn't. If Discord isn't running it tries again now and then.

mod activity;
mod ipc;

use std::{
    io,
    sync::{Arc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use attune_core::{
    bridge_controller::{BridgeController, BridgeEvent},
    protocol::SessionMusicState,
    session::{RuntimeStatus, SyncState},
};
use serde_json::Value;
use tauri::{AppHandle, Manager};
use tokio::{
    sync::{
        broadcast::{self, error::RecvError},
        mpsc, watch, Notify,
    },
    time::{sleep, sleep_until, timeout, Instant},
};
use tracing::{debug, warn};

use activity::Activity;
use ipc::Incoming;

/// The "Attune" application in the Discord Developer Portal. Public.
const CLIENT_ID: &str = "1570918885920604200";
/// Discord allows 5 activity updates per 20 s.
const MIN_UPDATE_GAP: Duration = Duration::from_secs(4);
/// How often to look for Discord while it isn't running.
const RETRY_EVERY: Duration = Duration::from_secs(20);
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// A song change arrives as a few events at once; read the app once after.
const SETTLE: Duration = Duration::from_millis(300);
/// Applying a session's change can take the music app a moment longer, and
/// it doesn't report those changes itself: read it again after this.
const SESSION_SETTLE: Duration = Duration::from_millis(1500);
/// Paused this long, the activity goes away until the music plays again.
const PAUSED_HIDE_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default)]
struct Wanted {
    enabled: bool,
    listen_along: bool,
    /// What the music app is playing.
    activity: Option<Activity>,
    /// The join code while you're in a session, for the Listen along button.
    code: Option<String>,
}

impl Wanted {
    fn shown(&self) -> Option<Activity> {
        if !self.enabled {
            return None;
        }
        let activity = self.activity.clone()?;
        Some(match &self.code {
            Some(code) if self.listen_along => activity.with_listen_along(code),
            _ => activity,
        })
    }
}

/// Handle to the presence task; managed as Tauri state.
pub struct DiscordPresence {
    wanted: watch::Sender<Wanted>,
    /// Tells the music app reader that the session changed something.
    session_changed: Arc<Notify>,
}

impl DiscordPresence {
    pub fn start(enabled: bool, listen_along: bool) -> Self {
        let (wanted, receiver) = watch::channel(Wanted {
            enabled,
            listen_along,
            activity: None,
            code: None,
        });
        tauri::async_runtime::spawn(run(receiver));
        Self {
            wanted,
            session_changed: Arc::new(Notify::new()),
        }
    }

    /// The session you're in, for the Listen along button, or `None` once
    /// you've left it. Each update may have changed what the music app
    /// plays, which it doesn't report itself, so it's read again.
    pub fn set_session(&self, status: Option<&RuntimeStatus>) {
        self.session_changed.notify_one();
        let code = status
            .filter(|status| status.sync != SyncState::Ended)
            .map(|status| status.session_id.clone());
        self.wanted.send_if_modified(|wanted| {
            let changed = wanted.code != code;
            wanted.code = code;
            changed
        });
    }

    /// What the music app `app` is playing, or with `None` nothing (it was
    /// disconnected).
    fn show_music_app(&self, state: Option<&SessionMusicState>, app: &str) {
        let next = state.and_then(|state| activity::from_state(state, app, now_ms()));
        self.wanted.send_if_modified(|wanted| {
            let same = match (&wanted.activity, &next) {
                (None, None) => true,
                (Some(a), Some(b)) => a.same_as(b),
                _ => false,
            };
            if !same {
                wanted.activity = next;
            }
            !same
        });
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.wanted.send_if_modified(|wanted| {
            let changed = wanted.enabled != enabled;
            wanted.enabled = enabled;
            changed
        });
    }

    pub fn set_listen_along(&self, listen_along: bool) {
        self.wanted.send_if_modified(|wanted| {
            let changed = wanted.listen_along != listen_along;
            wanted.listen_along = listen_along;
            changed
        });
    }
}

/// Keep Discord up to date with what `controller`'s music app plays, until
/// it's disconnected. Reads the app after each song change, play, pause or
/// seek it reports, and after each session update (the app keeps quiet
/// about changes a session makes). Holds it only weakly, so disconnecting
/// isn't held up.
pub fn follow_music_app(app: AppHandle, controller: &Arc<BridgeController>) {
    let mut events = controller.subscribe();
    let watched = Arc::downgrade(controller);
    let name = controller.provider_id().display_name();
    tauri::async_runtime::spawn(async move {
        let presence = app.state::<DiscordPresence>();
        let session_changed = presence.session_changed.clone();
        let mut pause = PauseClock::default();
        loop {
            if !read_music_app(&presence, &watched, name, &mut pause).await {
                break;
            }
            // Wake to hide it once it has been paused long enough.
            let hide_at = pause.hide_at().filter(|&at| at > Instant::now());
            match next_change(&mut events, &session_changed, hide_at).await {
                Change::App | Change::PausedLong => {}
                Change::Session => {
                    sleep(SESSION_SETTLE - SETTLE).await;
                    if !read_music_app(&presence, &watched, name, &mut pause).await {
                        break;
                    }
                }
                Change::Gone => break,
            }
        }
        presence.show_music_app(None, name);
    });
}

/// Show what the music app plays, or nothing once it has been paused for
/// [`PAUSED_HIDE_AFTER`]; `false` once it's gone.
async fn read_music_app(
    presence: &DiscordPresence,
    watched: &Weak<BridgeController>,
    name: &str,
    pause: &mut PauseClock,
) -> bool {
    let Some(controller) = watched.upgrade() else {
        return false;
    };
    let state = controller.get_session_state().await;
    drop(controller);
    match state {
        Ok(state) => {
            let now = Instant::now();
            pause.update(&state, now);
            let paused_long = pause.hide_at().is_some_and(|at| now >= at);
            presence.show_music_app((!paused_long).then_some(&state), name);
        }
        Err(error) => debug!(%error, "Could not read the music app for Discord"),
    }
    true
}

/// How long the music app has been paused on the same song.
#[derive(Debug, Default)]
struct PauseClock {
    /// The paused song (title and artists) and since when.
    since: Option<(String, Instant)>,
}

impl PauseClock {
    fn update(&mut self, state: &SessionMusicState, now: Instant) {
        let paused_on = state
            .queue
            .first()
            .filter(|_| !state.playback.is_playing)
            .map(|entry| format!("{}\n{}", entry.track.title, entry.track.artists.join("\n")));
        self.since = match (paused_on, self.since.take()) {
            (None, _) => None,
            (Some(song), Some((was, since))) if song == was => Some((was, since)),
            (Some(song), _) => Some((song, now)),
        };
    }

    /// When the activity should go away, while paused.
    fn hide_at(&self) -> Option<Instant> {
        self.since
            .as_ref()
            .map(|(_, since)| *since + PAUSED_HIDE_AFTER)
    }
}

enum Change {
    /// The music app reported a change.
    App,
    /// The session changed something, or might have.
    Session,
    /// Paused for [`PAUSED_HIDE_AFTER`].
    PausedLong,
    /// The music app is gone.
    Gone,
}

/// Wait for a change and, after [`SETTLE`], the rest of it.
async fn next_change(
    events: &mut broadcast::Receiver<BridgeEvent>,
    session_changed: &Notify,
    hide_at: Option<Instant>,
) -> Change {
    let change = loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(
                    BridgeEvent::Ready
                    | BridgeEvent::PlaystateChange(_)
                    | BridgeEvent::QueueChange(_),
                ) => break Change::App,
                Ok(BridgeEvent::Closed) | Err(RecvError::Closed) => return Change::Gone,
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
            },
            () = session_changed.notified() => break Change::Session,
            () = sleep_until(hide_at.unwrap_or_else(Instant::now)), if hide_at.is_some() => {
                return Change::PausedLong;
            }
        }
    };
    sleep(SETTLE).await;
    loop {
        match events.try_recv() {
            Ok(BridgeEvent::Closed) | Err(broadcast::error::TryRecvError::Closed) => {
                return Change::Gone;
            }
            Err(broadcast::error::TryRecvError::Empty) => return change,
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

async fn run(mut wanted: watch::Receiver<Wanted>) {
    loop {
        // Nothing to show: no reason to talk to Discord.
        if wanted
            .wait_for(|wanted| wanted.shown().is_some())
            .await
            .is_err()
        {
            return;
        }
        match serve(&mut wanted).await {
            Ok(()) => continue,
            Err(error) => debug!(%error, "Discord presence unavailable"),
        }
        // Discord isn't running (or went away): look again later, or as
        // soon as there's something else to show.
        tokio::select! {
            _ = sleep(RETRY_EVERY) => {}
            changed = wanted.changed() => if changed.is_err() { return },
        }
    }
}

/// One connection to Discord, kept while there's something to show.
/// `Ok` once there's nothing to show any more (the activity is cleared).
async fn serve(wanted: &mut watch::Receiver<Wanted>) -> io::Result<()> {
    let pipe = ipc::open().await?;
    let (mut reader, mut writer) = tokio::io::split(pipe);

    // Frames are read on a task of their own: a half-read frame must not be
    // dropped when the loop below wakes for something else.
    let (frames, mut incoming) = mpsc::channel::<io::Result<Incoming>>(16);
    let reading = tauri::async_runtime::spawn(async move {
        loop {
            let frame = ipc::read_frame(&mut reader).await;
            let failed = frame.is_err();
            if frames.send(frame).await.is_err() || failed {
                break;
            }
        }
    });
    let _stop_reading = AbortOnDrop(reading);

    ipc::send_handshake(&mut writer, CLIENT_ID).await?;
    timeout(READY_TIMEOUT, async {
        loop {
            match incoming.recv().await {
                Some(Ok(Incoming::Frame(frame))) if frame["evt"] == "READY" => return Ok(()),
                Some(Ok(Incoming::Close(reason))) => return Err(closed(&reason)),
                Some(Ok(_)) => continue,
                Some(Err(error)) => return Err(error),
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no READY from Discord"))??;
    debug!("Connected to Discord");

    let mut sent: Option<Activity> = None;
    let mut next_update = Instant::now();
    let mut nonce = 0u64;
    loop {
        let Some(want) = wanted.borrow().shown() else {
            nonce += 1;
            ipc::send_activity(&mut writer, None, nonce).await?;
            return Ok(());
        };
        let due = sent.as_ref().is_none_or(|sent| !sent.same_as(&want));
        if due && Instant::now() >= next_update {
            nonce += 1;
            let payload = serde_json::to_value(&want)?;
            ipc::send_activity(&mut writer, Some(&payload), nonce).await?;
            sent = Some(want);
            next_update = Instant::now() + MIN_UPDATE_GAP;
            continue;
        }
        tokio::select! {
            changed = wanted.changed() => if changed.is_err() { return Ok(()) },
            message = incoming.recv() => match message {
                Some(Ok(Incoming::Ping(payload))) => ipc::send_pong(&mut writer, &payload).await?,
                Some(Ok(Incoming::Frame(frame))) => report(&frame),
                Some(Ok(Incoming::Close(reason))) => return Err(closed(&reason)),
                Some(Err(error)) => return Err(error),
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            },
            // Waiting out the rate limit before an update.
            _ = sleep_until(next_update), if due => {}
        }
    }
}

/// Log Discord's refusals (a malformed activity, say).
fn report(frame: &Value) {
    if frame["evt"] == "ERROR" {
        warn!(code = %frame["data"]["code"], message = %frame["data"]["message"], "Discord refused the activity");
    }
}

fn closed(reason: &Value) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        format!("Discord closed the connection: {reason}"),
    )
}

struct AbortOnDrop(tauri::async_runtime::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use attune_core::protocol::{QueueEntry, SessionPlayback, TrackDescriptor};

    fn state(title: &str, is_playing: bool) -> SessionMusicState {
        SessionMusicState {
            queue: vec![QueueEntry {
                entry_id: "e1".to_owned(),
                track: TrackDescriptor {
                    title: title.to_owned(),
                    artists: vec!["Paper Satellites".to_owned()],
                    isrc: None,
                    album: None,
                    duration_ms: Some(240_000),
                    cover_url: None,
                },
                added_by: None,
            }],
            current_entry_id: Some("e1".to_owned()),
            playback: SessionPlayback {
                is_playing,
                position_ms: 0,
                position_captured_at_ms: 0,
            },
            repeat: Default::default(),
        }
    }

    #[test]
    fn hides_a_minute_into_a_pause_on_the_same_song() {
        let start = Instant::now();
        let mut pause = PauseClock::default();
        pause.update(&state("A", true), start);
        assert_eq!(pause.hide_at(), None);

        pause.update(&state("A", false), start);
        let hide_at = start + PAUSED_HIDE_AFTER;
        assert_eq!(pause.hide_at(), Some(hide_at));
        // Reading again while paused keeps the clock.
        pause.update(&state("A", false), start + Duration::from_secs(30));
        assert_eq!(pause.hide_at(), Some(hide_at));
        // Another song, still paused, starts it again.
        let later = start + Duration::from_secs(40);
        pause.update(&state("B", false), later);
        assert_eq!(pause.hide_at(), Some(later + PAUSED_HIDE_AFTER));
        // Playing stops it.
        pause.update(&state("B", true), later);
        assert_eq!(pause.hide_at(), None);
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use serde_json::json;

    /// Against the Discord client on this computer: sets a sample activity
    /// for a few seconds, prints Discord's reply, and clears it. Shows on
    /// your profile meanwhile.
    ///
    ///   cargo test -p attune --lib discord::live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs the Discord desktop client running"]
    async fn sets_and_clears_an_activity() {
        let pipe = ipc::open().await.expect("Discord isn't running");
        let (mut reader, mut writer) = tokio::io::split(pipe);
        ipc::send_handshake(&mut writer, CLIENT_ID).await.unwrap();
        let Incoming::Frame(ready) = ipc::read_frame(&mut reader).await.unwrap() else {
            panic!("expected READY");
        };
        assert_eq!(ready["evt"], "READY", "{ready}");
        println!("READY as {}", ready["data"]["user"]["username"]);

        let now = now_ms();
        let activity = Activity {
            name: "TIDAL".to_owned(),
            kind: 2,
            status_display_type: 1,
            details: "Attune presence test".to_owned(),
            state: Some("Paper Satellites".to_owned()),
            timestamps: Some(activity::Timestamps {
                start: now - 30_000,
                end: Some(now + 90_000),
            }),
            assets: None,
            buttons: Vec::new(),
        }
        .with_listen_along("KWPZ");
        let payload = serde_json::to_value(&activity).unwrap();
        ipc::send_activity(&mut writer, Some(&payload), 1)
            .await
            .unwrap();
        let Incoming::Frame(reply) = ipc::read_frame(&mut reader).await.unwrap() else {
            panic!("expected a reply");
        };
        println!("{}", serde_json::to_string_pretty(&reply).unwrap());
        assert_ne!(reply["evt"], json!("ERROR"), "{reply}");

        sleep(Duration::from_secs(3)).await;
        ipc::send_activity(&mut writer, None, 2).await.unwrap();
        let _ = ipc::read_frame(&mut reader).await;
    }
}
