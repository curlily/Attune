//! Discord Rich Presence: "Listening to <song>" on your Discord profile
//! while you're in a session, through the Discord desktop client's local
//! RPC connection (see `ipc`). No Discord account is linked and nothing
//! is sent anywhere else.
//!
//! One background task owns the connection. The app tells it what should
//! show (the session status, and whether the setting is on); the task
//! connects only while there is something to show, keeps the activity up
//! to date within Discord's rate limit, and disconnects (which clears it)
//! once there isn't. If Discord isn't running it tries again now and then.

mod activity;
mod ipc;

use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use attune_core::session::RuntimeStatus;
use serde_json::Value;
use tokio::{
    sync::{mpsc, watch},
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

#[derive(Debug, Clone, Default)]
struct Wanted {
    enabled: bool,
    activity: Option<Activity>,
}

impl Wanted {
    fn shown(&self) -> Option<Activity> {
        self.activity.clone().filter(|_| self.enabled)
    }
}

/// Handle to the presence task; managed as Tauri state.
pub struct DiscordPresence {
    wanted: watch::Sender<Wanted>,
}

impl DiscordPresence {
    pub fn start(enabled: bool) -> Self {
        let (wanted, receiver) = watch::channel(Wanted {
            enabled,
            activity: None,
        });
        tauri::async_runtime::spawn(run(receiver));
        Self { wanted }
    }

    /// Show the session's song, or with `None` (no session) nothing.
    pub fn show_status(&self, status: Option<&RuntimeStatus>) {
        let next = status.and_then(|status| activity::from_status(status, now_ms()));
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
            kind: 2,
            status_display_type: 2,
            details: "Attune presence test".to_owned(),
            state: Some("Paper Satellites".to_owned()),
            timestamps: Some(activity::Timestamps {
                start: now - 30_000,
                end: Some(now + 90_000),
            }),
            assets: None,
            buttons: vec![activity::Button {
                label: "Get Attune".to_owned(),
                url: "https://curlily.github.io/Attune/".to_owned(),
            }],
        };
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
