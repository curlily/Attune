use attune_core::{bridge_controller::BridgeController, providers};

#[tokio::main]
async fn main() -> attune_core::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "attune_core=info".into()))
        .init();

    let controller = BridgeController::initialise(providers::tidal()?).await?;
    let mut events = controller.subscribe();
    while let Ok(event) = events.recv().await {
        println!("EVENT: {event:?}");
    }
    Ok(())
}
