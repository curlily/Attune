//! A log of this run, kept in memory, for bug reports: Settings → Save log
//! writes it to Downloads.
//!
//! It records what happened in enough detail to replay it: what you did in
//! Attune, what the music app reported, every change the session made or
//! received, sync states, reconnects, and every error you were shown. Once
//! it outgrows [`MAX_BYTES`] the oldest lines go first. It holds no secrets:
//! rejoin secrets are never logged and the server address is left out.

use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use chrono::Local;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tracing_subscriber::{
    fmt::{self, time::ChronoLocal},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Layer,
};

use crate::{error::AppError, state::AppState};

/// Hours of a busy session fit comfortably.
const MAX_BYTES: usize = 16 * 1024 * 1024;
/// More detail than the console: everything Attune itself traces.
const SAVED_FILTER: &str =
    "info,attune_core=debug,attune_lib=debug,chromiumoxide=warn,chromiumoxide::handler=error";
/// The console, while developing (`RUST_LOG` overrides it).
const CONSOLE_FILTER: &str = "warn,attune_core=debug,chromiumoxide::handler=error";

static LOG: OnceLock<Arc<Buffer>> = OnceLock::new();

#[derive(Default)]
struct Buffer {
    inner: Mutex<Lines>,
}

#[derive(Default)]
struct Lines {
    lines: VecDeque<String>,
    bytes: usize,
    /// Older lines were dropped to stay under [`MAX_BYTES`].
    trimmed: bool,
}

impl Buffer {
    fn push(&self, text: String) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.bytes += text.len();
        inner.lines.push_back(text);
        while inner.bytes > MAX_BYTES {
            let Some(oldest) = inner.lines.pop_front() else {
                break;
            };
            inner.bytes -= oldest.len();
            inner.trimmed = true;
        }
    }

    fn contents(&self) -> (String, bool) {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        (
            inner.lines.iter().map(String::as_str).collect(),
            inner.trimmed,
        )
    }
}

struct Writer(Arc<Buffer>);

impl io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.push(String::from_utf8_lossy(bytes).into_owned());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Log to the console and to the in-memory log. Call once, first thing.
pub fn init() {
    let buffer = LOG.get_or_init(Default::default).clone();
    let console_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(CONSOLE_FILTER));
    tracing_subscriber::registry()
        .with(fmt::layer().with_filter(console_filter))
        .with(saved_layer(buffer))
        .init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        started = %Local::now().format("%Y-%m-%d %H:%M:%S %:z"),
        "Attune started"
    );
}

/// The layer that fills `buffer`: local times, no colour codes.
fn saved_layer<S>(buffer: Arc<Buffer>) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fmt::layer()
        .with_ansi(false)
        .with_timer(ChronoLocal::new("%H:%M:%S%.3f".to_owned()))
        .with_writer(move || Writer(buffer.clone()))
        .with_filter(EnvFilter::new(SAVED_FILTER))
}

/// Record a command the page called: what you did in Attune. Lookups the
/// page makes on its own (`get_…`) aren't actions and would drown them out.
pub fn command(name: &str, args: &Value) {
    if name.starts_with("get_") || name == "take_join_link" {
        return;
    }
    let mut args = args.clone();
    if let Some(update) = args.get_mut("update").and_then(Value::as_object_mut) {
        if update.contains_key("serverUrl") {
            update.insert("serverUrl".to_owned(), Value::from("<changed>"));
        }
    }
    tracing::info!(target: "attune_lib::ui", command = name, %args, "You");
}

/// Save the log to Downloads and show it in Explorer. Returns the file's
/// path.
#[tauri::command]
pub async fn save_log(app: AppHandle, state: State<'_, AppState>) -> Result<String, AppError> {
    let (lines, trimmed) = LOG.get().map(|log| log.contents()).unwrap_or_default();
    let mut text = header(&state).await;
    if trimmed {
        text.push_str("(The log was long; its oldest lines were dropped.)\n");
    }
    text.push('\n');
    text.push_str(&lines);

    let now = Local::now();
    let dir = app
        .path()
        .download_dir()
        .or_else(|_| app.path().desktop_dir())
        .or_else(|_| app.path().home_dir())
        .map_err(|error| io::Error::other(error.to_string()))?;
    let path: PathBuf = dir.join(format!(
        "Attune log {}.txt",
        now.format("%Y-%m-%d %H-%M-%S")
    ));
    std::fs::write(&path, text)?;
    tracing::info!(path = %path.display(), "Saved the log");
    if let Err(error) = app.opener().reveal_item_in_dir(&path) {
        tracing::debug!(%error, "Could not show the saved log");
    }
    Ok(path.display().to_string())
}

/// What the log alone wouldn't say: where things stand as it's saved.
async fn header(state: &AppState) -> String {
    let settings = state.settings().clone();
    let provider = match state.controller.lock().await.as_ref() {
        Some(controller) => controller.provider_id().display_name(),
        None => "none",
    };
    let session = match state.session.lock().await.as_ref() {
        Some(runtime) => {
            let status = runtime.status();
            format!(
                "{} ({:?}, {} people, sequence {})",
                status.session_id,
                status.sync,
                status.participants.len(),
                status.sequence
            )
        }
        None => "none".to_owned(),
    };
    format!(
        "Attune {version} log, saved {saved}\n\
         Windows {arch}\n\
         Music app: {provider}\n\
         Session: {session}\n\
         Settings: name {name:?}, listening offset {offset} ms, others can change \
         what's playing {control}, show on Discord {discord}, listen along {along}\n\
         Client ID: {client}\n",
        version = env!("CARGO_PKG_VERSION"),
        saved = Local::now().format("%Y-%m-%d %H:%M:%S %:z"),
        arch = std::env::consts::ARCH,
        name = settings.display_name.unwrap_or_default(),
        offset = settings.listening_offset_ms,
        control = settings.others_can_control,
        discord = settings.discord_presence,
        along = settings.discord_listen_along,
        client = settings.client_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_attunes_detail_and_your_actions() {
        let buffer = Arc::new(Buffer::default());
        let subscriber = tracing_subscriber::registry().with(saved_layer(buffer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "attune_core::session", sequence = 7, "Session change");
            tracing::debug!(target: "somebody_else", "Not ours");
            command("join_session", &serde_json::json!({ "code": "KWPZ" }));
            command("get_session_status", &Value::Null);
            command(
                "update_settings",
                &serde_json::json!({ "update": { "serverUrl": "wss://private.example" } }),
            );
        });
        let (text, _) = buffer.contents();
        assert!(text.contains("Session change sequence=7"), "{text}");
        assert!(!text.contains("Not ours"), "{text}");
        assert!(text.contains(r#"command="join_session""#), "{text}");
        assert!(text.contains("KWPZ"), "{text}");
        assert!(!text.contains("get_session_status"), "{text}");
        assert!(!text.contains("private.example"), "{text}");
        // Local time first, without colour codes.
        let first = text.lines().next().unwrap();
        assert!(
            first.as_bytes()[2] == b':' && !first.contains('\u{1b}'),
            "{first}"
        );
    }

    #[test]
    fn drops_the_oldest_lines_past_the_limit() {
        let buffer = Buffer::default();
        let line = "x".repeat(1024 * 1024 - 1) + "\n";
        for _ in 0..MAX_BYTES / line.len() {
            buffer.push(line.clone());
        }
        assert!(!buffer.contents().1);
        buffer.push("last\n".to_owned());
        let (text, trimmed) = buffer.contents();
        assert!(trimmed);
        assert!(text.len() <= MAX_BYTES);
        assert!(text.ends_with("last\n"));
    }
}
