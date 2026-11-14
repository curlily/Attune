//! What Discord shows: "Listening to <music app>" with the song the music
//! app is playing, its artists and cover, and a progress bar.
//!
//! Built from the music app's state alone (in a session it plays the
//! session's song), so it's the same function every time and testable
//! without Discord. The session's ID is its join code and
//! presence is visible to anyone sharing a server, so the code only appears
//! in a **Listen along** button, which the user turns on separately (see
//! [`Activity::with_listen_along`]).

use attune_core::protocol::SessionMusicState;
use serde::Serialize;

/// Discord's "Listening to …".
const LISTENING: u8 = 2;
/// Show `state` (the artists) in the status line under your name, not the
/// app's name or the song title.
const SHOW_STATE: u8 = 1;
/// Discord accepts 2 to 128 characters in its text fields.
const MAX_TEXT: usize = 128;
/// The download page, which offers to open Attune on the code in `?join=`.
const LISTEN_ALONG_URL: &str = "https://curlily.github.io/Attune/?join=";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activity {
    /// The music app, for "Listening to TIDAL" in place of the Discord
    /// application's own name.
    pub name: String,
    #[serde(rename = "type")]
    pub kind: u8,
    pub status_display_type: u8,
    pub details: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<Timestamps>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<Assets>,
    /// Shown to others, never to yourself. Discord allows two.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<Button>,
}

/// Unix milliseconds. With both, Discord draws a progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Timestamps {
    pub start: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assets {
    pub large_image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Button {
    pub label: String,
    pub url: String,
}

impl Activity {
    /// The same activity with a **Listen along** button that opens the
    /// download page on `code`: it starts Attune and joins if Attune is
    /// installed, and offers the download if not.
    pub fn with_listen_along(mut self, code: &str) -> Activity {
        self.buttons = vec![Button {
            label: "Listen along".to_owned(),
            url: format!("{LISTEN_ALONG_URL}{code}"),
        }];
        self
    }

    /// The same activity, give or take the clock: a status update moments
    /// later computes timestamps a few milliseconds apart, which isn't worth
    /// one of Discord's few updates per minute.
    pub fn same_as(&self, other: &Activity) -> bool {
        const SLACK_MS: u64 = 1500;
        let near = |a: u64, b: u64| a.abs_diff(b) <= SLACK_MS;
        let timestamps = match (self.timestamps, other.timestamps) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                near(a.start, b.start)
                    && match (a.end, b.end) {
                        (None, None) => true,
                        (Some(x), Some(y)) => near(x, y),
                        _ => false,
                    }
            }
            _ => false,
        };
        timestamps
            && self.name == other.name
            && self.kind == other.kind
            && self.details == other.details
            && self.state == other.state
            && self.assets == other.assets
            && self.buttons == other.buttons
    }
}

/// The activity for what the music app `app` is playing (`state`) at
/// `now_ms` (Unix ms, this computer's clock), or `None` when it has nothing
/// loaded.
pub fn from_state(state: &SessionMusicState, app: &str, now_ms: u64) -> Option<Activity> {
    let track = &state.queue.first()?.track;
    let playback = &state.playback;
    let details = text(&track.title).unwrap_or_else(|| "Unknown song".to_owned());
    let artists = text(&track.artists.join(", "));

    let (state, timestamps) = if playback.is_playing {
        let elapsed = now_ms.saturating_sub(playback.position_captured_at_ms);
        let position = playback.position_ms + elapsed;
        let start = now_ms.saturating_sub(position);
        let end = track
            .duration_ms
            .filter(|&duration| duration > 0 && position <= duration)
            .map(|duration| start + duration);
        (artists, Some(Timestamps { start, end }))
    } else {
        let paused = match &artists {
            Some(artists) => format!("Paused · {artists}"),
            None => "Paused".to_owned(),
        };
        // With no timestamps Discord counts up from when the activity was
        // set (a green ♫ 0:00). A song that started and ended at the moment
        // it was paused shows no timer at all.
        let paused_at = playback.position_captured_at_ms.min(now_ms);
        let stopped = Timestamps {
            start: paused_at,
            end: Some(paused_at),
        };
        (text(&paused), Some(stopped))
    };

    let assets = track
        .cover_url
        .as_deref()
        .filter(|url| url.starts_with("https://") && url.len() <= 256)
        .map(|url| Assets {
            large_image: url.to_owned(),
            large_text: None,
        });

    Some(Activity {
        name: app.to_owned(),
        kind: LISTENING,
        status_display_type: SHOW_STATE,
        details,
        state,
        timestamps,
        assets,
        buttons: Vec::new(),
    })
}

/// Fit `value` to Discord's 2–128 characters: `None` when empty, padded
/// when a single character, cut short with an ellipsis when too long.
fn text(value: &str) -> Option<String> {
    let value = value.trim();
    match value.chars().count() {
        0 => None,
        // Braille blank: Discord trims ordinary spaces.
        1 => Some(format!("{value}\u{2800}")),
        n if n > MAX_TEXT => {
            let cut: String = value.chars().take(MAX_TEXT - 1).collect();
            Some(format!("{}…", cut.trim_end()))
        }
        _ => Some(value.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use attune_core::protocol::{QueueEntry, SessionPlayback, TrackDescriptor};

    const NOW: u64 = 1_800_000_000_000;

    fn state(is_playing: bool) -> SessionMusicState {
        SessionMusicState {
            queue: vec![QueueEntry {
                entry_id: "e1".to_owned(),
                track: TrackDescriptor {
                    title: "Midnight Static".to_owned(),
                    artists: vec!["Paper Satellites".to_owned(), "Kei".to_owned()],
                    isrc: None,
                    album: Some("Night Drive".to_owned()),
                    duration_ms: Some(240_000),
                    cover_url: Some("https://i.scdn.co/image/abc".to_owned()),
                },
                added_by: None,
            }],
            current_entry_id: Some("e1".to_owned()),
            playback: SessionPlayback {
                is_playing,
                position_ms: 60_000,
                // Captured 2 s ago: the song is now 62 s in.
                position_captured_at_ms: NOW - 2_000,
            },
            repeat: Default::default(),
        }
    }

    #[test]
    fn playing_shows_the_song_with_a_progress_bar() {
        let activity = from_state(&state(true), "TIDAL", NOW).unwrap();
        assert_eq!(activity.kind, LISTENING);
        assert_eq!(activity.name, "TIDAL");
        assert_eq!(activity.details, "Midnight Static");
        assert_eq!(activity.state.as_deref(), Some("Paper Satellites, Kei"));
        let timestamps = activity.timestamps.unwrap();
        assert_eq!(timestamps.start, NOW - 62_000);
        assert_eq!(timestamps.end, Some(NOW - 62_000 + 240_000));
        let assets = activity.assets.unwrap();
        assert_eq!(assets.large_image, "https://i.scdn.co/image/abc");
        // Discord shows `large_text` as a line of its own: no album.
        assert_eq!(assets.large_text, None);
    }

    #[test]
    fn paused_has_no_timer() {
        let activity = from_state(&state(false), "TIDAL", NOW).unwrap();
        // Zero length, in the past: Discord shows neither a bar nor a counter.
        let paused_at = NOW - 2_000;
        assert_eq!(
            activity.timestamps,
            Some(Timestamps {
                start: paused_at,
                end: Some(paused_at)
            })
        );
        // And it stays put while the pause lasts.
        let later = from_state(&state(false), "TIDAL", NOW + 60_000).unwrap();
        assert_eq!(activity, later);
        assert_eq!(
            activity.state.as_deref(),
            Some("Paused · Paper Satellites, Kei")
        );
    }

    #[test]
    fn nothing_when_nothing_is_loaded() {
        let mut empty = state(true);
        empty.queue.clear();
        empty.current_entry_id = None;
        assert_eq!(from_state(&empty, "TIDAL", NOW), None);
    }

    #[test]
    fn never_mentions_the_join_code() {
        let activity = from_state(&state(true), "TIDAL", NOW).unwrap();
        let json = serde_json::to_string(&activity).unwrap();
        assert!(!json.contains("KWPZ"), "{json}");
    }

    #[test]
    fn listen_along_links_the_download_page_to_the_code() {
        let plain = from_state(&state(true), "TIDAL", NOW).unwrap();
        let along = plain.clone().with_listen_along("KWPZ");
        assert_eq!(along.buttons.len(), 1);
        assert_eq!(along.buttons[0].label, "Listen along");
        assert_eq!(
            along.buttons[0].url,
            "https://curlily.github.io/Attune/?join=KWPZ"
        );
        assert!(!plain.same_as(&along));
        let json = serde_json::to_value(&along).unwrap();
        assert_eq!(json["buttons"][0]["label"], "Listen along");
        // Without it, no `buttons` at all.
        assert!(serde_json::to_value(&plain)
            .unwrap()
            .get("buttons")
            .is_none());
    }

    #[test]
    fn text_fits_discords_limits() {
        assert_eq!(text("  "), None);
        assert_eq!(text("X").unwrap().chars().count(), 2);
        let long = "a".repeat(300);
        let fitted = text(&long).unwrap();
        assert_eq!(fitted.chars().count(), MAX_TEXT);
        assert!(fitted.ends_with('…'));
    }

    #[test]
    fn only_https_covers() {
        let mut plain = state(true);
        plain.queue[0].track.cover_url = Some("http://example.com/a.jpg".to_owned());
        assert_eq!(from_state(&plain, "TIDAL", NOW).unwrap().assets, None);
    }

    #[test]
    fn small_clock_differences_are_the_same_activity() {
        let a = from_state(&state(true), "TIDAL", NOW).unwrap();
        // While playing, the start doesn't move with the clock...
        assert_eq!(a, from_state(&state(true), "TIDAL", NOW + 400).unwrap());
        // ...but a fresh position report lands a few ms off.
        let mut report = state(true);
        report.playback.position_ms += 300;
        report.playback.position_captured_at_ms += 250;
        let b = from_state(&report, "TIDAL", NOW).unwrap();
        assert_ne!(a, b);
        assert!(a.same_as(&b));
        // A seek is a real change.
        let mut sought = state(true);
        sought.playback.position_ms = 120_000;
        assert!(!a.same_as(&from_state(&sought, "TIDAL", NOW).unwrap()));
    }
}
