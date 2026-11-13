//! User settings and the last session, stored as JSON in the app data folder.
//!
//! `settings.json` holds preferences; `rejoin.json` holds what is needed to
//! take the seat back after Attune restarts (it contains the rejoin secret,
//! so it is kept separate, never sent to the UI and deleted when the session
//! is over). Both are written atomically (write a temp file, then rename),
//! so a crash mid-write never leaves a half-written file.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use attune_core::{network::new_client_id, protocol::MAX_DISPLAY_NAME_CHARS, structs::ProviderId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::warn;

use crate::error::AppError;

/// The relay Attune connects to unless the user picks another. Builds take
/// it from `ATTUNE_DEFAULT_SERVER_URL` (set in CI, or locally in the ignored
/// `.cargo/config.toml`) so the public relay's address isn't in the source;
/// without it, a local relay.
pub const DEFAULT_SERVER_URL: &str = match option_env!("ATTUNE_DEFAULT_SERVER_URL") {
    Some(url) => url,
    None => "ws://127.0.0.1:8787/ws",
};
/// Upper end of the listening offset setting.
pub const MAX_LISTENING_OFFSET_MS: u32 = 1_000;

const SETTINGS_FILE: &str = "settings.json";
const REJOIN_FILE: &str = "rejoin.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// `None` until the first-run screen asks for it.
    pub display_name: Option<String>,
    pub server_url: String,
    /// Connected automatically on start when it's running with its debug port.
    pub last_provider: Option<ProviderId>,
    /// Play this far behind the group (for voice-chat sing-alongs).
    pub listening_offset_ms: u32,
    /// Identifies this installation to the server, so a restarted Attune can
    /// take its old seat back. Not secret: everyone in a session sees it.
    pub client_id: String,
    /// An update the user chose to skip: not offered on launch again.
    pub skipped_update_version: Option<String>,
    /// Show the session's song on your Discord profile.
    pub discord_presence: bool,
    /// Let others in your session change what's playing. Counts only while
    /// you're the session's host.
    pub others_can_control: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            display_name: None,
            server_url: DEFAULT_SERVER_URL.to_owned(),
            last_provider: None,
            listening_offset_ms: 0,
            client_id: new_client_id(),
            skipped_update_version: None,
            discord_presence: true,
            others_can_control: true,
        }
    }
}

/// The settings the UI can change. Absent fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdate {
    pub display_name: Option<String>,
    pub server_url: Option<String>,
    pub listening_offset_ms: Option<u32>,
    pub discord_presence: Option<bool>,
    pub others_can_control: Option<bool>,
}

impl Settings {
    /// Apply `update` after checking every field; on error nothing changes.
    pub fn apply(&mut self, update: SettingsUpdate) -> Result<(), AppError> {
        let display_name = match update.display_name {
            Some(name) => {
                let name = name.trim().to_owned();
                let length = name.chars().count();
                if length == 0 || length > MAX_DISPLAY_NAME_CHARS {
                    return Err(AppError::InvalidSetting(format!(
                        "Your name needs 1 to {MAX_DISPLAY_NAME_CHARS} characters."
                    )));
                }
                Some(name)
            }
            None => self.display_name.clone(),
        };
        let server_url = match update.server_url {
            Some(url) => {
                let url = url.trim().to_owned();
                if !(url.starts_with("ws://") || url.starts_with("wss://")) {
                    return Err(AppError::InvalidSetting(
                        "The server address starts with ws:// or wss://.".to_owned(),
                    ));
                }
                url
            }
            None => self.server_url.clone(),
        };
        let listening_offset_ms = match update.listening_offset_ms {
            Some(offset) if offset > MAX_LISTENING_OFFSET_MS => {
                return Err(AppError::InvalidSetting(format!(
                    "The listening offset goes up to {MAX_LISTENING_OFFSET_MS} ms."
                )));
            }
            Some(offset) => offset,
            None => self.listening_offset_ms,
        };
        self.display_name = display_name;
        self.server_url = server_url;
        self.listening_offset_ms = listening_offset_ms;
        if let Some(enabled) = update.discord_presence {
            self.discord_presence = enabled;
        }
        if let Some(allowed) = update.others_can_control {
            self.others_can_control = allowed;
        }
        Ok(())
    }
}

/// What's needed to take a seat back after a restart. Contains the rejoin
/// secret: never log it or send it to the UI.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejoinInfo {
    pub server_url: String,
    pub code: String,
    pub rejoin_secret: String,
}

impl std::fmt::Debug for RejoinInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RejoinInfo")
            .field("server_url", &self.server_url)
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

/// Reads and writes the files in one folder.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Saved settings, or defaults if there are none or they can't be read
    /// (a broken file shouldn't stop the app from starting). New defaults
    /// such as the client ID are saved right away so they stay stable.
    pub fn load_settings(&self) -> Settings {
        match read_json(&self.dir.join(SETTINGS_FILE)) {
            Ok(Some(settings)) => settings,
            Ok(None) => {
                let settings = Settings::default();
                if let Err(error) = self.save_settings(&settings) {
                    warn!(%error, "Could not save the default settings");
                }
                settings
            }
            Err(error) => {
                warn!(%error, "Settings file is unreadable; using defaults");
                Settings::default()
            }
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> io::Result<()> {
        write_json(&self.dir.join(SETTINGS_FILE), settings)
    }

    pub fn load_rejoin(&self) -> Option<RejoinInfo> {
        read_json(&self.dir.join(REJOIN_FILE)).ok().flatten()
    }

    pub fn save_rejoin(&self, info: &RejoinInfo) -> io::Result<()> {
        write_json(&self.dir.join(REJOIN_FILE), info)
    }

    pub fn clear_rejoin(&self) {
        match fs::remove_file(self.dir.join(REJOIN_FILE)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => warn!(%error, "Could not delete the saved session"),
        }
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("attune-settings-{}", new_client_id()));
        (Store::new(dir.clone()), dir)
    }

    #[test]
    fn first_load_saves_defaults_so_the_client_id_stays() {
        let (store, dir) = store();
        let first = store.load_settings();
        assert_eq!(first.server_url, DEFAULT_SERVER_URL);
        assert_eq!(store.load_settings().client_id, first.client_id);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn older_files_get_defaults_for_new_fields() {
        let (store, dir) = store();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SETTINGS_FILE), r#"{"displayName":"Lily"}"#).unwrap();
        let settings = store.load_settings();
        assert_eq!(settings.display_name.as_deref(), Some("Lily"));
        assert_eq!(settings.listening_offset_ms, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_file_falls_back_to_defaults() {
        let (store, dir) = store();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SETTINGS_FILE), "{not json").unwrap();
        assert_eq!(store.load_settings().display_name, None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn updates_are_checked_and_all_or_nothing() {
        let mut settings = Settings::default();
        settings
            .apply(SettingsUpdate {
                display_name: Some("  Lily ".to_owned()),
                ..SettingsUpdate::default()
            })
            .expect("valid");
        assert_eq!(settings.display_name.as_deref(), Some("Lily"));

        let before = settings.clone();
        for update in [
            SettingsUpdate {
                display_name: Some("   ".to_owned()),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                // A valid name with an invalid URL changes neither.
                display_name: Some("Sam".to_owned()),
                server_url: Some("http://example.com".to_owned()),
                ..SettingsUpdate::default()
            },
            SettingsUpdate {
                listening_offset_ms: Some(MAX_LISTENING_OFFSET_MS + 1),
                ..SettingsUpdate::default()
            },
        ] {
            assert!(settings.apply(update).is_err());
            assert_eq!(settings, before);
        }
    }

    #[test]
    fn rejoin_info_round_trips_and_is_deleted() {
        let (store, dir) = store();
        let info = RejoinInfo {
            server_url: DEFAULT_SERVER_URL.to_owned(),
            code: "KWPZ".to_owned(),
            rejoin_secret: "s3cr3t".to_owned(),
        };
        store.save_rejoin(&info).expect("save");
        assert_eq!(store.load_rejoin(), Some(info.clone()));
        assert!(!format!("{info:?}").contains("s3cr3t"));
        store.clear_rejoin();
        assert_eq!(store.load_rejoin(), None);
        store.clear_rejoin(); // already gone: fine
        let _ = fs::remove_dir_all(dir);
    }
}
