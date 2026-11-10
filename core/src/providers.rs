use std::path::PathBuf;

use crate::{
    Error, Result,
    structs::{ProviderConfig, ProviderId},
};

// Separate ports let both desktop clients remain open without competing for
// Chromium's single --remote-debugging-port listener.
const TIDAL_CDP_PORT: u16 = 9222;
const SPOTIFY_CDP_PORT: u16 = 9223;

fn env_dir(var: &'static str) -> Result<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .ok_or(Error::MissingEnv { var })
}

pub fn spotify() -> Result<ProviderConfig> {
    Ok(ProviderConfig {
        id: ProviderId::Spotify,
        display_name: "Spotify".to_owned(),
        executable_path: env_dir("APPDATA")?.join("Spotify").join("Spotify.exe"),
        page_url: "xpui.app.spotify.com".to_owned(),
        debugging_port: SPOTIFY_CDP_PORT,
        bridge_script: include_str!("adapters/spotify.js").to_owned(),
    })
}

pub fn tidal() -> Result<ProviderConfig> {
    Ok(ProviderConfig {
        id: ProviderId::Tidal,
        display_name: "TIDAL".to_owned(),
        executable_path: env_dir("LOCALAPPDATA")?.join("TIDAL").join("TIDAL.exe"),
        page_url: "desktop.tidal.com".to_owned(),
        debugging_port: TIDAL_CDP_PORT,
        bridge_script: include_str!("adapters/tidal.js").to_owned(),
    })
}
