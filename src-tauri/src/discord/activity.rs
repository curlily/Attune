//! What Discord shows while you're in a session: a Listening activity with
//! the current song, its artists and cover, and a progress bar.
//!
//! Built from the session's status alone, so it's the same function every
//! time and testable without Discord. Nothing here names the session: its
//! ID is the join code, and presence is visible to anyone sharing a server.

use attune_core::session::{RuntimeStatus, SyncState};
use serde::Serialize;

/// Discord's "Listening to …".
const LISTENING: u8 = 2;
/// Show `details` (the song title) in the status line, not the app's name.
const SHOW_DETAILS: u8 = 2;
/// Discord accepts 2 to 128 characters in its text fields.
const MAX_TEXT: usize = 128;
const DOWNLOAD_URL: &str = "https://curlily.github.io/Attune/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activity {
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

/// Shown to others, never to yourself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Button {
    pub label: String,
    pub url: String,
}

impl Activity {
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
            && self.kind == other.kind
            && self.details == other.details
            && self.state == other.state
            && self.assets == other.assets
            && self.buttons == other.buttons
    }
}

/// The activity for `status` at `now_ms` (Unix ms, this computer's clock),
/// or `None` when you aren't hearing the session's music: stepped out, not
/// able to play the current song, after an error, or once it has ended.
pub fn from_status(status: &RuntimeStatus, now_ms: u64) -> Option<Activity> {
    if !matches!(
        status.sync,
        SyncState::Synced | SyncState::Resyncing | SyncState::Reconnecting
    ) {
        return None;
    }
    let track = &status.queue.first()?.track;
    let details = text(&track.title).unwrap_or_else(|| "Unknown song".to_owned());
    let artists = text(&track.artists.join(", "));
    let playback = &status.playback;

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
        (text(&paused), None)
    };

    let assets = track
        .cover_url
        .as_deref()
        .filter(|url| url.starts_with("https://") && url.len() <= 256)
        .map(|url| Assets {
            large_image: url.to_owned(),
            large_text: track.album.as_deref().and_then(text),
        });

    Some(Activity {
        kind: LISTENING,
        status_display_type: SHOW_DETAILS,
        details,
        state,
        timestamps,
        assets,
        buttons: vec![Button {
            label: "Get Attune".to_owned(),
            url: DOWNLOAD_URL.to_owned(),
        }],
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
    use attune_core::{
        protocol::{SessionPlayback, TrackDescriptor},
        session::QueueItem,
    };

    const NOW: u64 = 1_800_000_000_000;

    fn status(sync: SyncState, is_playing: bool) -> RuntimeStatus {
        RuntimeStatus {
            session_id: "KWPZ".to_owned(),
            client_id: "me".to_owned(),
            sync,
            sequence: 1,
            queue: vec![QueueItem {
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
                playable_here: true,
            }],
            playback: SessionPlayback {
                is_playing,
                position_ms: 60_000,
                // Captured 2 s ago: the song is now 62 s in.
                position_captured_at_ms: NOW - 2_000,
            },
            participants: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn playing_shows_the_song_with_a_progress_bar() {
        let activity = from_status(&status(SyncState::Synced, true), NOW).unwrap();
        assert_eq!(activity.kind, LISTENING);
        assert_eq!(activity.details, "Midnight Static");
        assert_eq!(activity.state.as_deref(), Some("Paper Satellites, Kei"));
        let timestamps = activity.timestamps.unwrap();
        assert_eq!(timestamps.start, NOW - 62_000);
        assert_eq!(timestamps.end, Some(NOW - 62_000 + 240_000));
        let assets = activity.assets.unwrap();
        assert_eq!(assets.large_image, "https://i.scdn.co/image/abc");
        assert_eq!(assets.large_text.as_deref(), Some("Night Drive"));
    }

    #[test]
    fn paused_has_no_timer() {
        let activity = from_status(&status(SyncState::Synced, false), NOW).unwrap();
        assert_eq!(activity.timestamps, None);
        assert_eq!(
            activity.state.as_deref(),
            Some("Paused · Paper Satellites, Kei")
        );
    }

    #[test]
    fn nothing_while_not_hearing_the_session() {
        for sync in [
            SyncState::SteppedOut,
            SyncState::WaitingForNextSong,
            SyncState::Error,
            SyncState::Ended,
        ] {
            assert_eq!(from_status(&status(sync, true), NOW), None, "{sync:?}");
        }
        let mut empty = status(SyncState::Synced, true);
        empty.queue.clear();
        assert_eq!(from_status(&empty, NOW), None);
    }

    #[test]
    fn never_mentions_the_join_code() {
        let activity = from_status(&status(SyncState::Synced, true), NOW).unwrap();
        let json = serde_json::to_string(&activity).unwrap();
        assert!(!json.contains("KWPZ"), "{json}");
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
        let mut plain = status(SyncState::Synced, true);
        plain.queue[0].track.cover_url = Some("http://example.com/a.jpg".to_owned());
        assert_eq!(from_status(&plain, NOW).unwrap().assets, None);
    }

    #[test]
    fn small_clock_differences_are_the_same_activity() {
        let a = from_status(&status(SyncState::Synced, true), NOW).unwrap();
        // While playing, the start doesn't move with the clock...
        assert_eq!(
            a,
            from_status(&status(SyncState::Synced, true), NOW + 400).unwrap()
        );
        // ...but a fresh position report lands a few ms off.
        let mut report = status(SyncState::Synced, true);
        report.playback.position_ms += 300;
        report.playback.position_captured_at_ms += 250;
        let b = from_status(&report, NOW).unwrap();
        assert_ne!(a, b);
        assert!(a.same_as(&b));
        // A seek is a real change.
        let mut sought = status(SyncState::Synced, true);
        sought.playback.position_ms = 120_000;
        assert!(!a.same_as(&from_status(&sought, NOW).unwrap()));
    }
}
