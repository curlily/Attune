//! Live two-provider session: Spotify and YouTube Music in one jam.
//!
//!     cargo run -p attune-core --example live_pair -- daemon ws://127.0.0.1:8787/ws
//!
//! Attaches to both clients, creates a session on Spotify through
//! `SessionRuntime` (the app's code path) and joins it from YouTube Music.
//! Every operation either provider publishes and every status change is
//! logged with a timestamp. Commands from another shell:
//!
//!     cargo run -p attune-core --example live_pair -- state
//!     cargo run -p attune-core --example live_pair -- log
//!     cargo run -p attune-core --example live_pair -- eval spotify <js>
//!     cargo run -p attune-core --example live_pair -- eval youtube <js>
//!     cargo run -p attune-core --example live_pair -- stepout youtube
//!     cargo run -p attune-core --example live_pair -- resync youtube
//!     cargo run -p attune-core --example live_pair -- quit
//!
//! `log` returns (and clears) what happened since the last call.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use attune_core::{
    bridge_controller::{BridgeController, BridgeEvent},
    network::{SessionClientConfig, new_client_id},
    protocol::SessionMusicState,
    providers,
    session::SessionRuntime,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

const CONTROL: &str = "127.0.0.1:9334";

type Log = Arc<Mutex<Vec<String>>>;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn push(log: &Log, line: String) {
    eprintln!("{line}");
    if let Ok(mut log) = log.lock() {
        log.push(line);
    }
}

fn summary(state: &SessionMusicState) -> String {
    let mut out = format!(
        "playing={} position={}ms current={:?}\n",
        state.playback.is_playing, state.playback.position_ms, state.current_entry_id
    );
    for (index, entry) in state.queue.iter().enumerate() {
        out.push_str(&format!(
            "  [{index}] {} | {} — {} | album={:?} duration={:?}\n",
            &entry.entry_id[..8.min(entry.entry_id.len())],
            entry.track.title,
            entry.track.artists.join(", "),
            entry.track.album,
            entry.track.duration_ms,
        ));
    }
    out
}

fn watch_bridge(name: &'static str, controller: &BridgeController, log: Log) {
    let mut receiver = controller.subscribe();
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(BridgeEvent::Operation(operation)) => {
                    let text = serde_json::to_string(&operation).unwrap_or_default();
                    push(
                        &log,
                        format!("{} {name} op {}", now_ms(), &text[..text.len().min(600)]),
                    );
                }
                Ok(BridgeEvent::Error(error)) => {
                    push(&log, format!("{} {name} error {error}", now_ms()));
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    push(&log, format!("{name} lagged {n}"));
                }
                Err(_) => break,
            }
        }
    });
}

fn watch_runtime(name: &'static str, runtime: &SessionRuntime, log: Log) {
    let mut status = runtime.watch_status();
    let status_log = log.clone();
    tokio::spawn(async move {
        let mut last = String::new();
        while status.changed().await.is_ok() {
            let current = status.borrow().clone();
            let text = format!(
                "sync={:?} warnings={:?}",
                current.sync,
                current
                    .warnings
                    .iter()
                    .map(|w| format!("{} missing for {:?}", w.title, w.missing_for))
                    .collect::<Vec<_>>()
            );
            if text != last {
                push(&status_log, format!("{} {name} status {text}", now_ms()));
                last = text;
            }
        }
    });
    let mut notices = runtime.notices();
    tokio::spawn(async move {
        while let Ok(notice) = notices.recv().await {
            push(&log, format!("{} {name} notice {notice:?}", now_ms()));
        }
    });
}

struct Pair {
    spotify: Arc<BridgeController>,
    youtube: Arc<BridgeController>,
    spotify_runtime: SessionRuntime,
    youtube_runtime: SessionRuntime,
    log: Log,
}

impl Pair {
    fn runtime(&self, name: &str) -> Result<&SessionRuntime, String> {
        match name {
            "spotify" => Ok(&self.spotify_runtime),
            "youtube" => Ok(&self.youtube_runtime),
            other => Err(format!("unknown provider {other:?}")),
        }
    }
}

async fn run_command(pair: &Pair, line: &str) -> (String, bool) {
    let (command, argument) = line.split_once(' ').unwrap_or((line, ""));
    let result: Result<String, String> = async {
        match command {
            "state" => {
                let spotify = pair
                    .spotify
                    .get_session_state()
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                let youtube = pair
                    .youtube
                    .get_session_state()
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok(format!(
                    "SPOTIFY {}\nYOUTUBE {}",
                    summary(&spotify),
                    summary(&youtube)
                ))
            }
            "log" => {
                let drained: Vec<String> =
                    std::mem::take(&mut *pair.log.lock().map_err(|_| "lock")?);
                Ok(if drained.is_empty() {
                    "(nothing)\n".to_owned()
                } else {
                    drained.join("\n") + "\n"
                })
            }
            "eval" => {
                let (target, expression) = argument
                    .split_once(' ')
                    .ok_or("eval <spotify|youtube> <js>")?;
                let controller = match target {
                    "spotify" => &pair.spotify,
                    "youtube" => &pair.youtube,
                    other => return Err(format!("unknown provider {other:?}")),
                };
                let value = controller
                    .eval_value(expression)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok(format!(
                    "{}\n",
                    value
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "undefined".to_owned())
                ))
            }
            "stepout" => {
                pair.runtime(argument)?
                    .step_out()
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok(format!("{argument} stepped out\n"))
            }
            "resync" => {
                pair.runtime(argument)?
                    .resync()
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok(format!("{argument} rejoining\n"))
            }
            "quit" => Ok("bye\n".to_owned()),
            other => Err(format!("unknown command {other:?}")),
        }
    }
    .await;
    (
        result.unwrap_or_else(|e| format!("ERROR: {e}\n")),
        command == "quit",
    )
}

async fn daemon(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let log: Log = Arc::default();
    eprintln!("[pair] attaching to Spotify and YouTube Music ...");
    let (spotify, youtube) = tokio::join!(
        BridgeController::initialise(providers::spotify()?),
        BridgeController::initialise(providers::youtube_music()?),
    );
    let spotify = Arc::new(spotify?);
    let youtube = Arc::new(youtube?);
    watch_bridge("spotify", &spotify, log.clone());
    watch_bridge("youtube", &youtube, log.clone());

    eprintln!("[pair] creating the session on Spotify ...");
    let mut host = SessionClientConfig::new(url.to_owned(), new_client_id(), "Live Spotify");
    host.provider = Some("spotify".to_owned());
    let spotify_runtime = SessionRuntime::create(spotify.clone(), host).await?;
    watch_runtime("spotify", &spotify_runtime, log.clone());

    eprintln!("[pair] joining from YouTube Music ...");
    let mut guest = SessionClientConfig::new(url.to_owned(), new_client_id(), "Live YouTube");
    guest.provider = Some("youtube".to_owned());
    let youtube_runtime = SessionRuntime::join(
        youtube.clone(),
        guest,
        spotify_runtime.credentials().clone(),
    )
    .await?;
    watch_runtime("youtube", &youtube_runtime, log.clone());
    eprintln!(
        "[pair] both in session {}; control port {CONTROL}",
        spotify_runtime.status().session_id
    );

    let pair = Pair {
        spotify,
        youtube,
        spotify_runtime,
        youtube_runtime,
        log,
    };
    let listener = TcpListener::bind(CONTROL).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await?;
        let (response, quit) = run_command(&pair, line.trim()).await;
        write.write_all(response.as_bytes()).await?;
        write.shutdown().await?;
        if quit {
            break;
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), pair.youtube_runtime.leave()).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), pair.spotify_runtime.leave()).await;
    Ok(())
}

async fn send(command: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(CONTROL).await?;
    stream.write_all(format!("{command}\n").as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    print!("{response}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, url] if mode == "daemon" => {
            tracing_subscriber::fmt()
                .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
                    "attune_core::session=debug,attune_core=info,warn,chromiumoxide::handler=error".into()
                }))
                .with_writer(std::io::stderr)
                .init();
            daemon(url).await
        }
        [] => Err("usage: live_pair daemon <ws-url> | live_pair <command> [argument]".into()),
        _ => send(&args.join(" ")).await,
    }
}
