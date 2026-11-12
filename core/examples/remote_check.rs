//! Smoke test against a deployed relay with two real clients.
//!
//!     cargo run -p attune-core --example remote_check -- ws://host:8787/ws
//!
//! Checks create/join, an operation round trip (with latency), the
//! skip-what-nobody-can-play rule, and leaving. Prints a line per step and
//! exits non-zero on the first failure.

use std::time::{Duration, Instant};

use attune_core::{
    network::{SessionClient, SessionClientConfig, SessionEvent, SubmitOutcome, new_client_id},
    protocol::{QueueEntry, SessionMusicState, SessionOperation, SessionPlayback, TrackDescriptor},
};
use tokio::{sync::broadcast, time::timeout};

type Check = Result<(), Box<dyn std::error::Error>>;

fn entry(id: &str) -> QueueEntry {
    QueueEntry {
        entry_id: id.to_owned(),
        track: TrackDescriptor {
            title: format!("Song {id}"),
            artists: vec!["Artist".to_owned()],
            isrc: None,
            album: None,
            duration_ms: Some(200_000),
        },
    }
}

async fn next_remote(
    events: &mut broadcast::Receiver<SessionEvent>,
) -> Result<SessionEvent, String> {
    timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(event @ SessionEvent::RemoteOperation { .. }) => return Ok(event),
                Ok(_) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for a remote operation".to_owned())?
}

#[tokio::main]
async fn main() -> Check {
    let url = std::env::args()
        .nth(1)
        .ok_or("usage: remote_check <ws-url>")?;
    let config = |name: &str| SessionClientConfig::new(url.clone(), new_client_id(), name);
    let initial = SessionMusicState {
        queue: ["s1", "s2", "s3"].map(entry).to_vec(),
        current_entry_id: Some("s1".to_owned()),
        playback: SessionPlayback {
            is_playing: true,
            position_ms: 0,
            position_captured_at_ms: 0,
        },
    };

    let started = Instant::now();
    let (a, a_start) = SessionClient::create(config("Remote A"), initial).await?;
    println!(
        "ok  create ({:?}), code {}",
        started.elapsed(),
        a_start.credentials.code
    );

    let started = Instant::now();
    let (b, mut b_start) =
        SessionClient::join(config("Remote B"), a_start.credentials.clone()).await?;
    println!(
        "ok  join ({:?}), revision {}",
        started.elapsed(),
        b_start.snapshot.revision
    );

    // The client probes the clock right after connecting (one per second).
    tokio::time::sleep(std::time::Duration::from_millis(4_500)).await;
    match b.clock() {
        Some(clock) => println!(
            "ok  clock: server is {} ms ahead of this computer (round trip {} ms)",
            clock.offset_ms, clock.round_trip_ms
        ),
        None => println!("FAIL no clock estimate"),
    }

    let mut round_trips = Vec::new();
    let mut sequence = 0;
    for playing in [false, true, false, true, false] {
        let started = Instant::now();
        let outcome = a
            .submit_operation(
                SessionOperation::PlaybackSetPlaying {
                    is_playing: playing,
                },
                sequence,
            )
            .await?;
        let acked = started.elapsed();
        let SubmitOutcome::Applied {
            sequence: accepted, ..
        } = outcome
        else {
            return Err(format!("unexpected outcome {outcome:?}").into());
        };
        sequence = accepted;
        next_remote(&mut b_start.events).await?;
        round_trips.push((acked, started.elapsed()));
    }
    for (index, (acked, delivered)) in round_trips.iter().enumerate() {
        println!(
            "ok  op {}: ack after {acked:?}, delivered to B after {delivered:?}",
            index + 1
        );
    }

    a.set_unavailable(vec!["s2".to_owned()]).await?;
    b.set_unavailable(vec!["s2".to_owned()]).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let advance = SessionOperation::PlaybackAdvance {
        target_entry_id: "s2".to_owned(),
        position_ms: 0,
        is_playing: false,
    };
    a.submit_operation(advance, sequence).await?;
    let original = next_remote(&mut b_start.events).await?;
    let skip = next_remote(&mut b_start.events).await?;
    match skip {
        SessionEvent::RemoteOperation {
            origin_client_id: None,
            operation:
                SessionOperation::PlaybackAdvance {
                    ref target_entry_id,
                    ..
                },
            ..
        } if target_entry_id == "s3" => {
            println!("ok  server skipped the unplayable song (after {original:?})")
        }
        other => return Err(format!("expected a server skip to s3, got {other:?}").into()),
    }

    b.leave().await?;
    a.leave().await?;
    println!("ok  both left");
    Ok(())
}
