//! Live adapter test harness (manual testing against a running provider).
//!
//! Start the daemon once; it attaches to the provider and keeps the bridge:
//!
//!     cargo run -p jam-core --example live -- daemon tidal
//!
//! Then send commands from another shell:
//!
//!     cargo run -p jam-core --example live -- state
//!     cargo run -p jam-core --example live -- events
//!     cargo run -p jam-core --example live -- test-partial
//!     cargo run -p jam-core --example live -- test-missing-current
//!     cargo run -p jam-core --example live -- op '{"type":"playback.setPlaying","isPlaying":false}'
//!     cargo run -p jam-core --example live -- quit
//!
//! `events` returns (and clears) the bridge events seen since the last call.

use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use jam_core::{
    bridge_controller::{BridgeController, BridgeEvent},
    protocol::{QueueEntry, SessionMusicState, SessionOperation, SessionSnapshot, TrackDescriptor},
    providers,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

const CONTROL: &str = "127.0.0.1:9333";

type Events = Arc<Mutex<Vec<String>>>;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn ghost(entry_id: &str) -> QueueEntry {
    QueueEntry {
        entry_id: entry_id.to_owned(),
        track: TrackDescriptor {
            title: "Zzqxv Nonexistent Song 9f3a".to_owned(),
            artists: vec!["Nobody Atall Qqv".to_owned()],
            isrc: None,
            album: None,
            duration_ms: None,
        },
    }
}

fn summary(state: &SessionMusicState) -> String {
    let mut out = format!(
        "playing={} position={}ms current={:?}\n",
        state.playback.is_playing, state.playback.position_ms, state.current_entry_id
    );
    for (index, entry) in state.queue.iter().enumerate() {
        out.push_str(&format!(
            "  [{index}] {} | {} — {} | isrc={:?} album={:?} duration={:?}\n",
            entry.entry_id,
            entry.track.title,
            entry.track.artists.join(", "),
            entry.track.isrc,
            entry.track.album,
            entry.track.duration_ms,
        ));
    }
    out
}

fn snapshot(state: SessionMusicState) -> SessionSnapshot {
    SessionSnapshot {
        protocol_version: 1,
        session_id: "live-test".to_owned(),
        revision: 0,
        updated_at_ms: now_ms(),
        state,
    }
}

async fn run_command(controller: &BridgeController, events: &Events, line: &str) -> (String, bool) {
    let (command, argument) = line.split_once(' ').unwrap_or((line, ""));
    let result: Result<String, String> = async {
        match command {
            "state" => {
                let state = controller.get_session_state().await.map_err(|e| format!("{e:?}"))?;
                Ok(summary(&state))
            }
            "events" => {
                let drained: Vec<String> = std::mem::take(&mut *events.lock().map_err(|_| "lock")?);
                Ok(if drained.is_empty() {
                    "(no events)\n".to_owned()
                } else {
                    drained.join("\n") + "\n"
                })
            }
            "op" => {
                let operation: SessionOperation = serde_json::from_str(argument).map_err(|e| e.to_string())?;
                let outcome = controller.apply_operation(&operation).await.map_err(|e| format!("{e:?}"))?;
                Ok(format!("outcome: {outcome:?}\n"))
            }
            "test-partial" => {
                // Current queue with a song TIDAL can't have inserted after
                // the current entry.
                let mut state = controller.get_session_state().await.map_err(|e| format!("{e:?}"))?;
                state.queue.truncate(3);
                let before = summary(&state);
                state.queue.insert(1.min(state.queue.len()), ghost("live-ghost-upcoming"));
                if state.current_entry_id.is_none() {
                    return Err("queue is empty; queue some songs first".to_owned());
                }
                let started = std::time::Instant::now();
                let outcome = controller.apply_snapshot(&snapshot(state)).await;
                let elapsed = started.elapsed();
                let after = controller.get_session_state().await.map_err(|e| format!("{e:?}"))?;
                Ok(format!(
                    "BEFORE\n{before}\nOUTCOME ({elapsed:?}): {outcome:?}\n\nAFTER\n{}",
                    summary(&after)
                ))
            }
            "test-missing-current" => {
                let full = controller.get_session_state().await.map_err(|e| format!("{e:?}"))?;
                // Compare against the whole provider queue: nothing may change.
                let before = summary(&full);
                let mut state = full;
                state.queue.truncate(3);
                state.queue.insert(0, ghost("live-ghost-current"));
                state.current_entry_id = Some("live-ghost-current".to_owned());
                let started = std::time::Instant::now();
                let outcome = controller.apply_snapshot(&snapshot(state)).await;
                let elapsed = started.elapsed();
                let after = controller.get_session_state().await.map_err(|e| format!("{e:?}"))?;
                let unchanged = summary(&after).lines().skip(1).eq(before.lines().skip(1));
                Ok(format!(
                    "BEFORE\n{before}\nOUTCOME ({elapsed:?}): {outcome:?}\n\nAFTER (queue unchanged: {unchanged})\n{}",
                    summary(&after)
                ))
            }
            "eval" => {
                let value = controller.eval_value(argument).await.map_err(|e| format!("{e:?}"))?;
                Ok(format!("{}\n", value.map(|v| v.to_string()).unwrap_or_else(|| "undefined".to_owned())))
            }
            "quit" => Ok("bye\n".to_owned()),
            other => Err(format!("unknown command {other:?}")),
        }
    }
    .await;
    let quit = command == "quit";
    (result.unwrap_or_else(|e| format!("ERROR: {e}\n")), quit)
}

async fn daemon(provider: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = match provider {
        "tidal" => providers::tidal()?,
        "spotify" => providers::spotify()?,
        other => return Err(format!("unknown provider {other:?}").into()),
    };
    eprintln!("[live] attaching to {provider} ...");
    let controller = BridgeController::initialise(config).await?;
    eprintln!("[live] bridge ready; control port {CONTROL}");

    let events: Events = Arc::default();
    let mut receiver = controller.subscribe();
    let sink = events.clone();
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(BridgeEvent::Operation(operation)) => {
                    let text = serde_json::to_string(&operation).unwrap_or_default();
                    if let Ok(mut events) = sink.lock() {
                        events.push(format!("{} Operation {text}", now_ms()));
                    }
                }
                Ok(BridgeEvent::Error(error)) => {
                    if let Ok(mut events) = sink.lock() {
                        events.push(format!("{} Error {error}", now_ms()));
                    }
                }
                Ok(_) => {} // legacy playstate/queue events
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    if let Ok(mut events) = sink.lock() {
                        events.push(format!("lagged {n}"));
                    }
                }
                Err(_) => break,
            }
        }
    });

    let listener = TcpListener::bind(CONTROL).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await?;
        let (response, quit) = run_command(&controller, &events, line.trim()).await;
        write.write_all(response.as_bytes()).await?;
        write.shutdown().await?;
        if quit {
            break;
        }
    }
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
        [mode, provider] if mode == "daemon" => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    std::env::var("RUST_LOG").unwrap_or_else(|_| "jam_core=info".into()),
                )
                .with_writer(std::io::stderr)
                .init();
            daemon(provider).await
        }
        [] => Err("usage: live daemon <tidal|spotify> | live <command> [argument]".into()),
        _ => send(&args.join(" ")).await,
    }
}
