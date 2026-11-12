//! The main window: frameless, moved by dragging its header. It opens as a
//! normal window in the taskbar; its close button (and the tray icon) only
//! hide it to the tray, where Attune keeps running.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow, WindowEvent};
use tracing::warn;

const MAIN: &str = "main";
/// Clicking the tray icon takes focus from the window just before the click
/// arrives; focus lost this recently still counts as "in front".
const FOCUS_GRACE: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct WindowState {
    pinned: AtomicBool,
    focus_lost_at: Mutex<Option<Instant>>,
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window(MAIN)
}

/// Show and focus the window where it was, and let the page play its
/// opening animation.
pub fn show<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = main_window(app) else {
        return;
    };
    if !window.is_visible().unwrap_or(false) {
        let _ = window.emit("window-shown", ());
    }
    if let Err(error) = window
        .show()
        .and_then(|()| window.unminimize())
        .and_then(|()| window.set_focus())
    {
        warn!(%error, "Could not show the window");
    }
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = main_window(app) {
        let _ = window.hide();
    }
}

/// Left click on the tray icon: hide the window if it's in front, otherwise
/// show it (or bring it to the front from behind other windows).
pub fn toggle_from_tray<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = main_window(app) else {
        return;
    };
    let state = app.state::<WindowState>();
    let recently_focused = lock(&state.focus_lost_at).is_some_and(|at| at.elapsed() < FOCUS_GRACE);
    let in_front = window.is_focused().unwrap_or(false) || recently_focused;
    if window.is_visible().unwrap_or(false) && in_front {
        hide(app);
    } else {
        show(app);
    }
}

pub fn set_pinned<R: Runtime>(app: &AppHandle<R>, pinned: bool) {
    app.state::<WindowState>()
        .pinned
        .store(pinned, Ordering::Relaxed);
    if let Some(window) = main_window(app) {
        let _ = window.set_always_on_top(pinned);
    }
}

/// Window events: close hides to the tray; focus changes are noted for the
/// tray toggle.
pub fn on_event<R: Runtime>(window: &tauri::Window<R>, event: &WindowEvent) {
    if window.label() != MAIN {
        return;
    }
    let state = window.app_handle().state::<WindowState>();
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = window.hide();
        }
        WindowEvent::Focused(focused) => {
            *lock(&state.focus_lost_at) = (!focused).then(Instant::now);
        }
        _ => {}
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Pin the window: it stays on top of other windows.
#[tauri::command]
pub fn set_window_pinned(app: AppHandle, pinned: bool) {
    set_pinned(&app, pinned);
}

/// Hide the window to the tray.
#[tauri::command]
pub fn hide_window(app: AppHandle) {
    hide(&app);
}
