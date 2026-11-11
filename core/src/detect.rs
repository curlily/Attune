//! Which music apps are installed and running, for the connect screen.
//!
//! Connecting needs the app's debug port. An app that is already running
//! with it attaches without interruption; one running without it has to be
//! restarted once, which the UI should say before the user confirms.

use std::{ffi::OsStr, path::Path};

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, RefreshKind, System};

use crate::{
    Result,
    bridge_controller::music_client_running,
    providers,
    structs::{ProviderConfig, ProviderId},
};

/// One provider's state on this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAvailability {
    pub id: ProviderId,
    pub display_name: String,
    /// The desktop app MyJam can launch is installed.
    pub installed: bool,
    pub running: bool,
    /// Running with MyJam's debug port: connecting won't restart it.
    pub ready: bool,
    /// Why MyJam can't use this install, if it can't (shown as is).
    pub unsupported: Option<String>,
}

/// Check every known provider.
pub async fn detect_providers() -> Result<Vec<ProviderAvailability>> {
    let system =
        System::new_with_specifics(RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
        ));
    let mut found = Vec::new();
    for config in [providers::tidal()?, providers::spotify()?] {
        found.push(detect(&system, &config).await);
    }
    Ok(found)
}

async fn detect(system: &System, config: &ProviderConfig) -> ProviderAvailability {
    let exe_name = config
        .executable_path
        .file_name()
        .unwrap_or_else(|| OsStr::new(""));
    let running_from: Vec<&Path> = system
        .processes_by_exact_name(exe_name)
        .filter_map(|process| process.exe())
        .collect();
    let running = system.processes_by_exact_name(exe_name).next().is_some();
    let installed = config.executable_path.is_file();
    let ready = running
        && music_client_running(config.debugging_port, &config.page_url)
            .await
            .unwrap_or(false);

    // Microsoft Store apps live under WindowsApps and are launched through
    // an alias; MyJam hasn't been tested with them.
    let store_install = !installed
        && (running_from.iter().any(|path| is_store_path(path))
            || store_alias(exe_name).is_some_and(|alias| alias.is_file()));
    let unsupported = store_install.then(|| {
        format!(
            "The Microsoft Store version of {} isn't supported yet. Install it from the {} website instead.",
            config.display_name, config.display_name
        )
    });

    ProviderAvailability {
        id: config.id,
        display_name: config.display_name.clone(),
        installed,
        running,
        ready,
        unsupported,
    }
}

fn is_store_path(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("WindowsApps"))
}

/// Where Windows puts the launch alias of a Store app.
fn store_alias(exe_name: &OsStr) -> Option<std::path::PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(
        Path::new(&local)
            .join("Microsoft")
            .join("WindowsApps")
            .join(exe_name),
    )
}

/// The provider's configuration, by ID.
pub fn provider_config(id: ProviderId) -> Result<ProviderConfig> {
    match id {
        ProviderId::Tidal => providers::tidal(),
        ProviderId::Spotify => providers::spotify(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_paths_are_recognised() {
        assert!(is_store_path(Path::new(
            r"C:\Program Files\WindowsApps\SpotifyAB.SpotifyMusic_1.2.3\Spotify.exe"
        )));
        assert!(!is_store_path(Path::new(
            r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe"
        )));
    }
}
