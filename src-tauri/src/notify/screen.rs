//! What's in front of the user, where the overlay goes, and the Win32 calls
//! that keep it from ever taking focus: a game that loses focus can drop
//! input, minimise or leave full screen, so the overlay is shown, moved and
//! hidden only with calls that don't activate it.

use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

use crate::settings::Corner;

/// What the user is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Foreground {
    /// Attune's own window: it shows everything itself.
    Attune,
    /// A full-screen app or game (exclusive, or a borderless window that
    /// covers its monitor).
    FullScreen,
    Other,
}

/// The overlay window's size in page pixels: room for the widest pill.
/// It is click-through, so the empty part doesn't get in the way.
pub const OVERLAY_WIDTH: f64 = 360.0;
pub const OVERLAY_HEIGHT: f64 = 52.0;
/// Gap between the overlay and the screen's edges.
const MARGIN: f64 = 14.0;

/// The main window is open (not in the tray or minimised).
pub fn attune_shown<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false)
    })
}

/// A rectangle in physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Where the overlay goes on a monitor: `area` (physical pixels) at
/// `scale` (its DPI / 96).
pub fn corner_rect(area: Rect, scale: f64, corner: Corner) -> Rect {
    let width = (OVERLAY_WIDTH * scale).round() as i32;
    let height = (OVERLAY_HEIGHT * scale).round() as i32;
    let margin = (MARGIN * scale).round() as i32;
    let left = area.x + margin;
    let right = area.x + area.width - margin - width;
    let top = area.y + margin;
    let bottom = area.y + area.height - margin - height;
    let (x, y) = match corner {
        Corner::TopLeft => (left, top),
        Corner::TopRight => (right, top),
        Corner::BottomLeft => (left, bottom),
        Corner::BottomRight => (right, bottom),
    };
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
            MONITOR_DEFAULTTOPRIMARY,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Shell::{
                SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
                QUNS_RUNNING_D3D_FULL_SCREEN,
            },
            WindowsAndMessaging::{
                GetAncestor, GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetShellWindow,
                GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, SetWindowPos, ShowWindow,
                GA_ROOTOWNER, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
                SWP_SHOWWINDOW, SW_HIDE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST,
            },
        },
    };

    fn hwnd_of<R: Runtime>(window: &WebviewWindow<R>) -> Option<HWND> {
        window.hwnd().ok().map(|hwnd| hwnd.0 as HWND)
    }

    pub fn foreground<R: Runtime>(app: &AppHandle<R>) -> Foreground {
        let front = unsafe { GetForegroundWindow() };
        if front.is_null() {
            return Foreground::Other;
        }
        let main = app.get_webview_window("main").and_then(|w| hwnd_of(&w));
        let root = unsafe { GetAncestor(front, GA_ROOTOWNER) };
        if main.is_some_and(|main| main == front || main == root) {
            return Foreground::Attune;
        }
        if full_screen(front) {
            Foreground::FullScreen
        } else {
            Foreground::Other
        }
    }

    fn full_screen(front: HWND) -> bool {
        // What Windows itself uses to hold back notifications: exclusive
        // Direct3D, a full-screen app, presentation mode.
        let mut state = 0;
        if unsafe { SHQueryUserNotificationState(&mut state) } == 0
            && matches!(
                state,
                QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
            )
        {
            return true;
        }
        // Borderless games don't always count for that: a window that
        // covers its whole monitor does. The desktop covers it too.
        if front == unsafe { GetShellWindow() } || front == unsafe { GetDesktopWindow() } {
            return false;
        }
        let mut class = [0u16; 32];
        let len = unsafe { GetClassNameW(front, class.as_mut_ptr(), class.len() as i32) };
        let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
        if class == "Progman" || class == "WorkerW" {
            return false;
        }
        let mut window = NO_RECT;
        if unsafe { GetWindowRect(front, &mut window) } == 0 {
            return false;
        }
        let Some((monitor, _)) =
            monitor_info(unsafe { MonitorFromWindow(front, MONITOR_DEFAULTTOPRIMARY) })
        else {
            return false;
        };
        window.left <= monitor.x
            && window.top <= monitor.y
            && window.right >= monitor.x + monitor.width
            && window.bottom >= monitor.y + monitor.height
    }

    const NO_RECT: RECT = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    fn rect(r: RECT) -> Rect {
        Rect {
            x: r.left,
            y: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
        }
    }

    /// The monitor's whole area and its work area (without the taskbar).
    fn monitor_info(monitor: HMONITOR) -> Option<(Rect, Rect)> {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: NO_RECT,
            rcWork: NO_RECT,
            dwFlags: 0,
        };
        (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0)
            .then(|| (rect(info.rcMonitor), rect(info.rcWork)))
    }

    /// The overlay's place: on the monitor of the window in front (where
    /// the game is), over the taskbar's spot only when it's full screen.
    pub fn placement(front: Foreground, corner: Corner) -> Option<Rect> {
        let window = unsafe { GetForegroundWindow() };
        let monitor = if window.is_null() {
            unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
        } else {
            unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) }
        };
        let (whole, work) = monitor_info(monitor)?;
        let (mut dpi_x, mut dpi_y) = (96, 96);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        let area = if front == Foreground::FullScreen {
            whole
        } else {
            work
        };
        Some(corner_rect(area, f64::from(dpi_x) / 96.0, corner))
    }

    /// Never activated, not in Alt+Tab, above everything. Set again before
    /// each show: tao rewrites the extended style when its own flags change.
    /// tao also sets WS_EX_APPWINDOW, which would put it in Alt+Tab.
    fn keep_out_of_the_way(hwnd: HWND) {
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let wanted = (style | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST) as isize)
                & !(WS_EX_APPWINDOW as isize);
            if wanted != style {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
            }
        }
    }

    /// Move to `at` and show without activating. Twice: moving to a monitor
    /// with another DPI makes Windows resize it once it arrives.
    pub fn show_at<R: Runtime>(window: &WebviewWindow<R>, at: Rect) {
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        keep_out_of_the_way(hwnd);
        for _ in 0..2 {
            unsafe {
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    at.x,
                    at.y,
                    at.width,
                    at.height,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                );
            }
        }
    }

    pub fn hide<R: Runtime>(window: &WebviewWindow<R>) {
        if let Some(hwnd) = hwnd_of(window) {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
        }
    }
}

#[cfg(windows)]
pub use win::{foreground, hide, placement, show_at};

#[cfg(not(windows))]
mod other {
    use super::*;

    pub fn foreground<R: Runtime>(app: &AppHandle<R>) -> Foreground {
        let focused = app
            .get_webview_window("main")
            .is_some_and(|w| w.is_focused().unwrap_or(false));
        if focused {
            Foreground::Attune
        } else {
            Foreground::Other
        }
    }

    pub fn placement(_front: Foreground, _corner: Corner) -> Option<Rect> {
        None
    }

    pub fn show_at<R: Runtime>(window: &WebviewWindow<R>, at: Rect) {
        let _ = window.set_position(tauri::PhysicalPosition::new(at.x, at.y));
        let _ = window.show();
    }

    pub fn hide<R: Runtime>(window: &WebviewWindow<R>) {
        let _ = window.hide();
    }
}

#[cfg(not(windows))]
pub use other::{foreground, hide, placement, show_at};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_keep_a_margin_at_the_monitors_scale() {
        // A 1920×1080 monitor to the right of another, at 150 %.
        let area = Rect {
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let r = corner_rect(area, 1.5, Corner::BottomRight);
        assert_eq!((r.width, r.height), (540, 78));
        assert_eq!(r.x + r.width, 1920 + 1920 - 21);
        assert_eq!(r.y + r.height, 1080 - 21);
        let r = corner_rect(area, 1.0, Corner::TopLeft);
        assert_eq!((r.x, r.y), (1920 + 14, 14));
    }
}
