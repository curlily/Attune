//! Tray icon: the app icon with a status dot, and a small menu.
//!
//! Left click shows or hides the widget next to the icon. The dot tells the
//! state at a glance without opening anything: green in sync, amber catching
//! up or waiting, red out of sync, grey stepped out, none outside a session.

use std::sync::Mutex;

use attune_core::session::SyncState;
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime,
};
use tracing::warn;

use crate::{commands::session, state::AppState, window};

const TRAY_ID: &str = "main";
const ICON_SIZE: u32 = 32;

/// What the dot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayStatus {
    Idle,
    Synced,
    Busy,
    SteppedOut,
    Error,
}

impl TrayStatus {
    pub fn from_sync(sync: Option<SyncState>) -> Self {
        match sync {
            None | Some(SyncState::Ended) => Self::Idle,
            Some(SyncState::Synced) => Self::Synced,
            Some(
                SyncState::Resyncing | SyncState::Reconnecting | SyncState::WaitingForNextSong,
            ) => Self::Busy,
            Some(SyncState::SteppedOut) => Self::SteppedOut,
            Some(SyncState::Error) => Self::Error,
        }
    }

    fn dot(self) -> Option<[u8; 3]> {
        match self {
            Self::Idle => None,
            Self::Synced => Some([0x53, 0xc9, 0x8f]),
            Self::Busy => Some([0xe6, 0xb3, 0x4f]),
            Self::SteppedOut => Some([0x8a, 0x8e, 0x98]),
            Self::Error => Some([0xf0, 0x74, 0x74]),
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Self::Idle => "Attune",
            Self::Synced => "Attune: in sync",
            Self::Busy => "Attune: catching up",
            Self::SteppedOut => "Attune: stepped out",
            Self::Error => "Attune: out of sync",
        }
    }
}

/// Menu entries whose text or availability follows the session.
pub struct TrayMenu<R: Runtime> {
    follow: MenuItem<R>,
    leave: MenuItem<R>,
    status: Mutex<Option<TrayStatus>>,
}

pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<TrayIcon<R>> {
    let show = MenuItem::with_id(app, "show", "Show Attune", true, None::<&str>)?;
    let follow = MenuItem::with_id(app, "follow", "Step out", false, None::<&str>)?;
    let leave = MenuItem::with_id(app, "leave", "Leave the session", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Attune", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &follow,
            &leave,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    app.manage(TrayMenu {
        follow,
        leave,
        status: Mutex::new(None),
    });

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon(TrayStatus::Idle))
        .tooltip(TrayStatus::Idle.tooltip())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                window::toggle_from_tray(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(tray)
}

fn on_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "show" => window::show(app),
        "quit" => app.exit(0),
        "follow" | "leave" => {
            let app = app.clone();
            let leave = id == "leave";
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                let result = if leave {
                    session::leave(&state).await
                } else {
                    session::toggle_following(&state).await
                };
                if let Err(error) = result {
                    warn!(%error, "Tray action failed");
                }
            });
        }
        _ => {}
    }
}

/// Show `sync` (`None`: not in a session) on the icon and in the menu.
pub fn set_status<R: Runtime>(app: &AppHandle<R>, sync: Option<SyncState>) {
    let status = TrayStatus::from_sync(sync);
    let Some(menu) = app.try_state::<TrayMenu<R>>() else {
        return;
    };
    let in_jam = status != TrayStatus::Idle;
    let rejoin = matches!(sync, Some(SyncState::SteppedOut | SyncState::Error));
    let _ = menu
        .follow
        .set_text(if rejoin { "Resync" } else { "Step out" });
    let _ = menu.follow.set_enabled(in_jam);
    let _ = menu.leave.set_enabled(in_jam);

    let mut last = menu.status.lock().unwrap_or_else(|p| p.into_inner());
    if *last == Some(status) {
        return;
    }
    *last = Some(status);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(icon(status)));
        let _ = tray.set_tooltip(Some(status.tooltip()));
    }
}

fn icon(status: TrayStatus) -> Image<'static> {
    Image::new_owned(draw_icon(status.dot()), ICON_SIZE, ICON_SIZE)
}

/// The app icon at tray size, generated with the others by `tauri icon`.
const ICON_PNG: &[u8] = include_bytes!("../icons/32x32.png");

/// The app icon with an optional status dot in its lower right corner, as
/// ICON_SIZE × ICON_SIZE RGBA.
pub fn draw_icon(dot: Option<[u8; 3]>) -> Vec<u8> {
    let base = Image::from_bytes(ICON_PNG).expect("the bundled tray icon is a valid PNG");
    debug_assert_eq!((base.width(), base.height()), (ICON_SIZE, ICON_SIZE));
    let mut rgba = base.rgba().to_vec();
    let Some(dot) = dot else {
        return rgba;
    };

    let s = ICON_SIZE as f32;
    let (dot_center, dot_radius, ring) = (s * 0.80, s * 0.18, s * 0.06);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((px - dot_center).powi(2) + (py - dot_center).powi(2)).sqrt();
            let index = ((y * ICON_SIZE + x) * 4) as usize;
            // A clear gap around the dot, so it reads on any taskbar colour.
            let keep = (d - dot_radius - ring).clamp(0.0, 1.0);
            let mut alpha = f32::from(rgba[index + 3]) / 255.0 * keep;
            let dot_alpha = (dot_radius - d).clamp(0.0, 1.0);
            if dot_alpha > 0.0 {
                rgba[index..index + 3].copy_from_slice(&dot);
                alpha = alpha.max(dot_alpha);
            }
            rgba[index + 3] = (alpha * 255.0).round() as u8;
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn the_icon_is_the_app_icon_without_a_status() {
        let rgba = draw_icon(None);
        assert_eq!(rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        assert_eq!(pixel(&rgba, 32, 0, 0)[3], 0, "transparent corner");
        assert_eq!(pixel(&rgba, 32, 16, 4)[3], 255, "opaque body");
    }

    #[test]
    fn the_dot_shows_the_status_colour() {
        let green = TrayStatus::Synced.dot();
        let rgba = draw_icon(green);
        assert_eq!(pixel(&rgba, 32, 25, 25), [0x53, 0xc9, 0x8f, 255]);
        assert_eq!(TrayStatus::from_sync(None), TrayStatus::Idle);
        assert_eq!(
            TrayStatus::from_sync(Some(SyncState::WaitingForNextSong)),
            TrayStatus::Busy
        );
    }
}

#[cfg(test)]
mod preview {
    /// Writes the icons as raw RGBA for a visual check:
    /// `ATTUNE_ICON_DUMP=<dir> cargo test -p attune dump_icons -- --ignored`.
    #[test]
    #[ignore]
    fn dump_icons() {
        let dir = std::env::var("ATTUNE_ICON_DUMP").expect("ATTUNE_ICON_DUMP");
        for (name, status) in [
            ("idle", super::TrayStatus::Idle),
            ("synced", super::TrayStatus::Synced),
            ("busy", super::TrayStatus::Busy),
            ("stepped", super::TrayStatus::SteppedOut),
            ("error", super::TrayStatus::Error),
        ] {
            let rgba = super::draw_icon(status.dot());
            std::fs::write(format!("{dir}/{name}.rgba"), rgba).expect("write");
        }
    }
}
