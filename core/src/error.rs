use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("item {id} not found")]
    ItemNotFound { id: u64 },

    #[error("not connected to a service")]
    NotConnected,

    #[error("already connected to a service")]
    AlreadyConnected,

    #[error("browser connection failed")]
    BrowserConnectionFailed,

    #[error("CDP timeout")]
    CDPTimeout,

    #[error("CDP timeout")]
    CDPError(#[from] chromiumoxide::error::CdpError),

    #[error("target URL not found: {url}")]
    TargetUrlNotFound { url: String },

    #[error("path not found: {}", path.display())]
    PathNotFound { path: PathBuf },

    #[error("invalid JSON")]
    Json(#[from] serde_json::Error),

    #[error("failed to launch {}", path.display())]
    LaunchFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to kill {}", path.display())]
    KillFailed {
        path: PathBuf,
        source: std::io::Error,
    },
}
