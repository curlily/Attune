//! `attune://join/KWPZ` links and starting with Windows.
//!
//! A link opens Attune (or, through the single-instance plugin, reaches the
//! copy already running) and hands the join code to the page, which joins
//! as soon as it can. The code is kept until the page takes it, so a link
//! that started Attune isn't lost before the page is listening.

use std::sync::Mutex;

use attune_core::protocol::normalize_join_code;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_autostart::ManagerExt;

use crate::{error::AppError, window};

const SCHEME: &str = "attune://";

#[derive(Default)]
pub struct PendingJoin(Mutex<Option<String>>);

/// The join code in a `attune://join/<code>` link.
pub fn join_code(link: &str) -> Option<String> {
    let rest = link.trim().strip_prefix(SCHEME)?;
    let code = rest.strip_prefix("join/")?.trim_end_matches('/');
    normalize_join_code(code)
}

/// Handle command-line arguments or opened URLs that may be join links.
pub fn handle<'a, R: Runtime>(app: &AppHandle<R>, candidates: impl IntoIterator<Item = &'a str>) {
    let Some(code) = candidates.into_iter().find_map(join_code) else {
        return;
    };
    *app.state::<PendingJoin>()
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(code.clone());
    window::show(app);
    let _ = app.emit("join-link", code);
}

/// A join code from a link that the page hasn't handled yet.
#[tauri::command]
pub fn take_join_link(pending: State<'_, PendingJoin>) -> Option<String> {
    pending.0.lock().unwrap_or_else(|p| p.into_inner()).take()
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> Result<bool, AppError> {
    app.autolaunch()
        .is_enabled()
        .map_err(|error| AppError::Autostart(error.to_string()))
}

/// Start Attune (hidden in the tray) when Windows starts.
#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, AppError> {
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|error| AppError::Autostart(error.to_string()))?;
    Ok(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_links_give_their_code() {
        assert_eq!(join_code("attune://join/KWPZ"), Some("KWPZ".to_owned()));
        assert_eq!(join_code("attune://join/kwpz/"), Some("KWPZ".to_owned()));
        for other in [
            "attune://join/KWPO",
            "attune://leave/KWPZ",
            "https://join/KWPZ",
            "C:\\Program Files\\Attune\\Attune.exe",
            "--hidden",
        ] {
            assert_eq!(join_code(other), None, "{other}");
        }
    }
}
