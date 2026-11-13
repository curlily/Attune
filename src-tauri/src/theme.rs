//! Following Windows' light or dark taskbar with the icons Attune shows
//! there: the window's taskbar icon and the tray icon. (The page follows the
//! apps' light or dark setting by itself, through WebView2.)

use tauri::{image::Image, AppHandle, Manager, Runtime};
use tracing::warn;

use crate::tray;

/// The app icon's tile for a dark taskbar: dark, with a white face.
const DARK_TILE: &[u8] = include_bytes!("../icons/128x128@2x.png");
/// For a light taskbar: white, with a dark face. Rendered from
/// src/lib/Logo.svelte with the colours swapped.
const LIGHT_TILE: &[u8] = include_bytes!("../icons/icon-light.png");

/// Whether Windows draws the taskbar light ("Choose your Windows mode").
/// Elsewhere Attune always uses its dark look.
#[cfg(windows)]
pub fn light_taskbar() -> bool {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_u32("SystemUsesLightTheme"))
        .is_ok_and(|light| light != 0)
}

#[cfg(not(windows))]
pub fn light_taskbar() -> bool {
    false
}

/// Give the window and the tray the icons for the taskbar's current colour.
/// Cheap; called at start and whenever the theme may have changed.
pub fn follow_taskbar<R: Runtime>(app: &AppHandle<R>) {
    let light = light_taskbar();
    tray::refresh_icon(app);
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let tile = if light { LIGHT_TILE } else { DARK_TILE };
    match Image::from_bytes(tile) {
        Ok(icon) => {
            if let Err(error) = window.set_icon(icon) {
                warn!(%error, "Could not set the window icon");
            }
        }
        Err(error) => warn!(%error, "Could not read the window icon"),
    }
}
