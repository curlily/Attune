//! Live end-to-end check: a real provider in a real session.
//!
//!     cargo run -p attune-core --example live_session -- tidal ws://host:8787/ws
//!
//! Attaches to the running provider and creates a session through
//! `SessionRuntime` (the app's code path). A scripted headless participant
//! then joins and makes changes; after each step the provider's state is
//! printed. Operations the provider publishes (e.g. a song ending) are
//! printed as the headless participant receives them.

use std::{sync::Arc, time::Duration};

use attune_core::{
    bridge_controller::BridgeController,
    network::{SessionClient, SessionClientConfig, SessionEvent, SubmitOutcome, new_client_id},
    protocol::{QueueEntry, SessionMusicState, SessionOperation, TrackDescriptor},
    providers,
    session::SessionRuntime,
};
use tokio::{sync::broadcast, time::sleep};

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn line(state: &SessionMusicState) -> String {
    let titles: Vec<String> = state
        .queue
        .iter()
        .take(4)
        .map(|entry| format!("{} — {}", entry.track.title, entry.track.artists.join(", ")))
        .collect();
    format!(
        "playing={} pos={}ms len={} | {}",
        state.playback.is_playing,
        state.playback.position_ms,
        state.queue.len(),
        titles.join(" | ")
    )
}

/// Headless participant that tracks the session sequence.
struct Peer {
    client: SessionClient,
    events: broadcast::Receiver<SessionEvent>,
    applied: u64,
}

impl Peer {
    fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                SessionEvent::RemoteOperation {
                    sequence,
                    origin_client_id,
                    operation,
                    ..
                } => {
                    self.applied = sequence;
                    let origin = if origin_client_id.is_some() {
                        "provider"
                    } else {
                        "server"
                    };
                    let text = match &operation {
                        SessionOperation::QueueReplace {
                            entries,
                            current_entry_id,
                        } => format!(
                            "queue.replace current={current_entry_id:?} [{}]",
                            entries
                                .iter()
                                .take(6)
                                .map(|e| format!(
                                    "{}:{}",
                                    &e.entry_id[..8.min(e.entry_id.len())],
                                    e.track.title
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    println!(
                        "    peer received #{sequence} from {origin}: {}",
                        &text[..text.len().min(400)]
                    );
                }
                SessionEvent::Participants(participants) => {
                    for p in participants {
                        if !p.unavailable_entry_ids.is_empty()
                            || p.status != attune_core::protocol::ParticipantStatus::Synced
                        {
                            println!(
                                "    participant {} status={:?} unavailable={:?}",
                                p.display_name, p.status, p.unavailable_entry_ids
                            );
                        }
                    }
                }
                other => println!("    peer event: {other:?}"),
            }
        }
    }

    async fn submit(&mut self, operation: SessionOperation) -> Outcome {
        self.drain();
        match self
            .client
            .submit_operation(operation, self.applied)
            .await?
        {
            SubmitOutcome::Applied { sequence, .. } => {
                self.applied = sequence;
                println!("    accepted as #{sequence}");
            }
            other => println!("    outcome: {other:?}"),
        }
        Ok(())
    }
}

async fn step(
    title: &str,
    peer: &mut Peer,
    provider: &BridgeController,
    runtime: &SessionRuntime,
    wait: Duration,
) -> Outcome {
    sleep(wait).await;
    peer.drain();
    let state = provider.get_session_state().await?;
    let status = runtime.status();
    println!("  after {title}: [{:?}] {}", status.sync, line(&state));
    for warning in &status.warnings {
        println!(
            "    warning: {} missing for {:?} ({} ahead)",
            warning.title, warning.missing_for, warning.songs_ahead
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Outcome {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "attune_core::session=info,warn,chromiumoxide::handler=error".into()
        }))
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let [provider_name, url] = args.as_slice() else {
        return Err("usage: live_session <tidal|spotify|youtube> <ws-url>".into());
    };
    let config = match provider_name.as_str() {
        "tidal" => providers::tidal()?,
        "spotify" => providers::spotify()?,
        "youtube" => providers::youtube_music()?,
        other => return Err(format!("unknown provider {other}").into()),
    };

    println!("attaching to {provider_name} ...");
    let provider = Arc::new(BridgeController::initialise(config).await?);
    println!("creating session (waits for the provider to settle) ...");
    let mut app_config = SessionClientConfig::new(url.clone(), new_client_id(), "Live TIDAL");
    app_config.provider = Some(provider_name.clone());
    let runtime = SessionRuntime::create(provider.clone(), app_config).await?;
    let mut notices = runtime.notices();
    println!(
        "  session {} with {} songs",
        runtime.status().session_id,
        runtime.status().queue.len()
    );

    let (client, start) = SessionClient::join(
        SessionClientConfig::new(url.clone(), new_client_id(), "Script"),
        runtime.credentials().clone(),
    )
    .await?;
    let ids: Vec<String> = start
        .snapshot
        .state
        .queue
        .iter()
        .map(|e| e.entry_id.clone())
        .collect();
    let mut peer = Peer {
        client,
        events: start.events,
        applied: start.snapshot.revision,
    };
    let short = Duration::from_millis(1500);
    let long = Duration::from_secs(4);
    step("join", &mut peer, &provider, &runtime, short).await?;

    println!("1. peer pauses");
    peer.submit(SessionOperation::PlaybackSetPlaying { is_playing: false })
        .await?;
    step("pause", &mut peer, &provider, &runtime, short).await?;

    println!("2. peer resumes");
    peer.submit(SessionOperation::PlaybackSetPlaying { is_playing: true })
        .await?;
    step("resume", &mut peer, &provider, &runtime, short).await?;

    if ids.len() > 2 {
        println!("3. peer skips two songs ahead");
        peer.submit(SessionOperation::PlaybackAdvance {
            target_entry_id: ids[2].clone(),
            position_ms: 0,
            is_playing: true,
        })
        .await?;
        step("skip", &mut peer, &provider, &runtime, long).await?;
    }

    println!("4. peer queues a real song next (Kill Bill — SZA)");
    let real = QueueEntry {
        entry_id: new_client_id(),
        track: TrackDescriptor {
            title: "Kill Bill".to_owned(),
            artists: vec!["SZA".to_owned()],
            isrc: Some("USRC12204584".to_owned()),
            album: Some("SOS".to_owned()),
            duration_ms: Some(153_000),
            cover_url: None,
        },
        added_by: None,
    };
    peer.submit(SessionOperation::QueueInsert {
        entry: real.clone(),
        index: 1,
    })
    .await?;
    step("insert real", &mut peer, &provider, &runtime, long).await?;

    println!("5. peer queues a song TIDAL can't have");
    let ghost = QueueEntry {
        entry_id: new_client_id(),
        track: TrackDescriptor {
            title: "Zzqxv Nonexistent Song 9f3a".to_owned(),
            artists: vec!["Nobody Atall Qqv".to_owned()],
            isrc: None,
            album: None,
            duration_ms: None,
            cover_url: None,
        },
        added_by: None,
    };
    peer.submit(SessionOperation::QueueInsert {
        entry: ghost.clone(),
        index: 2,
    })
    .await?;
    step("insert ghost", &mut peer, &provider, &runtime, long).await?;

    println!("5b. peer moves the real song two places back");
    peer.submit(SessionOperation::QueueMove {
        entry_id: real.entry_id.clone(),
        to_index: 3,
    })
    .await?;
    step("move", &mut peer, &provider, &runtime, long).await?;

    println!("6. peer removes the real song again");
    peer.submit(SessionOperation::QueueRemove {
        entry_id: real.entry_id.clone(),
    })
    .await?;
    step("remove", &mut peer, &provider, &runtime, long).await?;

    println!("7. watching for 20 s (songs ending, your own changes in TIDAL) ...");
    step(
        "watching",
        &mut peer,
        &provider,
        &runtime,
        Duration::from_secs(20),
    )
    .await?;

    while let Ok(notice) = notices.try_recv() {
        println!("  notice: {notice:?}");
    }
    peer.client.leave().await?;
    runtime.leave().await?;
    println!("left; TIDAL stays connected until this process exits");
    Ok(())
}
