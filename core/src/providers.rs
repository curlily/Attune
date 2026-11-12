use std::path::PathBuf;

use crate::{
    Error, Result,
    structs::{BrowserPage, ProviderConfig, ProviderId},
};

// Separate ports let both desktop clients remain open without competing for
// Chromium's single --remote-debugging-port listener.
const TIDAL_CDP_PORT: u16 = 9222;
const SPOTIFY_CDP_PORT: u16 = 9223;
const YOUTUBE_MUSIC_CDP_PORT: u16 = 9224;

/// Overrides where the browser profile for YouTube Music is kept.
const YOUTUBE_MUSIC_PROFILE_ENV: &str = "ATTUNE_YOUTUBE_MUSIC_PROFILE";

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
        browser: None,
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
        browser: None,
    })
}

/// YouTube Music has no desktop app: it runs as a page in an installed
/// Chromium browser, in a profile of its own.
pub fn youtube_music() -> Result<ProviderConfig> {
    let profile_dir = match std::env::var_os(YOUTUBE_MUSIC_PROFILE_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => env_dir("LOCALAPPDATA")?
            .join("Attune")
            .join("YouTubeMusicProfile"),
    };
    Ok(ProviderConfig {
        id: ProviderId::YoutubeMusic,
        display_name: "YouTube Music".to_owned(),
        executable_path: chromium_browser(),
        page_url: "music.youtube.com".to_owned(),
        debugging_port: YOUTUBE_MUSIC_CDP_PORT,
        bridge_script: include_str!("adapters/youtube_music.js").to_owned(),
        browser: Some(BrowserPage {
            start_url: "https://music.youtube.com".to_owned(),
            profile_dir,
        }),
    })
}

/// The first installed Chromium browser, or where Edge would be (Windows
/// ships it) so "not installed" still names something.
fn chromium_browser() -> PathBuf {
    let candidates = browser_candidates();
    candidates
        .iter()
        .find(|path| path.is_file())
        .or(candidates.first())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("msedge.exe"))
}

fn browser_candidates() -> Vec<PathBuf> {
    const BROWSERS: [&[&str]; 3] = [
        &["Microsoft", "Edge", "Application", "msedge.exe"],
        &["Google", "Chrome", "Application", "chrome.exe"],
        &["BraveSoftware", "Brave-Browser", "Application", "brave.exe"],
    ];
    let roots: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    BROWSERS
        .iter()
        .flat_map(|parts| {
            roots.iter().map(|root| {
                parts
                    .iter()
                    .fold(root.clone(), |path, part| path.join(part))
            })
        })
        .collect()
}
