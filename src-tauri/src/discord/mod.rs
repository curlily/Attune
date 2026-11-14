//! Discord Rich Presence: "Listening to TIDAL" (or whichever music app) and
//! the song on your Discord profile
//! while you're in a session, through the Discord desktop client's local
//! RPC connection (see `ipc`). No Discord account is linked and nothing
//! is sent anywhere else.
//!
//! It shows the session's song while you're hearing it, and otherwise
//! whatever your music app is playing (outside a session, or stepped out).
//! With **Listen along** on, the activity also has a button for others that
//! opens the download page on the session's join code, while you're in one.
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
    sync::Arc,
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
        mpsc, watch,
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

#[derive(Debug, Clone, Default)]
struct Wanted {
    enabled: bool,
    listen_along: bool,
    /// The session's song, while you're hearing it.
    session: Option<Activity>,
    /// What the music app is playing, shown when the session's song isn't.
    music_app: Option<Activity>,
    /// The join code while you're in a session, for the Listen along button.
    code: Option<String>,
}

impl Wanted {
    fn shown(&self) -> Option<Activity> {
        if !self.enabled {
            return None;
        }
        let activity = self.session.clone().or_else(|| self.music_app.clone())?;
        Some(match &self.code {
            Some(code) if self.listen_along => activity.with_listen_along(code),
            _ => activity,
        })
    }
}

/// Handle to the presence task; managed as Tauri state.
pub struct DiscordPresence {
    wanted: watch::Sender<Wanted>,
}

impl DiscordPresence {
    pub fn start(enabled: bool, listen_along: bool) -> Self {
        let (wanted, receiver) = watch::channel(Wanted {
            enabled,
            listen_along,
            session: None,
            music_app: None,
            code: None,
        });
        tauri::async_runtime::spawn(run(receiver));
        Self { wanted }
    }

    /// Follow the session (played on `app`), or with `None` leave it.
    pub fn show_status(&self, status: Option<&RuntimeStatus>, app: &str) {
        let next = status.and_then(|status| activity::from_status(status, app, now_ms()));
        let code = status
            .filter(|status| status.sync != SyncState::Ended)
            .map(|status| status.session_id.clone());
        self.wanted.send_if_modified(|wanted| {
            let same = wanted.code == code && same(&wanted.session, &next);
            if !same {
                wanted.session = next;
                wanted.code = code;
            }
            !same
        });
    }

    /// What the music app `app` is playing, or with `None` nothing (it was
    /// disconnected).
    fn show_music_app(&self, state: Option<&SessionMusicState>, app: &str) {
        let next = state.and_then(|state| activity::from_music_app(state, app, now_ms()));
        self.wanted.send_if_modified(|wanted| {
            let same = same(&wanted.music_app, &next);
            if !same {
                wanted.music_app = next;
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

fn same(a: &Option<Activity>, b: &Option<Activity>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.same_as(b),
        _ => false,
    }
}

/// Keep Discord up to date with what `controller`'s music app plays, until
/// it's disconnected. Reads the app after each song change, play, pause or
/// seek it reports; holds it only weakly, so disconnecting isn't held up.
pub fn follow_music_app(app: AppHandle, controller: &Arc<BridgeController>) {
    let mut events = controller.subscribe();
    let watched = Arc::downgrade(controller);
    let name = controller.provider_id().display_name();
    tauri::async_runtime::spawn(async move {
        let presence = app.state::<DiscordPresence>();
        loop {
            let Some(controller) = watched.upgrade() else {
                break;
            };
            let state = controller.get_session_state().await;
            drop(controller);
            match state {
                Ok(state) => presence.show_music_app(Some(&state), name),
                Err(error) => debug!(%error, "Could not read the music app for Discord"),
            }
            if !next_change(&mut events).await {
                break;
            }
        }
        presence.show_music_app(None, name);
    });
}

/// Wait for the music app to report a change and the rest of it to arrive.
/// `false` once the app is gone.
async fn next_change(events: &mut broadcast::Receiver<BridgeEvent>) -> bool {
    loop {
        match events.recv().await {
            Ok(
                BridgeEvent::Ready | BridgeEvent::PlaystateChange(_) | BridgeEvent::QueueChange(_),
            ) => break,
            Ok(BridgeEvent::Closed) | Err(RecvError::Closed) => return false,
            Ok(_) | Err(RecvError::Lagged(_)) => continue,
        }
    }
    sleep(SETTLE).await;
    loop {
        match events.try_recv() {
            Ok(BridgeEvent::Closed) | Err(broadcast::error::TryRecvError::Closed) => return false,
            Err(broadcast::error::TryRecvError::Empty) => return true,
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
            status_display_type: 2,
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
