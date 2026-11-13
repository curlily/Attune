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
    #[cfg(windows)]
    set_taskbar_icon(&window, tile);
}

/// Tauri's `set_icon` only sets a window's small icon (title bar, Alt+Tab
/// thumbnails). The taskbar shows the big one; with none set, Windows falls
/// back to its cached icon for the exe file, which outlives icon changes
/// (it showed an old icon long after the exe had the new one). So set the
/// big icon too, at the system's icon size.
#[cfg(windows)]
fn set_taskbar_icon<R: Runtime>(window: &tauri::WebviewWindow<R>, png: &[u8]) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, DestroyIcon, SendMessageW, HICON, ICON_BIG, LR_DEFAULTCOLOR,
        LR_DEFAULTSIZE, WM_SETICON,
    };
    /// The icon set last, destroyed once replaced.
    static CURRENT: AtomicUsize = AtomicUsize::new(0);

    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let Ok(len) = u32::try_from(png.len()) else {
        return;
    };
    // PNG data is a valid icon resource since Windows Vista.
    let icon: HICON = unsafe {
        CreateIconFromResourceEx(
            png.as_ptr(),
            len,
            1, // an icon, not a cursor
            0x0003_0000,
            0,
            0,
            LR_DEFAULTCOLOR | LR_DEFAULTSIZE,
        )
    };
    if icon.is_null() {
        warn!("Could not create the taskbar icon");
        return;
    }
    unsafe {
        SendMessageW(hwnd.0 as _, WM_SETICON, ICON_BIG as usize, icon as isize);
    }
    let previous = CURRENT.swap(icon as usize, Ordering::Relaxed);
    if previous != 0 {
        unsafe {
            DestroyIcon(previous as HICON);
        }
    }
}
