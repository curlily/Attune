//! Notifications drawn over other apps: a small click-through pill in a
//! corner of the screen, for when Attune runs in the background (a game in
//! front, say) and something happens in the session.
//!
//! Every event shows the moment it arrives and replaces whatever is on
//! screen: nothing waits, groups or queues. In persistent mode the pill
//! stays between events, showing an ongoing state or else the song
//! ([`rest_for`]). What to say is decided here (`label_*`, `rest_for`,
//! `should_show`, pure and tested); where and how in [`overlay`], and
//! what's in front of the user in [`screen`].

pub mod overlay;
mod screen;

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use attune_core::session::{Activity, Notice, RuntimeStatus, SyncState};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

use crate::{
    settings::{OverlayMode, OverlaySettings, ShowWhen},
    state::AppState,
};

pub use overlay::{close_overlay, open_overlay};
pub use screen::Foreground;

/// Which overlay setting lets an event through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Playback,
    People,
    Sync,
    Songs,
}

/// How the pill's icon is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    Info,
    Success,
    Warning,
    Danger,
}

/// One notification: an icon (a name from Icon.svelte) and one line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Flash {
    pub icon: &'static str,
    pub text: String,
    pub tone: Tone,
    /// Album art shown in place of the icon (song changes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
}

impl Flash {
    fn new(icon: &'static str, text: impl Into<String>, tone: Tone) -> Self {
        Self {
            icon,
            text: text.into(),
            tone,
            cover: None,
        }
    }
}

/// "Title \u{2014} Artist, Artist", or the title alone.
fn song_line(track: &attune_core::protocol::TrackDescriptor) -> String {
    if track.artists.is_empty() {
        track.title.clone()
    } else {
        format!("{} \u{2014} {}", track.title, track.artists.join(", "))
    }
}

/// The song card for a change of song: `seen` is the session and entry
/// shown last (`None`: nothing yet). The first song of a session isn't a
/// change; neither is an empty queue.
pub fn song_change(seen: Option<&(String, String)>, status: &RuntimeStatus) -> Option<Flash> {
    let current = status.queue.first()?;
    let (session, entry) = seen?;
    if *session != status.session_id || *entry == current.entry_id {
        return None;
    }
    Some(Flash {
        cover: current.track.cover_url.clone(),
        ..Flash::new("music", song_line(&current.track), Tone::Info)
    })
}

/// What persistent mode shows between events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rest {
    pub flash: Flash,
    /// It's the song (not a state), which may be shown faded.
    pub song: bool,
}

/// Persistent mode's pill between events: an ongoing state if there is one
/// (and the Sync group is on), else what's playing. `interrupted`: why the
/// music app stepped out on its own ("advertisement", "preview").
pub fn rest_for(status: &RuntimeStatus, interrupted: Option<&str>, sync_on: bool) -> Option<Rest> {
    let state = |icon, text: &str, tone| Rest {
        flash: Flash::new(icon, text, tone),
        song: false,
    };
    if sync_on {
        match status.sync {
            SyncState::Resyncing => return Some(state("refresh", "Catching up", Tone::Info)),
            SyncState::Reconnecting => {
                return Some(state("cloudOff", "Reconnecting", Tone::Warning))
            }
            SyncState::SteppedOut => {
                let text = match interrupted {
                    Some("preview") => "Stepped out: previews only",
                    Some(_) => "Stepped out: ad",
                    None => "Stepped out",
                };
                return Some(state("eject", text, Tone::Info));
            }
            SyncState::WaitingForNextSong => {
                return Some(state("clock", "Waiting for the next song", Tone::Warning))
            }
            SyncState::Error => return Some(state("alert", "Out of sync", Tone::Danger)),
            SyncState::Synced | SyncState::Ended => {}
        }
    }
    if status.sync == SyncState::Ended {
        return None;
    }
    let current = status.queue.first()?;
    let text = song_line(&current.track);
    let icon = if status.playback.is_playing {
        "play"
    } else {
        "pause"
    };
    Some(Rest {
        flash: Flash::new(icon, text, Tone::Info),
        song: true,
    })
}

/// Why the music app last stepped out on its own, while it still is.
static INTERRUPTED: Mutex<Option<String>> = Mutex::new(None);

fn interrupted() -> std::sync::MutexGuard<'static, Option<String>> {
    INTERRUPTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// "Paused by Lily", or "Paused" for a change without a name.
fn by(action: &str, by: &Option<String>) -> String {
    match by {
        Some(name) => format!("{action} by {name}"),
        None => action.to_owned(),
    }
}

fn someone(by: &Option<String>) -> &str {
    by.as_deref().unwrap_or("Someone")
}

pub fn label_activity(activity: &Activity) -> (Group, Flash) {
    use Activity::*;
    let info = |icon, text: String| Flash::new(icon, text, Tone::Info);
    match activity {
        Paused { by: who } => (Group::Playback, info("pause", by("Paused", who))),
        Resumed { by: who } => (Group::Playback, info("play", by("Resumed", who))),
        Skipped { by: None } => (
            Group::Playback,
            info("skip", "Skipped: nobody can play it".to_owned()),
        ),
        Skipped { by: who } => (Group::Playback, info("skip", by("Skipped", who))),
        Seeked { by: who } => (Group::Playback, info("seek", by("Seeked", who))),
        Played { by: who, title } => (
            Group::Playback,
            info("play", format!("{} played “{title}”", someone(who))),
        ),
        Added { by: who, title } => (
            Group::Playback,
            info("plus", format!("{} added “{title}”", someone(who))),
        ),
        Removed { by: who, title } => (
            Group::Playback,
            info("trash", format!("{} removed “{title}”", someone(who))),
        ),
        QueueChanged { by: who } => (
            Group::Playback,
            info("list", format!("{} changed the queue", someone(who))),
        ),
        Repeat { by: who, on } => (
            Group::Playback,
            info(
                "repeat",
                format!(
                    "{} turned repeat {}",
                    someone(who),
                    if *on { "on" } else { "off" }
                ),
            ),
        ),
        Joined { name } => (Group::People, info("userPlus", format!("{name} joined"))),
        Left { name } => (Group::People, info("userMinus", format!("{name} left"))),
        NewHost { you: true, .. } => (Group::People, info("crown", "You're the host".to_owned())),
        NewHost { name, .. } => (Group::People, info("crown", format!("{name} is the host"))),
    }
}

/// A change of this client's sync state worth telling. Back to `Synced`
/// comes as [`Notice::Resynced`]; `Ended` as [`Notice::Ended`].
pub fn label_sync(from: SyncState, to: SyncState) -> Option<Flash> {
    if from == to {
        return None;
    }
    Some(match to {
        SyncState::Resyncing => Flash::new("refresh", "Catching up", Tone::Info),
        SyncState::Reconnecting => Flash::new("cloudOff", "Reconnecting", Tone::Warning),
        SyncState::SteppedOut => Flash::new("eject", "Stepped out", Tone::Info),
        SyncState::WaitingForNextSong => {
            Flash::new("clock", "Waiting for the next song", Tone::Warning)
        }
        SyncState::Error => Flash::new("alert", "Out of sync", Tone::Danger),
        SyncState::Synced | SyncState::Ended => return None,
    })
}

pub fn label_notice(notice: &Notice) -> Option<Flash> {
    Some(match notice {
        Notice::Interrupted { reason } if reason == "preview" => {
            Flash::new("eject", "Stepped out: previews only", Tone::Warning)
        }
        Notice::Interrupted { .. } => Flash::new("eject", "Stepped out: ad", Tone::Warning),
        Notice::NotAllowed { host } => Flash::new(
            "alert",
            format!("Only {host} can change what's playing"),
            Tone::Warning,
        ),
        Notice::SongUnavailable { title } => Flash::new(
            "alert",
            format!("“{title}” isn't available here"),
            Tone::Warning,
        ),
        Notice::QueueEditUndone => {
            Flash::new("alert", "Your queue change was undone", Tone::Warning)
        }
        Notice::Resynced => Flash::new("check", "Back in sync", Tone::Success),
        Notice::Desynchronized { .. } => Flash::new("alert", "Out of sync", Tone::Danger),
        // Leaving was the user's own doing.
        Notice::Ended { reason } if reason == "left" || reason == "closed" => return None,
        Notice::Ended { .. } => Flash::new("logout", "The session has ended", Tone::Info),
        Notice::ServerError { .. } => return None,
    })
}

/// Whether to draw over what's in front right now. `attune_shown`: the
/// main window is open (not in the tray or minimised).
pub fn should_show(settings: &OverlaySettings, front: Foreground, attune_shown: bool) -> bool {
    if !settings.enabled || front == Foreground::Attune {
        return false;
    }
    match settings.show_when {
        ShowWhen::FullScreen => front == Foreground::FullScreen,
        ShowWhen::Hidden => front == Foreground::FullScreen || !attune_shown,
        ShowWhen::Always => true,
    }
}

fn group_on(settings: &OverlaySettings, group: Group) -> bool {
    match group {
        Group::Playback => settings.playback,
        Group::People => settings.people,
        Group::Sync => settings.sync,
        Group::Songs => settings.songs && settings.mode == OverlayMode::Flash,
    }
}

/// Show `flash` if the settings and what's on screen allow it. Returns
/// whether it was shown.
fn offer<R: Runtime>(app: &AppHandle<R>, group: Group, flash: Flash) -> bool {
    let Some(state) = app.try_state::<AppState>() else {
        return false;
    };
    let settings = state.settings().overlay.clone();
    if !group_on(&settings, group) {
        return false;
    }
    let front = screen::foreground(app);
    if !should_show(&settings, front, screen::attune_shown(app)) {
        return false;
    }
    overlay::show(app, &settings, front, flash);
    true
}

/// After someone skips or picks a song, its notification says so; the
/// song card that would follow at once would only wipe it out.
const SONG_ACTION_COVERS: Duration = Duration::from_secs(2);
static LAST_SONG_ACTION: Mutex<Option<Instant>> = Mutex::new(None);
/// The session and entry the song card last saw.
static SEEN_SONG: Mutex<Option<(String, String)>> = Mutex::new(None);

fn locked<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn on_activity<R: Runtime>(app: &AppHandle<R>, activity: &Activity) {
    if matches!(activity, Activity::Skipped { .. } | Activity::Played { .. }) {
        *locked(&LAST_SONG_ACTION) = Some(Instant::now());
    }
    let (group, flash) = label_activity(activity);
    offer(app, group, flash);
}

fn on_song<R: Runtime>(app: &AppHandle<R>, status: &RuntimeStatus) {
    let card = {
        let mut seen = locked(&SEEN_SONG);
        let card = song_change(seen.as_ref(), status);
        if let Some(current) = status.queue.first() {
            *seen = Some((status.session_id.clone(), current.entry_id.clone()));
        }
        card
    };
    let told = locked(&LAST_SONG_ACTION).is_some_and(|at| at.elapsed() < SONG_ACTION_COVERS);
    if let Some(card) = card.filter(|_| !told) {
        offer(app, Group::Songs, card);
    }
}

/// The session's last status, so a settings change can redo the pill.
static LAST_STATUS: Mutex<Option<RuntimeStatus>> = Mutex::new(None);

/// The overlay settings changed: apply them now, island included.
pub fn settings_changed<R: Runtime>(app: &AppHandle<R>) {
    let last = LAST_STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(status) = last {
        on_status(app, &status);
    }
    overlay::settings_changed(app);
}

/// The session's status changed: update persistent mode's pill.
pub fn on_status<R: Runtime>(app: &AppHandle<R>, status: &RuntimeStatus) {
    *LAST_STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(status.clone());
    on_song(app, status);
    let reason = {
        let mut reason = interrupted();
        if status.sync != SyncState::SteppedOut {
            *reason = None;
        }
        reason.clone()
    };
    let sync_on = app
        .try_state::<AppState>()
        .is_some_and(|state| state.settings().overlay.sync);
    overlay::set_rest(app, rest_for(status, reason.as_deref(), sync_on));
}

/// Returns whether the overlay showed it (so a Windows notification for
/// the same thing isn't needed).
pub fn on_notice<R: Runtime>(app: &AppHandle<R>, notice: &Notice) -> bool {
    if let Notice::Interrupted { reason } = notice {
        *interrupted() = Some(reason.clone());
    }
    label_notice(notice).is_some_and(|flash| offer(app, Group::Sync, flash))
}

pub fn on_sync<R: Runtime>(app: &AppHandle<R>, from: SyncState, to: SyncState) {
    if let Some(flash) = label_sync(from, to) {
        offer(app, Group::Sync, flash);
    }
}

/// "Show a test notification": shown whatever is in front, so the corner
/// and length can be tried from the settings page. Async: making the
/// window on the main thread (where sync commands run) would deadlock.
#[tauri::command]
pub async fn test_overlay(app: AppHandle) {
    let settings = app.state::<AppState>().settings().overlay.clone();
    let front = screen::foreground(&app);
    overlay::show(
        &app,
        &settings,
        front,
        Flash::new("pause", "Paused by Attune", Tone::Info),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lily() -> Option<String> {
        Some("Lily".to_owned())
    }

    #[test]
    fn actions_say_who_did_them() {
        let text = |a: Activity| label_activity(&a).1.text;
        assert_eq!(text(Activity::Paused { by: lily() }), "Paused by Lily");
        assert_eq!(text(Activity::Skipped { by: lily() }), "Skipped by Lily");
        assert_eq!(
            text(Activity::Skipped { by: None }),
            "Skipped: nobody can play it"
        );
        assert_eq!(
            text(Activity::Added {
                by: lily(),
                title: "Song".to_owned()
            }),
            "Lily added “Song”"
        );
        assert_eq!(
            label_activity(&Activity::Joined {
                name: "Sam".to_owned()
            }),
            (
                Group::People,
                Flash::new("userPlus", "Sam joined", Tone::Info)
            )
        );
    }

    #[test]
    fn sync_changes_name_the_state_and_skip_the_quiet_ones() {
        use SyncState::*;
        assert_eq!(label_sync(Synced, Resyncing).unwrap().text, "Catching up");
        assert_eq!(label_sync(Synced, SteppedOut).unwrap().text, "Stepped out");
        assert_eq!(label_sync(Resyncing, Synced), None, "comes as Resynced");
        assert_eq!(label_sync(Synced, Ended), None, "comes as Ended");
        assert_eq!(label_sync(Error, Error), None);
    }

    #[test]
    fn leaving_yourself_is_not_news() {
        let ended = |reason: &str| Notice::Ended {
            reason: reason.to_owned(),
        };
        assert_eq!(label_notice(&ended("left")), None);
        assert!(label_notice(&ended("the session no longer exists")).is_some());
        assert_eq!(
            label_notice(&Notice::Interrupted {
                reason: "advertisement".to_owned()
            })
            .unwrap()
            .text,
            "Stepped out: ad"
        );
    }

    fn status(sync: SyncState, playing: bool, queue: &[(&str, &[&str])]) -> RuntimeStatus {
        use attune_core::{protocol::SessionPlayback, session::QueueItem};
        RuntimeStatus {
            session_id: "KWPZ".to_owned(),
            client_id: "me".to_owned(),
            sync,
            behind_ms: None,
            sequence: 1,
            queue: queue
                .iter()
                .map(|(title, artists)| QueueItem {
                    entry_id: title.to_string(),
                    track: serde_json::from_value(serde_json::json!({
                        "title": title, "artists": artists,
                    }))
                    .unwrap(),
                    added_by: None,
                    playable_here: true,
                })
                .collect(),
            playback: SessionPlayback {
                is_playing: playing,
                position_ms: 0,
                position_captured_at_ms: 0,
            },
            participants: Vec::new(),
            warnings: Vec::new(),
            host_client_id: None,
            others_can_control: true,
            repeat: Default::default(),
        }
    }

    #[test]
    fn the_island_shows_a_state_over_the_song() {
        let song: &[(&str, &[&str])] = &[("Song", &["A", "B"]), ("Next", &[])];
        let rest = |sync, playing, reason, sync_on| {
            rest_for(&status(sync, playing, song), reason, sync_on).map(|r| (r.flash.text, r.song))
        };
        assert_eq!(
            rest(SyncState::Synced, true, None, true),
            Some(("Song \u{2014} A, B".to_owned(), true))
        );
        assert_eq!(
            rest_for(&status(SyncState::Synced, false, song), None, true)
                .unwrap()
                .flash
                .icon,
            "pause"
        );
        assert_eq!(
            rest(SyncState::SteppedOut, true, Some("advertisement"), true),
            Some(("Stepped out: ad".to_owned(), false))
        );
        assert_eq!(
            rest(SyncState::Resyncing, true, None, true),
            Some(("Catching up".to_owned(), false))
        );
        // With the Sync group off the song shows through.
        assert_eq!(
            rest(SyncState::Resyncing, true, None, false),
            Some(("Song \u{2014} A, B".to_owned(), true))
        );
        assert_eq!(rest(SyncState::Ended, true, None, true), None);
        assert_eq!(
            rest_for(&status(SyncState::Synced, true, &[]), None, true),
            None,
            "nothing queued: nothing to show"
        );
    }

    #[test]
    fn a_new_song_gets_a_card_but_not_the_first_one() {
        let queue: &[(&str, &[&str])] = &[("Song", &["A"]), ("Next", &[])];
        let now = status(SyncState::Synced, true, queue);
        let seen = |entry: &str| ("KWPZ".to_owned(), entry.to_owned());
        assert_eq!(song_change(None, &now), None, "joining isn't a change");
        assert_eq!(song_change(Some(&seen("Song")), &now), None, "same song");
        let card = song_change(Some(&seen("Earlier")), &now).unwrap();
        assert_eq!(card.text, "Song \u{2014} A");
        let other_session = ("ABCD".to_owned(), "Earlier".to_owned());
        assert_eq!(song_change(Some(&other_session), &now), None);
        assert_eq!(
            song_change(
                Some(&seen("Earlier")),
                &status(SyncState::Synced, true, &[])
            ),
            None
        );
    }

    #[test]
    fn song_cards_are_for_flash_mode_only() {
        let on = OverlaySettings {
            songs: true,
            ..OverlaySettings::default()
        };
        assert!(group_on(&on, Group::Songs));
        assert!(
            !group_on(&OverlaySettings::default(), Group::Songs),
            "off by default"
        );
        let persistent = OverlaySettings {
            mode: OverlayMode::Persistent,
            ..on
        };
        assert!(!group_on(&persistent, Group::Songs));
    }

    #[test]
    fn showing_follows_the_setting_and_never_covers_attune() {
        use Foreground::*;
        let with = |show_when| OverlaySettings {
            show_when,
            ..OverlaySettings::default()
        };
        let full = with(ShowWhen::FullScreen);
        assert!(should_show(&full, FullScreen, true));
        assert!(!should_show(&full, Other, false));

        let hidden = with(ShowWhen::Hidden);
        assert!(should_show(&hidden, Other, false));
        assert!(!should_show(&hidden, Other, true));
        assert!(should_show(&hidden, FullScreen, true));

        let always = with(ShowWhen::Always);
        assert!(should_show(&always, Other, true));
        for settings in [&full, &hidden, &always] {
            assert!(!should_show(settings, Attune, false), "{settings:?}");
        }
        let off = OverlaySettings {
            enabled: false,
            ..always
        };
        assert!(!should_show(&off, FullScreen, false));
    }
}
