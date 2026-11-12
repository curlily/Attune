//! Live two-provider session, e.g. Spotify and YouTube Music in one jam.
//!
//!     cargo run -p attune-core --example live_pair -- daemon ws://127.0.0.1:8787/ws [<host> <guest>]
//!
//! Providers are `spotify`, `youtube` and `tidal` (default: spotify youtube).
//! Attaches to both clients, creates a session on the host through
//! `SessionRuntime` (the app's code path) and joins it from the guest.
//! Every operation either provider publishes and every status change is
//! logged with a timestamp. Commands from another shell:
//!
//!     cargo run -p attune-core --example live_pair -- state
//!     cargo run -p attune-core --example live_pair -- log
//!     cargo run -p attune-core --example live_pair -- eval <provider> <js>
//!     cargo run -p attune-core --example live_pair -- stepout <provider>
//!     cargo run -p attune-core --example live_pair -- resync <provider>
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
    structs::ProviderConfig,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

const CONTROL: &str = "127.0.0.1:9334";

type Log = Arc<Mutex<Vec<String>>>;
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

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

/// One participant: its provider and its session runtime.
struct Member {
    name: &'static str,
    bridge: Arc<BridgeController>,
    runtime: SessionRuntime,
}

struct Pair {
    members: Vec<Member>,
    log: Log,
}

impl Pair {
    fn member(&self, name: &str) -> Result<&Member, String> {
        self.members
            .iter()
            .find(|member| member.name == name)
            .ok_or_else(|| format!("unknown provider {name:?}"))
    }
}

fn provider(name: &str) -> Outcome<(&'static str, ProviderConfig)> {
    Ok(match name {
        "spotify" => ("spotify", providers::spotify()?),
        "youtube" => ("youtube", providers::youtube_music()?),
        "tidal" => ("tidal", providers::tidal()?),
        other => return Err(format!("unknown provider {other:?}").into()),
    })
}

async fn run_command(pair: &Pair, line: &str) -> (String, bool) {
    let (command, argument) = line.split_once(' ').unwrap_or((line, ""));
    let result: Result<String, String> = async {
        match command {
            "state" => {
                let mut out = String::new();
                for member in &pair.members {
                    let state = member
                        .bridge
                        .get_session_state()
                        .await
                        .map_err(|e| format!("{e:?}"))?;
                    out.push_str(&format!(
                        "{} {}\n",
                        member.name.to_uppercase(),
                        summary(&state)
                    ));
                }
                Ok(out)
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
                let (target, expression) =
                    argument.split_once(' ').ok_or("eval <provider> <js>")?;
                let value = pair
                    .member(target)?
                    .bridge
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
                pair.member(argument)?
                    .runtime
                    .step_out()
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok(format!("{argument} stepped out\n"))
            }
            "resync" => {
                pair.member(argument)?
                    .runtime
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

async fn daemon(url: &str, host: &str, guest: &str) -> Outcome<()> {
    let log: Log = Arc::default();
    let (host_name, host_config) = provider(host)?;
    let (guest_name, guest_config) = provider(guest)?;
    eprintln!("[pair] attaching to {host_name} and {guest_name} ...");
    let (host_bridge, guest_bridge) = tokio::join!(
        BridgeController::initialise(host_config),
        BridgeController::initialise(guest_config),
    );
    let host_bridge = Arc::new(host_bridge?);
    let guest_bridge = Arc::new(guest_bridge?);
    watch_bridge(host_name, &host_bridge, log.clone());
    watch_bridge(guest_name, &guest_bridge, log.clone());

    eprintln!("[pair] creating the session on {host_name} ...");
    let mut config =
        SessionClientConfig::new(url.to_owned(), new_client_id(), format!("Live {host_name}"));
    config.provider = Some(host_name.to_owned());
    let host_runtime = SessionRuntime::create(host_bridge.clone(), config).await?;
    watch_runtime(host_name, &host_runtime, log.clone());

    eprintln!("[pair] joining from {guest_name} ...");
    let joined_at = now_ms();
    let mut config = SessionClientConfig::new(
        url.to_owned(),
        new_client_id(),
        format!("Live {guest_name}"),
    );
    config.provider = Some(guest_name.to_owned());
    let guest_runtime = SessionRuntime::join(
        guest_bridge.clone(),
        config,
        host_runtime.credentials().clone(),
    )
    .await?;
    watch_runtime(guest_name, &guest_runtime, log.clone());
    eprintln!(
        "[pair] both in session {} (join took {} ms); control port {CONTROL}",
        host_runtime.status().session_id,
        now_ms() - joined_at
    );

    let pair = Pair {
        members: vec![
            Member {
                name: host_name,
                bridge: host_bridge,
                runtime: host_runtime,
            },
            Member {
                name: guest_name,
                bridge: guest_bridge,
                runtime: guest_runtime,
            },
        ],
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
    for member in pair.members.into_iter().rev() {
        let _ = tokio::time::timeout(Duration::from_secs(5), member.runtime.leave()).await;
    }
    Ok(())
}

async fn send(command: &str) -> Outcome<()> {
    let mut stream = TcpStream::connect(CONTROL).await?;
    stream.write_all(format!("{command}\n").as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    print!("{response}");
    Ok(())
}

#[tokio::main]
async fn main() -> Outcome<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("daemon") {
        let (url, host, guest) = match &args[1..] {
            [url] => (url.as_str(), "spotify", "youtube"),
            [url, host, guest] => (url.as_str(), host.as_str(), guest.as_str()),
            _ => return Err("usage: live_pair daemon <ws-url> [<host> <guest>]".into()),
        };
        tracing_subscriber::fmt()
            .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
                "attune_core::session=debug,attune_core=info,warn,chromiumoxide::handler=error"
                    .into()
            }))
            .with_writer(std::io::stderr)
            .init();
        return daemon(url, host, guest).await;
    }
    if args.is_empty() {
        return Err(
            "usage: live_pair daemon <ws-url> [<host> <guest>] | live_pair <command> [argument]"
                .into(),
        );
    }
    send(&args.join(" ")).await
}
