//! `jam-server` executable.
//!
//! Configuration (environment variable, or the flag where noted):
//! - `JAM_BIND` / `--bind <addr>`: listen address, default `127.0.0.1:8787`.
//!   Behind Cloudflare Tunnel or another reverse proxy, keep it on loopback.
//! - `JAM_MAX_SESSIONS` (default 100), `JAM_MAX_PARTICIPANTS` (default 16)
//! - `JAM_RECONNECT_GRACE_SECS` (default 60)
//! - `JAM_JOIN_ATTEMPTS_PER_MINUTE` (default 10): failed joins allowed per
//!   IP address. Behind a reverse proxy every client shares the proxy's
//!   address, so raise it there.
//! - `RUST_LOG` for log filtering, default `info`.

use std::{env, error::Error, process::ExitCode, time::Duration};

use jam_server::{ServerConfig, serve};
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:8787";

fn env_number<T: std::str::FromStr>(name: &str) -> Result<Option<T>, String> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map(Some)
            .map_err(|_| format!("{name} must be a number, got {value:?}")),
        Err(_) => Ok(None),
    }
}

fn bind_address() -> Result<String, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [] => Ok(env::var("JAM_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned())),
        [flag, address] if flag == "--bind" => Ok(address.clone()),
        [flag] if flag == "-h" || flag == "--help" => {
            println!(
                "Usage: jam-server [--bind <addr>]\nSee the README for environment variables."
            );
            std::process::exit(0);
        }
        _ => Err("usage: jam-server [--bind <addr>]".to_owned()),
    }
}

fn config() -> Result<ServerConfig, String> {
    let mut config = ServerConfig::default();
    if let Some(value) = env_number("JAM_MAX_SESSIONS")? {
        config.max_sessions = value;
    }
    if let Some(value) = env_number("JAM_MAX_PARTICIPANTS")? {
        config.max_participants = value;
    }
    if let Some(seconds) = env_number("JAM_RECONNECT_GRACE_SECS")? {
        config.reconnect_grace = Duration::from_secs(seconds);
    }
    if let Some(value) = env_number("JAM_JOIN_ATTEMPTS_PER_MINUTE")? {
        config.join_attempts_per_minute = value;
    }
    Ok(config)
}

async fn run() -> Result<(), Box<dyn Error>> {
    let bind = bind_address()?;
    let config = config()?;
    let listener = TcpListener::bind(&bind).await?;
    info!(
        address = %listener.local_addr()?,
        max_sessions = config.max_sessions,
        max_participants = config.max_participants,
        "MyJam relay listening (WebSocket endpoint: /ws)"
    );
    serve(listener, config, async {
        let _ = tokio::signal::ctrl_c().await;
        info!("Shutting down");
    })
    .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "jam-server failed");
            ExitCode::FAILURE
        }
    }
}
