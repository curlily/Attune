#![allow(dead_code)]

use attune_protocol::{
    ClientHello, PROTOCOL_VERSION, QueueEntry, SessionMusicState, SessionPlayback, TrackDescriptor,
};

pub const DURATION_MS: u64 = 200_000;

pub fn track(title: &str) -> TrackDescriptor {
    TrackDescriptor {
        title: title.to_owned(),
        artists: vec!["Artist".to_owned()],
        isrc: None,
        album: Some("Album".to_owned()),
        duration_ms: Some(DURATION_MS),
        cover_url: None,
    }
}

pub fn entry(id: &str) -> QueueEntry {
    QueueEntry {
        entry_id: id.to_owned(),
        track: track(&format!("Song {id}")),
    }
}

/// A valid state whose queue is `ids` (current first).
pub fn state(
    ids: &[&str],
    is_playing: bool,
    position_ms: u64,
    captured_at_ms: u64,
) -> SessionMusicState {
    SessionMusicState {
        queue: ids.iter().map(|id| entry(id)).collect(),
        current_entry_id: ids.first().map(|id| (*id).to_owned()),
        playback: SessionPlayback {
            is_playing,
            position_ms,
            position_captured_at_ms: captured_at_ms,
        },
    }
}

pub fn empty(captured_at_ms: u64) -> SessionMusicState {
    state(&[], false, 0, captured_at_ms)
}

pub fn ids(state: &SessionMusicState) -> Vec<&str> {
    state
        .queue
        .iter()
        .map(|entry| entry.entry_id.as_str())
        .collect()
}

pub fn hello(client_id: &str) -> ClientHello {
    ClientHello {
        protocol_version: PROTOCOL_VERSION,
        client_id: client_id.to_owned(),
        display_name: format!("User {client_id}"),
        provider: Some("tidal".to_owned()),
    }
}
