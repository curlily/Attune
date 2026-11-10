use std::{
    ffi::OsStr,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
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
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sysinfo::System;
use tokio::sync::broadcast;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::{
    Error::{self, CDPError}, Result, protocol::{SessionMusicState, SessionOperation, SessionSnapshot}, structs::{PlayState, ProviderConfig, QueueState, Track},
};

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

        if !music_client_running(config.debugging_port, &config.page_url).await? {
            // Relaunch music client with debugging port
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
            .or(Err(Error::BrowserConnectionFailed))?;

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
                return Err(CDPError(error));
            }
        };

        let target = match targets.into_iter().find(|target| {
            target.r#type == "page" && target.url.contains(&self.config.page_url)
        }) {
            Some(target) => target,
            None => {
                handler_task.abort();
                return Err(Error::TargetUrlNotFound { url: self.config.page_url.clone() });
            }
        };

        let page = match self.find_page(&browser, &target).await {
            Ok(page) => page,
            Err(error) => {
                handler_task.abort();
                return Err(error.into());
            }
        };

        self.browser = Some(browser);
        self.page = Some(page);
        self.handler_task = Some(handler_task);

        info!(provider = %self.config.display_name, "Connected to provider renderer");
        Ok(())
    }

    async fn find_page(
        &self,
        browser: &Browser,
        target: &TargetInfo,
    ) -> anyhow::Result<Page> {
        let mut last_error = None;

        // Try to get the page a few times with delays, because Chromiumoxide can be slow
        for attempt in 1..=20 {
            match browser.get_page(target.target_id.clone()).await {
                Ok(page) => return Ok(page),
                Err(error) => {
                    last_error = Some(error);

                    tracing::debug!(
                        attempt,
                        target_id = ?target.target_id,
                        "Page not ready yet"
                    );

                    sleep(Duration::from_millis(100)).await;
                }
            }
        }

        Err(anyhow!(
            "Target was found but never became attachable: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        ))
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

    fn page(&self) -> anyhow::Result<&Page> {
        self.page
            .as_ref()
            .ok_or_else(|| anyhow!("not connected to page"))
    }

    pub async fn install_bridge(&mut self) -> Result<()> {
        let mut bridge_rx = self.event_tx.subscribe();

        let page = self.page()?;
        let bridge_source = self.config.bridge_script.clone();

        // Enable the CDP domains used by our event listeners.
        page.execute(PageEnableParams::default())
            .await
            .context("failed to enable Page domain")?;
        page.execute(RuntimeEnableParams::default())
            .await
            .context("failed to enable Runtime domain")?;
        page.execute(LogEnableParams::default())
            .await
            .context("failed to enable Log domain")?;

        // Register listeners before reload.
        let mut binding_events = page
            .event_listener::<EventBindingCalled>()
            .await
            .context("failed to subscribe to CDP binding events")?;
        let mut console_events = page
            .event_listener::<EventConsoleApiCalled>()
            .await
            .context("failed to subscribe to browser console events")?;
        let mut exception_events = page
            .event_listener::<EventExceptionThrown>()
            .await
            .context("failed to subscribe to JavaScript exception events")?;
        let mut log_events = page
            .event_listener::<EventEntryAdded>()
            .await
            .context("failed to subscribe to Chromium log events")?;

        page.execute(AddBindingParams::new("__jamEvent"))
            .await
            .context("failed to install __jamEvent binding")?;

        let script_id = page
            .evaluate_on_new_document(&bridge_source)
            .await
            .context("failed to install bridge init script")?;

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
            return Err(anyhow!(error)
                .context("failed to issue page reload")
                .into());
        }

        info!("Reload requested; waiting for bridge Ready");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    event_task.abort();
                    return Err(anyhow!(
                        "Timed out waiting for bridge Ready"
                    ).into());
                }

                event = bridge_rx.recv() => {
                    match event {
                        Ok(BridgeEvent::Ready) => {
                            info!("Bridge ready");
                            self.event_task = Some(event_task);

                            let version = self
                                .eval_value("window.__MYJAM_BRIDGE_V1__?.version ?? null")
                                .await?;
                            if version.and_then(|value| value.as_u64()) != Some(1) {
                                if let Some(task) = self.event_task.take() {
                                    task.abort();
                                }
                                bail!("provider bridge does not implement MyJam bridge protocol v1");
                            }

                            return Ok(());
                        }

                        Ok(BridgeEvent::Error(error)) => {
                            event_task.abort();
                            return Err(anyhow!(
                                "Bridge initialization failed: {error}"
                            ).into());
                        }

                        Ok(event) => {
                            debug!(?event, "Bridge event before Ready");
                        }

                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(n, "Bridge event receiver lagged");
                        }

                        Err(broadcast::error::RecvError::Closed) => {
                            event_task.abort();
                            return Err(anyhow!(
                                "Bridge event channel closed"
                            ).into());
                        }
                    }
                }
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.event_tx.subscribe()
    }

    pub async fn eval_value(&self, expression: &str) -> Result<Option<Value>> {
        let page = self.page()?;
        let result = page
            .evaluate_expression(expression)
            .await
            .with_context(|| format!("CDP evaluation failed: {expression}"))?;

        Ok(result.into_value::<Value>().ok())
    }

    /// Call a method on the legacy provider bridge API (`window.bridge`).
    /// The CDP transport stays provider-neutral; only the injected script
    /// implements the method's provider-specific behavior.
    async fn call_bridge(&self, method: &str, args: &[Value]) -> Result<Option<Value>> {
        self.call_api("window.bridge", method, args).await
    }

    /// Call a method on the shared protocol API exposed by each provider script.
    async fn call_protocol(&self, method: &str, args: &[Value]) -> Result<Option<Value>> {
        self.call_api("window.__MYJAM_BRIDGE_V1__", method, args).await
    }

    async fn call_api(
        &self,
        api_expression: &str,
        method: &str,
        args: &[Value],
    ) -> Result<Option<Value>> {
        let method_json = serde_json::to_string(method)?;
        let args_json = serde_json::to_string(args)?;
        // Return an explicit envelope so a JS exception is never confused
        // with a successful `undefined` return value from a void method.
        let expression = format!(
            "(async () => {{ try {{ const api = {api_expression}; if (!api) throw new Error('Bridge API is unavailable'); const fn = api[{method_json}]; if (typeof fn !== 'function') throw new Error('Bridge method is unavailable: ' + {method_json}); const value = await fn.apply(api, {args_json}); return {{ __myjamBridgeResult: true, ok: true, value: value === undefined ? null : value }}; }} catch (error) {{ return {{ __myjamBridgeResult: true, ok: false, error: String(error?.stack ?? error) }}; }} }})()"
        );
        let result = self
            .eval_value(&expression)
            .await?
            .context("CDP did not return a bridge result")?;

        if result["__myjamBridgeResult"] != true {
             bail!("CDP returned an invalid bridge response");
        }
        if result["ok"] != true {
            let error = result["error"]
                .as_str()
                .unwrap_or("unknown JavaScript error");
            bail!("provider bridge call failed: {error}");
        }

        Ok(Some(result["value"].clone()))
    }

    async fn call_protocol_value<T: DeserializeOwned>(
        &self,
        method: &str,
        args: &[Value],
    ) -> Result<T> {
        let value = self
            .call_protocol(method, args)
            .await?
            .context("bridge method returned no value")?;
        Ok(serde_json::from_value(value)
            .context("bridge returned an invalid value")?)
    }

    /// Read the current provider state in the shared, provider-independent format.
    pub async fn get_session_state(&self) -> Result<SessionMusicState> {
        self.call_protocol_value("getState", &[]).await
    }

    /// Apply an authoritative session snapshot to the connected music client.
    pub async fn apply_snapshot(&self, snapshot: &SessionSnapshot) -> Result<()> {
        if snapshot.protocol_version != 1 {
            bail!(
                "unsupported session protocol version: {}",
                snapshot.protocol_version
            );
        }
        self.call_protocol("applySnapshot", &[serde_json::to_value(&snapshot.state)?])
            .await?;
        Ok(())
    }

    /// Apply one accepted provider-independent session operation.
    pub async fn apply_operation(&self, operation: &SessionOperation) -> Result<()> {
        self.call_protocol("applyOperation", &[serde_json::to_value(operation)?])
            .await?;
        Ok(())
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
        let result = self
            .call_bridge("getPlayState", &[])
            .await?
            .context("provider bridge returned no play state")?;
        serde_json::from_value(result)
            .context("provider bridge returned an invalid play state")
            .map_err(Into::into)
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

async fn wait_for_cdp(
    debugging_port: u16,
    page_url: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;

    info!("Waiting for CDP...");

    loop {
        if music_client_running(debugging_port, page_url).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::CDPTimeout)
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn launch_music_client(path: &Path, debugging_port: u16) -> Result<()> {
    Command::new(path)
        .arg(format!("--remote-debugging-port={debugging_port}"))
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

async fn music_client_running(debugging_port: u16, page_url: &str) -> Result<bool> {
    let url = format!("http://127.0.0.1:{debugging_port}/json/list");

    let response = match reqwest::get(&url).await {
        Ok(response) => response,
        Err(_) => return Ok(false),
    };

    if !response.status().is_success() {
        return Ok(false);
    }

    let targets: Vec<serde_json::Value> = response.json().await?;

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
        .ok_or_else(|| Error::PathNotFound { path: path.to_path_buf() })?;
    let status = Command::new("taskkill")
        .args(["/IM", exe, "/T", "/F"])
        .status()
        .context("failed to execute taskkill")?;

    // Exit code 128 = The process "TIDAL.exe" was not found.
    if let Some(128) = status.code() {
        info!("No active {exe} process found");
        return Ok(());
    }

    if !status.success() {
        bail!("Taskkill failed with status: {status}");
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
            bail!("{exe} is still running after {} seconds", timeout.as_secs());
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
