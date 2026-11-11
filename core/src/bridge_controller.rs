use std::{
    ffi::OsStr,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use chromiumoxide::cdp::{
    browser_protocol::{
        log::{EnableParams as LogEnableParams, EventEntryAdded},
        page::{EnableParams as PageEnableParams, ReloadParams},
    },
    js_protocol::runtime::{
        AddBindingParams, EnableParams as RuntimeEnableParams, EventBindingCalled,
        EventConsoleApiCalled, EventExceptionThrown,
    },
};
use chromiumoxide::{Browser, Page, cdp::browser_protocol::target::TargetInfo};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sysinfo::System;
use tokio::sync::broadcast;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::{
    Error, Result,
    protocol::{SessionMusicState, SessionOperation, SessionSnapshot},
    structs::{PlayState, ProviderConfig, QueueState, Track},
};

/// Attach a static context to a chromiumoxide failure.
fn cdp(context: &'static str) -> impl FnOnce(chromiumoxide::error::CdpError) -> Error {
    move |source| Error::Cdp { context, source }
}

/// Result of applying session state or an operation to the provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    /// Upcoming entries the provider could not find. They were left out of the
    /// provider's queue but remain in the session.
    pub unavailable_entry_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum BridgeEvent {
    Ready,
    PlaystateChange(PlayState),
    QueueChange(QueueState),
    Operation(SessionOperation),
    Error(String),
}

pub struct BridgeController {
    config: ProviderConfig,
    browser: Option<Browser>,
    page: Option<Page>,
    event_tx: broadcast::Sender<BridgeEvent>,
    handler_task: Option<tokio::task::JoinHandle<()>>,
    event_task: Option<tokio::task::JoinHandle<()>>,
}

impl BridgeController {
    pub async fn initialise(config: ProviderConfig) -> Result<Self> {
        let (event_tx, _) = broadcast::channel(64);

        if music_client_running(config.debugging_port, &config.page_url).await? {
            // Ready to attach.
        } else if debug_port_open(config.debugging_port).await {
            // Started with the debug port, but its page is missing for the
            // moment (e.g. mid-reload). It comes back by itself; restarting
            // here killed the user's client for nothing (seen live).
            info!(provider = %config.display_name, "Debug port open; waiting for the page");
        } else {
            // Not running, or running without the debug port: (re)start it
            // with the port. Interrupts playback once, when MyJam first
            // connects to a normally started client.
            let mut sys = System::default();
            kill_music_client(&mut sys, &config.executable_path).await?;
            launch_music_client(&config.executable_path, config.debugging_port)?;
        }

        // Wait for CDP to be ready
        wait_for_cdp(
            config.debugging_port,
            &config.page_url,
            Duration::from_secs(30),
        )
        .await?;

        let mut controller = Self {
            config,
            browser: None,
            page: None,
            event_tx,
            handler_task: None,
            event_task: None,
        };

        controller.connect_cdp().await?;
        controller.install_bridge().await?;

        Ok(controller)
    }

    pub async fn connect_cdp(&mut self) -> Result<()> {
        let cdp_url = format!("http://127.0.0.1:{}", self.config.debugging_port);
        info!(
            provider = %self.config.display_name,
            provider_id = ?self.config.id,
            cdp_port = self.config.debugging_port,
            "Connecting to provider renderer over CDP"
        );
        let (mut browser, mut handler) = Browser::connect(&cdp_url)
            .await
            .map_err(cdp("connect to the provider browser"))?;

        let handler_task = tokio::spawn(async move {
            while let Some(result) = handler.next().await {
                if let Err(err) = result {
                    error!(
                        error = %err,
                        error_debug = ?err,
                        "Chromiumoxide handler error"
                    );
                }
            }

            error!("Chromiumoxide handler stream ended");
        });

        let targets = match browser.fetch_targets().await {
            Ok(targets) => targets,
            Err(error) => {
                handler_task.abort();
                return Err(cdp("list browser targets")(error));
            }
        };

        let target = match targets
            .into_iter()
            .find(|target| target.r#type == "page" && target.url.contains(&self.config.page_url))
        {
            Some(target) => target,
            None => {
                handler_task.abort();
                return Err(Error::TargetNotFound {
                    url: self.config.page_url.clone(),
                });
            }
        };

        let page = match self.find_page(&browser, &target).await {
            Ok(page) => page,
            Err(error) => {
                handler_task.abort();
                return Err(error);
            }
        };

        self.browser = Some(browser);
        self.page = Some(page);
        self.handler_task = Some(handler_task);

        info!(provider = %self.config.display_name, "Connected to provider renderer");
        Ok(())
    }

    async fn find_page(&self, browser: &Browser, target: &TargetInfo) -> Result<Page> {
        const ATTEMPTS: u32 = 20;

        // Try to get the page a few times with delays, because Chromiumoxide can be slow
        let mut attempt = 1;
        loop {
            match browser.get_page(target.target_id.clone()).await {
                Ok(page) => return Ok(page),
                Err(error) if attempt >= ATTEMPTS => {
                    return Err(cdp("attach to the provider page target")(error));
                }
                Err(_) => {
                    debug!(attempt, target_id = ?target.target_id, "Page not ready yet");
                    attempt += 1;
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    #[allow(dead_code)]
    async fn reconnect(&mut self) -> Result<()> {
        self.page = None;
        self.browser = None;
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        if let Some(task) = self.handler_task.take() {
            task.abort();
        }

        self.connect_cdp().await?;

        info!("Reconnecting to browser");
        Ok(())
    }

    fn page(&self) -> Result<&Page> {
        self.page.as_ref().ok_or(Error::NotConnected)
    }

    pub async fn install_bridge(&mut self) -> Result<()> {
        let mut bridge_rx = self.event_tx.subscribe();

        let page = self.page()?;
        let bridge_source = self.config.bridge_script.clone();

        // Enable the CDP domains used by our event listeners.
        page.execute(PageEnableParams::default())
            .await
            .map_err(cdp("enable the Page domain"))?;
        page.execute(RuntimeEnableParams::default())
            .await
            .map_err(cdp("enable the Runtime domain"))?;
        page.execute(LogEnableParams::default())
            .await
            .map_err(cdp("enable the Log domain"))?;

        // Register listeners before reload.
        let mut binding_events = page
            .event_listener::<EventBindingCalled>()
            .await
            .map_err(cdp("subscribe to CDP binding events"))?;
        let mut console_events = page
            .event_listener::<EventConsoleApiCalled>()
            .await
            .map_err(cdp("subscribe to browser console events"))?;
        let mut exception_events = page
            .event_listener::<EventExceptionThrown>()
            .await
            .map_err(cdp("subscribe to JavaScript exception events"))?;
        let mut log_events = page
            .event_listener::<EventEntryAdded>()
            .await
            .map_err(cdp("subscribe to Chromium log events"))?;

        page.execute(AddBindingParams::new("__jamEvent"))
            .await
            .map_err(cdp("install the __jamEvent binding"))?;

        let script_id = page
            .evaluate_on_new_document(&bridge_source)
            .await
            .map_err(cdp("install the bridge init script"))?;

        info!(?script_id, "Bridge init script registered");

        let event_tx = self.event_tx.clone();

        let event_task = tokio::spawn(async move {
            let (mut binding_open, mut console_open, mut exception_open, mut log_open) =
                (true, true, true, true);

            loop {
                if !(binding_open || console_open || exception_open || log_open) {
                    break;
                }

                tokio::select! {
                    event = binding_events.next(), if binding_open => {
                        match event {
                            Some(event) if event.name == "__jamEvent" => {
                                match serde_json::from_str::<BridgeEvent>(&event.payload) {
                                    Ok(event) => {
                                        debug!(?event, "Bridge event");
                                        let _ = event_tx.send(event);
                                    }
                                    Err(err) => {
                                        error!(
                                            %err,
                                            payload = %event.payload,
                                            "Invalid bridge event"
                                        );
                                    }
                                }
                            }
                            Some(_) => {}
                            None => binding_open = false,
                        }
                    }

                    event = console_events.next(), if console_open => {
                        match event {
                            Some(event) => {
                                let kind = format!("{:?}", event.r#type);

                                match kind.as_str() {
                                    "Error" | "Assert" => {
                                        error!(?event, "Browser console error");
                                    }
                                    "Warning" => {
                                        warn!(?event, "Browser console warning");
                                    }
                                    _ => {
                                        debug!(?event, "Browser console");
                                    }
                                }
                            }
                            None => console_open = false,
                        }
                    }

                    event = exception_events.next(), if exception_open => {
                        match event {
                            Some(event) => {
                                error!(
                                    ?event.exception_details,
                                    "Uncaught JavaScript exception"
                                );
                            }
                            None => exception_open = false,
                        }
                    }

                    event = log_events.next(), if log_open => {
                        match event {
                            Some(event) => {
                                debug!(?event, "Chromium log entry");
                            }
                            None => log_open = false,
                        }
                    }
                }
            }

            warn!("All browser event streams ended");
        });

        // Use the raw CDP reload command rather than Page::reload(), which
        // also performs chromiumoxide's navigation wait. The init script
        // registered above still runs before page scripts/resources.
        info!("Requesting page reload to execute bridge");
        if let Err(error) = page.execute(ReloadParams::default()).await {
            event_task.abort();
            return Err(cdp("reload the provider page")(error));
        }

        info!("Reload requested; waiting for bridge Ready");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    event_task.abort();
                    return Err(Error::Timeout { context: "receive the bridge Ready event" });
                }

                event = bridge_rx.recv() => {
                    match event {
                        Ok(BridgeEvent::Ready) => {
                            info!("Bridge ready");
                            self.event_task = Some(event_task);

                            let version = self
                                .eval_value("window.__MYJAM_BRIDGE_V1__?.version ?? null")
                                .await?
                                .and_then(|value| value.as_u64());
                            if version != Some(1) {
                                if let Some(task) = self.event_task.take() {
                                    task.abort();
                                }
                                return Err(Error::UnsupportedBridgeVersion { found: version });
                            }

                            return Ok(());
                        }

                        Ok(BridgeEvent::Error(error)) => {
                            event_task.abort();
                            return Err(Error::BridgeReported { message: error });
                        }

                        Ok(event) => {
                            debug!(?event, "Bridge event before Ready");
                        }

                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(n, "Bridge event receiver lagged");
                        }

                        Err(broadcast::error::RecvError::Closed) => {
                            event_task.abort();
                            return Err(Error::EventChannelClosed);
                        }
                    }
                }
            }
        }
    }

    /// Which provider this controller is attached to.
    pub fn provider_id(&self) -> crate::structs::ProviderId {
        self.config.id
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.event_tx.subscribe()
    }

    pub async fn eval_value(&self, expression: &str) -> Result<Option<Value>> {
        let page = self.page()?;
        let result = page
            .evaluate_expression(expression)
            .await
            .map_err(cdp("evaluate a renderer expression"))?;

        Ok(result.into_value::<Value>().ok())
    }

    /// Call a method on the legacy provider bridge API (`window.bridge`).
    /// The CDP transport stays provider-neutral; only the injected script
    /// implements the method's provider-specific behavior.
    async fn call_bridge(&self, method: &'static str, args: &[Value]) -> Result<Value> {
        self.call_api("window.bridge", method, args).await
    }

    /// Call a method on the shared protocol API exposed by each provider script.
    async fn call_protocol(&self, method: &'static str, args: &[Value]) -> Result<Value> {
        self.call_api("window.__MYJAM_BRIDGE_V1__", method, args)
            .await
    }

    /// Returns the method's value; a void JavaScript method yields `Value::Null`.
    async fn call_api(
        &self,
        api_expression: &str,
        method: &'static str,
        args: &[Value],
    ) -> Result<Value> {
        let method_json = serde_json::to_string(method)?;
        let args_json = serde_json::to_string(args)?;
        // Return an explicit envelope so a JS exception is never confused
        // with a successful `undefined` return value from a void method.
        // Adapters may attach a string `code` and JSON `details` to an error
        // to report an expected, structured failure (see `bridge_failure`).
        let expression = format!(
            "(async () => {{ try {{ const api = {api_expression}; if (!api) throw new Error('Bridge API is unavailable'); const fn = api[{method_json}]; if (typeof fn !== 'function') throw new Error('Bridge method is unavailable: ' + {method_json}); const value = await fn.apply(api, {args_json}); return {{ __myjamBridgeResult: true, ok: true, value: value === undefined ? null : value }}; }} catch (error) {{ return {{ __myjamBridgeResult: true, ok: false, error: String(error?.stack ?? error), message: String(error?.message ?? error), code: typeof error?.code === 'string' ? error.code : null, details: error?.details ?? null }}; }} }})()"
        );
        let invalid = || Error::InvalidBridgeResponse {
            method,
            source: None,
        };
        let mut result = self.eval_value(&expression).await?.ok_or_else(invalid)?;

        if result["__myjamBridgeResult"] != true {
            return Err(invalid());
        }
        if result["ok"] != true {
            return Err(bridge_failure(method, result));
        }

        Ok(result["value"].take())
    }

    async fn call_protocol_value<T: DeserializeOwned>(
        &self,
        method: &'static str,
        args: &[Value],
    ) -> Result<T> {
        let value = self.call_protocol(method, args).await?;
        parse_bridge_value(method, value)
    }

    /// Read the current provider state in the shared, provider-independent format.
    pub async fn get_session_state(&self) -> Result<SessionMusicState> {
        self.call_protocol_value("getState", &[]).await
    }

    /// Apply an authoritative session snapshot to the connected music client.
    ///
    /// Upcoming entries the provider cannot find are skipped and reported in
    /// the outcome. If the *current* entry cannot be found, nothing changes
    /// and [`Error::TrackUnavailable`] is returned.
    pub async fn apply_snapshot(&self, snapshot: &SessionSnapshot) -> Result<ApplyOutcome> {
        if snapshot.protocol_version != crate::protocol::PROTOCOL_VERSION {
            return Err(Error::UnsupportedProtocolVersion {
                found: snapshot.protocol_version,
            });
        }
        self.call_protocol_value("applySnapshot", &[serde_json::to_value(&snapshot.state)?])
            .await
    }

    /// Apply one accepted provider-independent session operation. Reports
    /// unavailable entries the same way as [`Self::apply_snapshot`].
    pub async fn apply_operation(&self, operation: &SessionOperation) -> Result<ApplyOutcome> {
        self.call_protocol_value("applyOperation", &[serde_json::to_value(operation)?])
            .await
    }

    /// Dispose the renderer adapter. This does not terminate the music client.
    pub async fn dispose(&mut self) -> Result<()> {
        if self.page.is_some() {
            let _ = self.call_protocol("dispose", &[]).await;
        }
        self.page = None;
        self.browser = None;
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        if let Some(task) = self.handler_task.take() {
            task.abort();
        }
        Ok(())
    }

    pub async fn get_playstate(&self) -> Result<PlayState> {
        let value = self.call_bridge("getPlayState", &[]).await?;
        parse_bridge_value("getPlayState", value)
    }

    pub async fn play_track(&self, track: Track) -> Result<()> {
        self.call_bridge("playTrack", &[json!(track)]).await?;

        Ok(())
    }

    pub async fn pause(&self) -> Result<()> {
        self.call_bridge("pause", &[]).await?;

        Ok(())
    }

    pub async fn resume(&self) -> Result<()> {
        self.call_bridge("resume", &[]).await?;

        Ok(())
    }

    pub async fn seek(&self, position_ms: u64) -> Result<()> {
        self.call_bridge("seek", &[json!(position_ms)]).await?;

        Ok(())
    }

    pub async fn next(&self) -> Result<()> {
        self.call_bridge("next", &[]).await?;

        Ok(())
    }

    pub async fn previous(&self) -> Result<()> {
        self.call_bridge("previous", &[]).await?;

        Ok(())
    }

    pub async fn add_to_queue(&self, tracks: Vec<Track>) -> Result<()> {
        self.call_bridge("addToQueue", &[json!(tracks)]).await?;

        Ok(())
    }

    pub async fn move_queue_item(&self, from_index: u16, to_index: u16) -> Result<()> {
        self.call_bridge("moveQueueItem", &[json!(from_index), json!(to_index)])
            .await?;

        Ok(())
    }

    pub async fn remove_from_queue(&self, index: u16) -> Result<()> {
        self.call_bridge("removeFromQueue", &[json!(index)]).await?;

        Ok(())
    }
}

/// Map a failed bridge envelope to a typed error. Adapter error codes are the
/// JS→Rust equivalent of enum variants; unknown codes are plain failures.
fn bridge_failure(method: &'static str, mut result: Value) -> Error {
    fn text(value: &Value, key: &str) -> Option<String> {
        value[key].as_str().map(str::to_owned)
    }

    if result["code"] == "trackUnavailable" {
        let message = text(&result, "message").unwrap_or_else(|| "track unavailable".to_owned());
        return match serde_json::from_value(result["details"]["entryIds"].take()) {
            Ok(entry_ids) => Error::TrackUnavailable { entry_ids, message },
            Err(source) => Error::InvalidBridgeResponse {
                method,
                source: Some(source),
            },
        };
    }

    let message = text(&result, "error").unwrap_or_else(|| "unknown JavaScript error".to_owned());
    Error::BridgeCallFailed { method, message }
}

fn parse_bridge_value<T: DeserializeOwned>(method: &'static str, value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|source| Error::InvalidBridgeResponse {
        method,
        source: Some(source),
    })
}

async fn wait_for_cdp(debugging_port: u16, page_url: &str, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;

    info!("Waiting for CDP...");

    loop {
        if music_client_running(debugging_port, page_url).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Timeout {
                context: "find the provider page on the CDP port",
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn launch_music_client(path: &Path, debugging_port: u16) -> Result<()> {
    // Detach the music client from our stdio: it outlives us, and inherited
    // handles kept our output pipes open after we exited (seen live).
    Command::new(path)
        .arg(format!("--remote-debugging-port={debugging_port}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|source| Error::LaunchFailed {
            path: path.to_path_buf(),
            source,
        })?;

    let executable_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("music client");
    info!("Launching {executable_name} with remote debugging enabled");
    Ok(())
}

/// Whether something answers the CDP HTTP endpoint on `debugging_port`.
async fn debug_port_open(debugging_port: u16) -> bool {
    let url = format!("http://127.0.0.1:{debugging_port}/json/version");
    reqwest::get(&url)
        .await
        .is_ok_and(|response| response.status().is_success())
}

async fn music_client_running(debugging_port: u16, page_url: &str) -> Result<bool> {
    let url = format!("http://127.0.0.1:{debugging_port}/json/list");

    let response = match reqwest::get(&url).await {
        Ok(response) => response,
        Err(_) => return Ok(false),
    };

    if !response.status().is_success() {
        return Ok(false);
    }

    let targets: Vec<serde_json::Value> = response.json().await.map_err(|source| Error::Http {
        context: "read the CDP target list",
        source,
    })?;

    Ok(targets.iter().any(|target| {
        target["type"] == "page"
            && target["url"]
                .as_str()
                .is_some_and(|url| url.contains(page_url))
    }))
}

async fn kill_music_client(sys: &mut System, path: &Path) -> Result<()> {
    let exe = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::InvalidExecutablePath {
            path: path.to_path_buf(),
        })?;
    let status = Command::new("taskkill")
        .args(["/IM", exe, "/T", "/F"])
        .status()
        .map_err(|source| Error::KillFailed {
            exe: exe.to_owned(),
            source,
        })?;

    // Exit code 128 = The process "TIDAL.exe" was not found.
    if let Some(128) = status.code() {
        info!("No active {exe} process found");
        return Ok(());
    }

    if !status.success() {
        return Err(Error::KillCommandFailed {
            exe: exe.to_owned(),
            status,
        });
    }

    let timeout = Duration::from_secs(5);
    let start = Instant::now();

    loop {
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let still_running = sys
            .processes_by_exact_name(OsStr::new(exe))
            .next()
            .is_some();

        if !still_running {
            info!("{exe} process dead");
            return Ok(());
        }

        if start.elapsed() >= timeout {
            return Err(Error::ProcessStillRunning {
                exe: exe.to_owned(),
                timeout_secs: timeout.as_secs(),
            });
        }

        sleep(Duration::from_millis(100)).await;
    }
}

impl Drop for BridgeController {
    fn drop(&mut self) {
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        if let Some(task) = self.handler_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_envelope(code: Value, details: Value) -> Value {
        json!({
            "__myjamBridgeResult": true,
            "ok": false,
            "error": "Error: boom\n    at adapter.js:1:1",
            "message": "boom",
            "code": code,
            "details": details,
        })
    }

    #[test]
    fn track_unavailable_code_maps_to_typed_error() {
        let envelope = failed_envelope(
            json!("trackUnavailable"),
            json!({ "entryIds": ["current", "later"] }),
        );
        match bridge_failure("applySnapshot", envelope) {
            Error::TrackUnavailable { entry_ids, message } => {
                assert_eq!(entry_ids, ["current", "later"]);
                assert_eq!(message, "boom", "uses the message, not the stack");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn malformed_track_unavailable_details_are_an_invalid_response() {
        let envelope = failed_envelope(json!("trackUnavailable"), json!({ "entryIds": "nope" }));
        assert!(matches!(
            bridge_failure("applySnapshot", envelope),
            Error::InvalidBridgeResponse {
                method: "applySnapshot",
                source: Some(_)
            }
        ));
    }

    #[test]
    fn uncoded_and_unknown_codes_are_plain_call_failures() {
        for code in [Value::Null, json!("somethingElse")] {
            match bridge_failure("applyOperation", failed_envelope(code, Value::Null)) {
                Error::BridgeCallFailed { method, message } => {
                    assert_eq!(method, "applyOperation");
                    assert!(
                        message.contains("adapter.js"),
                        "keeps the stack for debugging"
                    );
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn apply_outcome_uses_camel_case() {
        let outcome: ApplyOutcome =
            serde_json::from_value(json!({ "unavailableEntryIds": ["x"] })).expect("deserialize");
        assert_eq!(outcome.unavailable_entry_ids, ["x"]);
    }
}
