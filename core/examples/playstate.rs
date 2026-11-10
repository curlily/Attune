use jam_core::{bridge_controller::BridgeController, providers};

#[tokio::main]
async fn main() -> jam_core::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "jam_core=info".into()))
        .init();

    let controller = BridgeController::initialise(providers::tidal()?).await?;

    let play_state = controller.get_playstate().await?;
    match play_state.track {
        Some(track) => {
            println!(
                "Current Playstate\n| Title: {} \n| Artists: {}\n| Position: {}\n| Timestamp: {}\n| Playing: {}",
                track.title,
                track.artists.join(", "),
                play_state.position_ms.unwrap_or_default(),
                play_state.timestamp_ms.unwrap_or_default(),
                play_state.playing
            )
        }
        None => {
            println!("Currently not playing");
        }
    }
    Ok(())
}
