use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderId {
    Spotify,
    Tidal,
    YoutubeMusic,
    AppleMusic,
}

impl ProviderId {
    /// Display label shared with other participants.
    pub fn label(self) -> &'static str {
        match self {
            Self::Spotify => "spotify",
            Self::Tidal => "tidal",
            Self::YoutubeMusic => "youtube-music",
            Self::AppleMusic => "apple-music",
        }
    }

    /// The service's own name, as people know it.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Spotify => "Spotify",
            Self::Tidal => "TIDAL",
            Self::YoutubeMusic => "YouTube Music",
            Self::AppleMusic => "Apple Music",
        }
    }
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
    /// Set when the provider is a website in a browser rather than a
    /// desktop app of its own.
    pub browser: Option<BrowserPage>,
}

/// A provider that runs as a page in a Chromium browser.
///
/// The executable is the user's browser, so Attune must never close it to
/// restart it: it opens a window of its own, with a profile of its own
/// (browsers ignore the debug port on the user's default profile).
#[derive(Debug, Clone)]
pub struct BrowserPage {
    /// The page to open, as an app window.
    pub start_url: String,
    /// Browser profile used only for this provider.
    pub profile_dir: PathBuf,
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
