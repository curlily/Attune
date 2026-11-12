//! The widget window: frameless, first opened next to the tray icon, moved
//! by dragging its header, and closing only hides it to the tray.
//!
//! It doesn't hide when it loses focus (the user found that disruptive, and
//! it fought with dragging). Once the user has moved it, it stays where they
//! put it instead of jumping back to the tray each time it opens.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, Runtime, WebviewWindow, WindowEvent,
};
use tracing::warn;

const MAIN: &str = "main";
/// Space between the tray icon and the window.
const GAP: i32 = 10;
/// Clicking the tray icon takes focus from the window just before the click
/// arrives; focus lost this recently still counts as "in front".
const FOCUS_GRACE: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct WindowState {
    pinned: AtomicBool,
    focus_lost_at: Mutex<Option<Instant>>,
    /// Where the tray icon was last seen, for the first opening.
    last_icon: Mutex<Option<IconRect>>,
    /// Where we last put the window; a move elsewhere was the user's.
    placed_at: Mutex<Option<(i32, i32)>>,
    /// The user has moved the window: keep it where they put it.
    user_placed: AtomicBool,
}

/// The tray icon's area in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A monitor's usable area (without the taskbar) in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Where the window goes: centred on the icon, on the side away from the
/// taskbar, and always inside the work area.
pub fn place(icon: IconRect, window: (i32, i32), work: Area) -> (i32, i32) {
    let (width, height) = window;
    let icon_center_x = (icon.x + icon.width / 2.0).round() as i32;
    let icon_center_y = icon.y + icon.height / 2.0;
    let work_center_y = f64::from(work.y) + f64::from(work.height) / 2.0;

    let x = icon_center_x - width / 2;
    let y = if icon_center_y > work_center_y {
        // Taskbar at the bottom (the usual case): open above the icon.
        icon.y.round() as i32 - height - GAP
    } else {
        // Taskbar at the top.
        (icon.y + icon.height).round() as i32 + GAP
    };
    let max_x = work.x + work.width - width - GAP;
    let max_y = work.y + work.height - height - GAP;
    (
        x.clamp(work.x + GAP, max_x.max(work.x + GAP)),
        y.clamp(work.y + GAP, max_y.max(work.y + GAP)),
    )
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window(MAIN)
}

fn icon_rect<R: Runtime>(window: &WebviewWindow<R>, rect: Rect) -> IconRect {
    let scale = window.scale_factor().unwrap_or(1.0);
    let position = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    IconRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    }
}

fn move_near_icon<R: Runtime>(window: &WebviewWindow<R>, icon: IconRect) {
    let Ok(size) = window.outer_size() else {
        return;
    };
    let center = (icon.x + icon.width / 2.0, icon.y + icon.height / 2.0);
    let monitor = window
        .monitor_from_point(center.0, center.1)
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let work = monitor.work_area();
    let area = Area {
        x: work.position.x,
        y: work.position.y,
        width: work.size.width as i32,
        height: work.size.height as i32,
    };
    let (x, y) = place(icon, (size.width as i32, size.height as i32), area);
    *lock(&window.state::<WindowState>().placed_at) = Some((x, y));
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Show and focus the window (at its last place), and let the page play
/// its opening animation.
pub fn show<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = main_window(app) else {
        return;
    };
    if !window.is_visible().unwrap_or(false) {
        let state = app.state::<WindowState>();
        if !state.user_placed.load(Ordering::Relaxed) {
            if let Some(icon) = *lock(&state.last_icon) {
                move_near_icon(&window, icon);
            }
        }
        let _ = window.emit("window-shown", ());
    }
    if let Err(error) = window.show().and_then(|()| window.set_focus()) {
        warn!(%error, "Could not show the window");
    }
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = main_window(app) {
        let _ = window.hide();
    }
}

/// Remember where the tray icon is, so the window opens next to it.
pub fn remember_icon<R: Runtime>(app: &AppHandle<R>, rect: Rect) {
    if let Some(window) = main_window(app) {
        *lock(&app.state::<WindowState>().last_icon) = Some(icon_rect(&window, rect));
    }
}

/// Left click on the tray icon: hide the window if it's in front, otherwise
/// show it (or bring it to the front from behind other windows).
pub fn toggle_from_tray<R: Runtime>(app: &AppHandle<R>, rect: Rect) {
    let Some(window) = main_window(app) else {
        return;
    };
    remember_icon(app, rect);
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

/// Window events: close hides to the tray; a move we didn't make means the
/// user placed the window; focus changes are noted for the tray toggle.
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
        WindowEvent::Moved(position)
            if *lock(&state.placed_at) != Some((position.x, position.y)) =>
        {
            state.user_placed.store(true, Ordering::Relaxed);
        }
        _ => {}
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Pin the widget: it stays on top of other windows.
#[tauri::command]
pub fn set_window_pinned(app: AppHandle, pinned: bool) {
    set_pinned(&app, pinned);
}

/// Hide the widget to the tray.
#[tauri::command]
pub fn hide_window(app: AppHandle) {
    hide(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Area = Area {
        x: 0,
        y: 0,
        width: 1920,
        height: 1032, // 48 px taskbar at the bottom
    };

    fn icon(x: f64, y: f64) -> IconRect {
        IconRect {
            x,
            y,
            width: 24.0,
            height: 24.0,
        }
    }

    #[test]
    fn opens_above_a_bottom_taskbar_centred_on_the_icon() {
        let (x, y) = place(icon(1500.0, 1044.0), (380, 640), WORK);
        assert_eq!(x, 1512 - 190);
        assert_eq!(y, 1032 - 640 - GAP);
    }

    #[test]
    fn stays_on_screen_near_the_corner() {
        let (x, _) = place(icon(1890.0, 1044.0), (380, 640), WORK);
        assert_eq!(x, 1920 - 380 - GAP);
    }

    #[test]
    fn opens_below_a_top_taskbar() {
        let work = Area {
            x: 0,
            y: 48,
            width: 1920,
            height: 1032,
        };
        let (_, y) = place(icon(1500.0, 12.0), (380, 640), work);
        assert_eq!(y, 48 + GAP);
    }

    #[test]
    fn a_second_monitor_uses_its_own_area() {
        let work = Area {
            x: 1920,
            y: 0,
            width: 2560,
            height: 1392,
        };
        let (x, y) = place(icon(4300.0, 1404.0), (380, 640), work);
        assert!(x >= 1920 && x + 380 <= 1920 + 2560);
        assert_eq!(y, 1392 - 640 - GAP);
    }
}
