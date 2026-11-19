//! The overlay window: transparent, click-through, never focused, above
//! everything. It lives while a session does, so nothing waits for a web
//! view to start; outside a session it's made only for the test
//! notification and closed again after it.
//!
//! The page shows two layers: a *flash* (something just happened) on top
//! of the *rest* (persistent mode only: an ongoing state, or the song). A
//! flash replaces the one before at once and falls back to the rest when
//! its time is up. In flash mode there is no rest, so the window hides.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tracing::{debug, warn};

use super::{
    screen::{self, Foreground, Rect, OVERLAY_HEIGHT, OVERLAY_WIDTH},
    should_show, Flash, Rest,
};
use attune_core::structs::ProviderId;

use crate::{
    settings::{Corner, OverlayMode, OverlaySettings},
    state::AppState,
};

pub const OVERLAY: &str = "overlay";
/// The page's fade-out after a flash's time is up; the window is hidden
/// once it's done (when there's no rest to show).
const FADE_MS: u64 = 220;
/// How often persistent mode checks what's in front: the island follows a
/// game going full screen, or moving to another monitor, this quickly.
const WATCH_EVERY: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct OverlayState {
    /// Increased with every flash; a hide timer for an older one does
    /// nothing.
    flashes: AtomicU64,
    /// A flash is on screen.
    flashing: AtomicBool,
    /// Increased with every session, so a watcher from an old one stops.
    sessions: AtomicU64,
    in_session: AtomicBool,
    /// The page is listening.
    ready: AtomicBool,
    /// A flash to send once it is.
    pending: Mutex<Option<FlashPayload>>,
    /// What the rest layer would show, before the settings have their say.
    rest: Mutex<Option<Rest>>,
    /// The rest layer as last sent to the page.
    sent_rest: Mutex<Option<RestPayload>>,
    /// Where the window is while shown; `None` while hidden.
    placed: Mutex<Option<Rect>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FlashPayload {
    #[serde(flatten)]
    flash: Flash,
    duration_ms: u32,
    corner: Corner,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RestPayload {
    rest: Option<Flash>,
    /// Shown faded (the song, with "dim" on).
    dim: bool,
    corner: Corner,
}

fn state<R: Runtime>(app: &AppHandle<R>) -> &OverlayState {
    if app.try_state::<OverlayState>().is_none() {
        app.manage(OverlayState::default());
    }
    app.state::<OverlayState>().inner()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn overlay_settings<R: Runtime>(app: &AppHandle<R>) -> OverlaySettings {
    app.state::<AppState>().settings().overlay.clone()
}

/// Make the window, hidden. Don't call it on the main thread: building a
/// web view waits for the main thread on Windows.
fn ensure<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    if let Some(window) = app.get_webview_window(OVERLAY) {
        return Some(window);
    }
    let state = state(app);
    state.ready.store(false, Ordering::SeqCst);
    *lock(&state.sent_rest) = None;
    *lock(&state.placed) = None;
    let built = WebviewWindowBuilder::new(app, OVERLAY, WebviewUrl::App("overlay.html".into()))
        .title("Attune overlay")
        .inner_size(OVERLAY_WIDTH, OVERLAY_HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .build();
    match built {
        Ok(window) => {
            // Clicks go to whatever is underneath.
            if let Err(error) = window.set_ignore_cursor_events(true) {
                warn!(%error, "Could not make the overlay click-through");
            }
            Some(window)
        }
        Err(error) => {
            warn!(%error, "Could not create the overlay");
            None
        }
    }
}

/// Show the window at `at`, unless it's there already.
fn place<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>, at: Rect) {
    let mut placed = lock(&state(app).placed);
    if *placed != Some(at) {
        screen::show_at(window, at);
        *placed = Some(at);
    }
}

fn hide<R: Runtime>(app: &AppHandle<R>) {
    let mut placed = lock(&state(app).placed);
    if placed.take().is_some() {
        if let Some(window) = app.get_webview_window(OVERLAY) {
            screen::hide(&window);
        }
    }
}

/// A session started: have the window ready, and in persistent mode keep
/// it shown while the settings allow.
pub fn open_overlay<R: Runtime>(app: &AppHandle<R>) {
    let state = state(app);
    state.in_session.store(true, Ordering::SeqCst);
    let session = state.sessions.fetch_add(1, Ordering::SeqCst) + 1;
    if overlay_settings(app).enabled {
        ensure(app);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut every = tokio::time::interval(WATCH_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let state = self::state(&app);
            if !state.in_session.load(Ordering::SeqCst)
                || state.sessions.load(Ordering::SeqCst) != session
            {
                break;
            }
            refresh(&app);
        }
    });
}

/// The session is over: close the window.
pub fn close_overlay<R: Runtime>(app: &AppHandle<R>) {
    let state = state(app);
    state.in_session.store(false, Ordering::SeqCst);
    state.ready.store(false, Ordering::SeqCst);
    state.flashing.store(false, Ordering::SeqCst);
    *lock(&state.rest) = None;
    *lock(&state.sent_rest) = None;
    *lock(&state.placed) = None;
    if let Some(window) = app.get_webview_window(OVERLAY) {
        let _ = window.destroy();
    }
}

/// What the rest layer should be now (`None`: nothing, as in flash mode).
fn rest_payload(settings: &OverlaySettings, rest: Option<&Rest>) -> RestPayload {
    let rest = rest.filter(|_| settings.enabled && settings.mode == OverlayMode::Persistent);
    RestPayload {
        dim: rest.is_some_and(|rest| rest.song && settings.dim_idle),
        rest: rest.map(|rest| rest.flash.clone()),
        corner: settings.corner,
    }
}

/// Bring the window in line with the rest layer, the settings and what's
/// in front: send the page a changed rest, and show, move or hide the
/// island. A flash on screen is left alone until its time is up.
fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let state = state(app);
    let settings = overlay_settings(app);
    let payload = rest_payload(&settings, lock(&state.rest).as_ref());
    let wanted = payload.rest.is_some();

    if state.ready.load(Ordering::SeqCst) {
        let mut sent = lock(&state.sent_rest);
        if sent.as_ref() != Some(&payload) {
            let _ = app.emit_to(OVERLAY, "overlay-rest", payload.clone());
            *sent = Some(payload);
        }
    }

    let front = screen::foreground(app);
    let show = wanted && should_show(&settings, front, screen::attune_shown(app));
    if show {
        if let (Some(window), Some(at)) = (ensure(app), screen::placement(front, settings.corner)) {
            place(app, &window, at);
        }
    } else if !state.flashing.load(Ordering::SeqCst) {
        hide(app);
    }
}

/// Persistent mode's base changed (a new song, a new state): show it now
/// rather than at the next check.
pub fn set_rest<R: Runtime>(app: &AppHandle<R>, rest: Option<Rest>) {
    *lock(&state(app).rest) = rest;
    if state(app).in_session.load(Ordering::SeqCst) {
        refresh(app);
    }
}

/// Show `flash` now, over whatever is there.
pub fn show<R: Runtime>(
    app: &AppHandle<R>,
    settings: &OverlaySettings,
    front: Foreground,
    flash: Flash,
) {
    let state = state(app);
    let shown = state.flashes.fetch_add(1, Ordering::SeqCst) + 1;
    state.flashing.store(true, Ordering::SeqCst);
    let Some(window) = ensure(app) else {
        return;
    };
    let Some(at) = screen::placement(front, settings.corner) else {
        return;
    };
    debug!(text = %flash.text, "Overlay");
    place(app, &window, at);
    let payload = FlashPayload {
        flash,
        duration_ms: settings.flash_ms,
        corner: settings.corner,
    };
    if state.ready.load(Ordering::SeqCst) {
        let _ = app.emit_to(OVERLAY, "overlay-flash", payload);
    } else {
        *lock(&state.pending) = Some(payload);
    }

    let app = app.clone();
    let done_after = Duration::from_millis(u64::from(settings.flash_ms) + FADE_MS);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(done_after).await;
        let state = self::state(&app);
        if state.flashes.load(Ordering::SeqCst) != shown {
            return; // a newer one is up
        }
        state.flashing.store(false, Ordering::SeqCst);
        if state.in_session.load(Ordering::SeqCst) {
            refresh(&app); // back to the island, or hidden
        } else {
            close_overlay(&app);
        }
    });
}

/// The settings changed: apply them to the island at once.
pub fn settings_changed<R: Runtime>(app: &AppHandle<R>) {
    if state(app).in_session.load(Ordering::SeqCst) {
        refresh(app);
    }
}

/// The overlay page is listening: send it what's waiting. Returns the
/// connected music app, for the Match theme (`provider-changed` follows
/// changes). Like `get_providers`, it doesn't wait for a connect in
/// progress (that holds the lock for seconds), only for other commands'
/// brief holds. A connect may have announced its app already, before this
/// page listened: the page is told once the connect lets go.
#[tauri::command]
pub async fn overlay_ready(app: AppHandle) -> Option<ProviderId> {
    let app_state = app.state::<AppState>();
    let provider = {
        let found = match app_state.controller.try_lock() {
            Ok(slot) => slot.as_ref().map(|controller| controller.provider_id()),
            Err(_) if app_state.connecting().is_some() => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let connected = app
                        .state::<AppState>()
                        .controller
                        .lock()
                        .await
                        .as_ref()
                        .map(|controller| controller.provider_id());
                    let _ = app.emit_to(OVERLAY, "provider-changed", connected);
                });
                None
            }
            Err(_) => app_state
                .controller
                .lock()
                .await
                .as_ref()
                .map(|controller| controller.provider_id()),
        };
        found
    };
    let state = state(&app);
    state.ready.store(true, Ordering::SeqCst);
    *lock(&state.sent_rest) = None;
    refresh(&app);
    if let Some(payload) = lock(&state.pending).take() {
        let _ = app.emit_to(OVERLAY, "overlay-flash", payload);
    }
    provider
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Tone;

    fn song() -> Rest {
        Rest {
            flash: Flash {
                icon: "play",
                text: "Song — Artist".to_owned(),
                tone: Tone::Info,
                cover: None,
            },
            song: true,
        }
    }

    #[test]
    fn the_rest_layer_is_only_for_persistent_mode() {
        let flash_mode = OverlaySettings::default();
        assert_eq!(rest_payload(&flash_mode, Some(&song())).rest, None);

        let persistent = OverlaySettings {
            mode: OverlayMode::Persistent,
            ..OverlaySettings::default()
        };
        let payload = rest_payload(&persistent, Some(&song()));
        assert_eq!(payload.rest.unwrap().text, "Song — Artist");
        assert!(payload.dim, "the song is dimmed by default");

        let bright = OverlaySettings {
            dim_idle: false,
            ..persistent.clone()
        };
        assert!(!rest_payload(&bright, Some(&song())).dim);

        let off = OverlaySettings {
            enabled: false,
            ..persistent
        };
        assert_eq!(rest_payload(&off, Some(&song())).rest, None);
    }
}
