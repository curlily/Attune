//! Discord: "Listening to <song>" on your profile while you're in a
//! session, and joining a friend's session from Discord. Everything goes
//! through the Discord desktop client's local RPC connection (see `ipc`);
//! no Discord account is linked and nothing is sent anywhere else.
//!
//! One background task owns the connection, kept open while Discord runs:
//!
//! - It shows what the app wants shown (the session's song, if the setting
//!   is on), within Discord's rate limit, and clears it otherwise.
//! - It subscribes to `ACTIVITY_JOIN` (you pressed Join, or accepted an
//!   invite: here is the session's secret) and `ACTIVITY_JOIN_REQUEST` (a
//!   friend asks to join yours), and passes them to the app as
//!   [`DiscordEvent`]s. Discord starts Attune through the
//!   `discord-<application ID>` link when it isn't running, so a join is
//!   delivered once this connects.
//! - It sends your answers to join requests back to Discord.

mod activity;
mod ipc;

use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use attune_core::{
    protocol::{is_invite_secret, SessionInvite},
    session::RuntimeStatus,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{
    sync::{mpsc, watch},
    time::{sleep, sleep_until, timeout, Instant},
};
use tracing::{debug, info, warn};

use activity::Activity;
use ipc::Incoming;

/// The "Attune" application in the Discord Developer Portal. Public.
pub const CLIENT_ID: &str = "1570918885920604200";
/// Discord allows 5 activity updates per 20 s.
const MIN_UPDATE_GAP: Duration = Duration::from_secs(4);
/// How often to look for Discord while it isn't running.
const RETRY_EVERY: Duration = Duration::from_secs(20);
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Something Discord asks of the app.
#[derive(Debug, Clone)]
pub enum DiscordEvent {
    /// Join the session behind this invite secret.
    Join { secret: String },
    /// A friend asks to join your session.
    JoinRequest(JoinRequest),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    pub user_id: String,
    pub name: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug)]
enum Command {
    Answer { user_id: String, accept: bool },
}

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

/// Handle to the Discord task; managed as Tauri state.
pub struct DiscordPresence {
    wanted: watch::Sender<Wanted>,
    commands: mpsc::UnboundedSender<Command>,
}

impl DiscordPresence {
    /// Start the task. `on_event` is called for joins and join requests.
    pub fn start(enabled: bool, on_event: impl Fn(DiscordEvent) + Send + Sync + 'static) -> Self {
        let (wanted, receiver) = watch::channel(Wanted {
            enabled,
            activity: None,
        });
        let (commands, command_receiver) = mpsc::unbounded_channel();
        tauri::async_runtime::spawn(run(receiver, command_receiver, on_event));
        Self { wanted, commands }
    }

    /// Show the session's song (with its invite, friends can ask to join),
    /// or with `None` (no session) nothing.
    pub fn show_status(&self, status: Option<&RuntimeStatus>, invite: Option<&SessionInvite>) {
        let next = status.and_then(|status| activity::from_status(status, invite, now_ms()));
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

    /// Accept or decline a friend's request to join.
    pub fn answer(&self, user_id: String, accept: bool) {
        let _ = self.commands.send(Command::Answer { user_id, accept });
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

async fn run(
    mut wanted: watch::Receiver<Wanted>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    on_event: impl Fn(DiscordEvent),
) {
    loop {
        match serve(&mut wanted, &mut commands, &on_event).await {
            Ok(()) => return,
            Err(error) => debug!(%error, "Discord unavailable"),
        }
        // Discord isn't running (or went away): look again later.
        tokio::select! {
            _ = sleep(RETRY_EVERY) => {}
            changed = wanted.changed() => if changed.is_err() { return },
        }
    }
}

/// One connection to Discord, kept until it fails. `Ok` only when the app
/// is shutting down.
async fn serve(
    wanted: &mut watch::Receiver<Wanted>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    on_event: &impl Fn(DiscordEvent),
) -> io::Result<()> {
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

    let mut nonce = 0u64;
    let mut next_nonce = || {
        nonce += 1;
        nonce.to_string()
    };
    for event in ["ACTIVITY_JOIN", "ACTIVITY_JOIN_REQUEST"] {
        let subscribe = json!({ "cmd": "SUBSCRIBE", "evt": event, "nonce": next_nonce() });
        ipc::write_frame(&mut writer, ipc::FRAME, &subscribe).await?;
    }

    // Discord starts each connection with nothing shown.
    let mut sent: Option<Activity> = None;
    let mut next_update = Instant::now();
    loop {
        let want = wanted.borrow().shown();
        let due = match (&sent, &want) {
            (None, None) => false,
            (Some(sent), Some(want)) => !sent.same_as(want),
            _ => true,
        };
        if due && Instant::now() >= next_update {
            let payload = want.as_ref().map(serde_json::to_value).transpose()?;
            ipc::send_activity(&mut writer, payload.as_ref(), &next_nonce()).await?;
            sent = want;
            next_update = Instant::now() + MIN_UPDATE_GAP;
            continue;
        }
        tokio::select! {
            changed = wanted.changed() => if changed.is_err() { return Ok(()) },
            command = commands.recv() => match command {
                Some(Command::Answer { user_id, accept }) => {
                    let cmd = if accept { "SEND_ACTIVITY_JOIN_INVITE" } else { "CLOSE_ACTIVITY_JOIN_REQUEST" };
                    let answer = json!({ "cmd": cmd, "args": { "user_id": user_id }, "nonce": next_nonce() });
                    ipc::write_frame(&mut writer, ipc::FRAME, &answer).await?;
                }
                None => return Ok(()),
            },
            message = incoming.recv() => match message {
                Some(Ok(Incoming::Ping(payload))) => ipc::send_pong(&mut writer, &payload).await?,
                Some(Ok(Incoming::Frame(frame))) => dispatch(&frame, on_event),
                Some(Ok(Incoming::Close(reason))) => return Err(closed(&reason)),
                Some(Err(error)) => return Err(error),
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            },
            // Waiting out the rate limit before an update.
            _ = sleep_until(next_update), if due => {}
        }
    }
}

/// Pass Discord's events on to the app, and log its refusals.
fn dispatch(frame: &Value, on_event: &impl Fn(DiscordEvent)) {
    if frame["cmd"] != "DISPATCH" {
        if frame["evt"] == "ERROR" {
            warn!(cmd = %frame["cmd"], code = %frame["data"]["code"], message = %frame["data"]["message"], "Discord refused a command");
        }
        return;
    }
    if let Some(event) = event_from(frame) {
        info!(kind = ?std::mem::discriminant(&event), "Discord event");
        on_event(event);
    }
}

fn event_from(frame: &Value) -> Option<DiscordEvent> {
    let data = &frame["data"];
    match frame["evt"].as_str()? {
        "ACTIVITY_JOIN" => {
            let secret = data["secret"].as_str()?;
            // Only ever an invite of ours.
            is_invite_secret(secret).then(|| DiscordEvent::Join {
                secret: secret.to_owned(),
            })
        }
        "ACTIVITY_JOIN_REQUEST" => {
            let user = &data["user"];
            let user_id = user["id"].as_str()?.to_owned();
            if user_id.is_empty() || !user_id.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let name = user["global_name"]
                .as_str()
                .or_else(|| user["username"].as_str())
                .unwrap_or("Someone")
                .to_owned();
            let avatar_url = user["avatar"]
                .as_str()
                .filter(|hash| {
                    hash.bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                })
                .map(|hash| {
                    format!("https://cdn.discordapp.com/avatars/{user_id}/{hash}.png?size=64")
                });
            Some(DiscordEvent::JoinRequest(JoinRequest {
                user_id,
                name,
                avatar_url,
            }))
        }
        _ => None,
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

    #[test]
    fn join_events_carry_only_invite_secrets() {
        let join = |secret: &str| {
            event_from(
                &json!({ "cmd": "DISPATCH", "evt": "ACTIVITY_JOIN", "data": { "secret": secret } }),
            )
        };
        let secret = "ab".repeat(32);
        assert!(matches!(join(&secret), Some(DiscordEvent::Join { secret: s }) if s == secret));
        assert!(join("KWPZ").is_none());
        assert!(join("not a secret").is_none());
    }

    #[test]
    fn join_requests_name_the_friend() {
        let request = event_from(&json!({
            "cmd": "DISPATCH",
            "evt": "ACTIVITY_JOIN_REQUEST",
            "data": { "user": { "id": "80351110224678912", "username": "sam", "global_name": "Sam", "avatar": "a_1b2c" } }
        }));
        let Some(DiscordEvent::JoinRequest(request)) = request else {
            panic!("expected a join request");
        };
        assert_eq!(request.name, "Sam");
        assert_eq!(
            request.avatar_url.as_deref(),
            Some("https://cdn.discordapp.com/avatars/80351110224678912/a_1b2c.png?size=64")
        );
        // Not a Discord user ID: ignored rather than passed on.
        let odd = json!({ "cmd": "DISPATCH", "evt": "ACTIVITY_JOIN_REQUEST", "data": { "user": { "id": "../x" } } });
        assert!(event_from(&odd).is_none());
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Against the Discord client on this computer: sets a sample activity
    /// with a party and join secret for a few seconds, prints Discord's
    /// reply, and clears it. Shows on your profile meanwhile.
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
            party: Some(activity::Party {
                id: "attune-live-test".to_owned(),
                size: [2, 16],
                privacy: 0,
            }),
            secrets: Some(activity::Secrets {
                join: "0".repeat(64),
            }),
        };
        let payload = serde_json::to_value(&activity).unwrap();
        ipc::send_activity(&mut writer, Some(&payload), "1")
            .await
            .unwrap();
        let Incoming::Frame(reply) = ipc::read_frame(&mut reader).await.unwrap() else {
            panic!("expected a reply");
        };
        println!("{}", serde_json::to_string_pretty(&reply).unwrap());
        assert_ne!(reply["evt"], json!("ERROR"), "{reply}");

        sleep(Duration::from_secs(3)).await;
        ipc::send_activity(&mut writer, None, "2").await.unwrap();
        let _ = ipc::read_frame(&mut reader).await;
    }
}
