use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderId {
    Spotify,
    Tidal,
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub id: ProviderId,
    pub display_name: String,
    pub executable_path: PathBuf,
    pub page_url: String,
    /// Dedicated loopback CDP port for this provider process.
    pub debugging_port: u16,
    pub bridge_script: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Track {
    pub title: String,
    pub artists: Vec<String>,
    pub isrc: Option<String>,
    pub provider: Option<ProviderId>,
    pub provider_track_id: Option<String>,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayState {
    pub track: Option<Track>,
    pub position_ms: Option<u64>,
    pub timestamp_ms: Option<u64>,
    pub playing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueState {
    pub queue: Vec<Track>,
}
