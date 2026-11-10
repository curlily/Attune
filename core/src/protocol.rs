//! Provider-independent music session protocol shared by all renderer adapters.
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    /// Stable identity of this queue occurrence; duplicates have different IDs.
    pub entry_id: String,
    pub track: TrackDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPlayback {
    pub is_playing: bool,
    pub position_ms: u64,
    /// Wall-clock timestamp at which position_ms was captured.
    pub position_captured_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMusicState {
    /// Queue is ordered current entry first, followed by upcoming entries.
    #[serde(default)]
    pub queue: Vec<QueueEntry>,
    /// Usually queue[0]. Kept explicit so empty queues and transitions are clear.
    pub current_entry_id: Option<String>,
    pub playback: SessionPlayback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub protocol_version: u16,
    pub session_id: String,
    pub revision: u64,
    pub updated_at_ms: u64,
    pub origin_client_id: String,
    pub state: SessionMusicState,
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
    PlaybackSeek {
        entry_id: String,
        position_ms: u64,
    },
    #[serde(rename = "playback.setPlaying")]
    PlaybackSetPlaying { is_playing: bool },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn operation_uses_provider_independent_wire_names() {
        let operation = SessionOperation::PlaybackSetPlaying { is_playing: true };
        let value = serde_json::to_value(operation).expect("serialize operation");

        assert_eq!(value, json!({
            "type": "playback.setPlaying",
            "isPlaying": true,
        }));
    }

    #[test]
    fn operation_fields_use_camel_case() {
        let operation = SessionOperation::PlaybackAdvance {
            target_entry_id: "entry-1".to_owned(),
            position_ms: 2500,
            is_playing: true,
        };
        let value = serde_json::to_value(operation).expect("serialize operation");

        assert_eq!(value["targetEntryId"], "entry-1");
        assert_eq!(value["positionMs"], 2500);
        assert_eq!(value["isPlaying"], true);
        assert!(value.get("target_entry_id").is_none());
    }

    #[test]
    fn state_uses_camel_case_fields() {
        let state = SessionMusicState {
            queue: Vec::new(),
            current_entry_id: None,
            playback: SessionPlayback {
                is_playing: false,
                position_ms: 0,
                position_captured_at_ms: 1_800_000_000_000,
            },
        };
        let value = serde_json::to_value(state).expect("serialize state");

        assert_eq!(value["currentEntryId"], serde_json::Value::Null);
        assert_eq!(value["playback"]["positionCapturedAtMs"], 1_800_000_000_000_u64);
    }
}
