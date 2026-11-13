//! Shared music-session data model.
//!
//! Provider-specific identifiers never cross this boundary. Each adapter maps
//! these descriptors and operations to its own music client's internal API.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDescriptor {
    pub title: String,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Album art from the provider that queued it, for display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    /// Stable identity of this queue occurrence; duplicates have different IDs.
    pub entry_id: String,
    pub track: TrackDescriptor,
    /// Display name of who queued it. Set by the server from the sender of
    /// the change that added it; kept unchanged afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPlayback {
    pub is_playing: bool,
    pub position_ms: u64,
    /// Wall-clock timestamp at which `position_ms` was captured. In
    /// server-issued state this is the server's clock.
    pub position_captured_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMusicState {
    /// Ordered current entry first, followed by upcoming entries. History is
    /// excluded.
    #[serde(default)]
    pub queue: Vec<QueueEntry>,
    /// Always `queue[0].entryId`, or `null` for an empty queue. Kept explicit
    /// because both adapters and operations refer to it directly.
    pub current_entry_id: Option<String>,
    pub playback: SessionPlayback,
    /// Absent (off) in states from before repeat existed.
    #[serde(default, skip_serializing_if = "is_off")]
    pub repeat: RepeatMode,
}

fn is_off(mode: &RepeatMode) -> bool {
    *mode == RepeatMode::Off
}

/// Authoritative session state as issued by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub protocol_version: u16,
    pub session_id: String,
    /// Equals the session sequence at the time the snapshot was taken.
    pub revision: u64,
    pub updated_at_ms: u64,
    pub state: SessionMusicState,
}

/// Where the new current song of a `queue.replace` is (protocol 2). A
/// provider may start a new list part-way: Spotify resumes an album or
/// playlist where it was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacePlayback {
    pub position_ms: u64,
    pub is_playing: bool,
}

/// Absolute, provider-independent operations. They are intentionally not
/// encoded as provider actions such as Redux action names or Spotify URIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum SessionOperation {
    #[serde(rename = "queue.replace")]
    QueueReplace {
        entries: Vec<QueueEntry>,
        current_entry_id: Option<String>,
        /// Only used when the current entry changes; without it the new
        /// current song starts at 0, playing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        playback: Option<ReplacePlayback>,
    },
    #[serde(rename = "queue.insert")]
    QueueInsert { entry: QueueEntry, index: usize },
    #[serde(rename = "queue.remove")]
    QueueRemove { entry_id: String },
    #[serde(rename = "queue.move")]
    QueueMove { entry_id: String, to_index: usize },
    #[serde(rename = "playback.select")]
    PlaybackSelect {
        entry_id: String,
        position_ms: u64,
        is_playing: bool,
    },
    #[serde(rename = "playback.advance")]
    PlaybackAdvance {
        target_entry_id: String,
        position_ms: u64,
        is_playing: bool,
    },
    #[serde(rename = "playback.seek")]
    PlaybackSeek { entry_id: String, position_ms: u64 },
    #[serde(rename = "playback.setPlaying")]
    PlaybackSetPlaying { is_playing: bool },
    /// Repeat the current song (or not). Providers keep their own repeat
    /// off; clients restart the song when it ends.
    #[serde(rename = "playback.setRepeat")]
    PlaybackSetRepeat { mode: RepeatMode },
}

/// What happens when the current song ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RepeatMode {
    /// Move on to the next song.
    #[default]
    Off,
    /// Play the same song again.
    One,
}

impl SessionOperation {
    /// Whether the operation changes queue structure (and therefore the
    /// server's `lastQueueChangeSeq`).
    pub fn changes_queue(&self) -> bool {
        !matches!(
            self,
            Self::PlaybackSeek { .. }
                | Self::PlaybackSetPlaying { .. }
                | Self::PlaybackSetRepeat { .. }
        )
    }
}
