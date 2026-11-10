use std::path::PathBuf;
use std::process::ExitStatus;

pub type Result<T> = std::result::Result<T, Error>;

/// Typed failures of the jam-core library.
///
/// Source errors are kept (`#[source]`/`#[from]`) so callers can walk the
/// chain; conversion to plain text happens only at the UI/wire boundary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // --- Lifecycle -------------------------------------------------------
    #[error("not connected to a music provider")]
    NotConnected,

    #[error("already connected to a music provider")]
    AlreadyConnected,

    // --- Configuration ---------------------------------------------------
    #[error("environment variable {var} is not set")]
    MissingEnv { var: &'static str },

    #[error("executable path has no usable file name: {}", path.display())]
    InvalidExecutablePath { path: PathBuf },

    // --- Process management ---------------------------------------------
    #[error("failed to launch {}", path.display())]
    LaunchFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to run the process termination command for {exe}")]
    KillFailed {
        exe: String,
        #[source]
        source: std::io::Error,
    },

    #[error("process termination command for {exe} exited with {status}")]
    KillCommandFailed { exe: String, status: ExitStatus },

    #[error("{exe} is still running after {timeout_secs} seconds")]
    ProcessStillRunning { exe: String, timeout_secs: u64 },

    // --- CDP / browser transport ----------------------------------------
    #[error("CDP request failed while trying to {context}")]
    Cdp {
        context: &'static str,
        #[source]
        source: chromiumoxide::error::CdpError,
    },

    #[error("HTTP request failed while trying to {context}")]
    Http {
        context: &'static str,
        #[source]
        source: reqwest::Error,
    },

    #[error("no renderer page matching {url} was found")]
    TargetNotFound { url: String },

    #[error("timed out while waiting to {context}")]
    Timeout { context: &'static str },

    // --- Provider bridge -------------------------------------------------
    #[error("provider bridge reported an error: {message}")]
    BridgeReported { message: String },

    #[error("provider bridge call {method} failed: {message}")]
    BridgeCallFailed {
        method: &'static str,
        /// JavaScript error text returned by the adapter.
        message: String,
    },

    #[error("provider bridge call {method} returned an invalid response")]
    InvalidBridgeResponse {
        method: &'static str,
        #[source]
        source: Option<serde_json::Error>,
    },

    #[error("provider bridge does not implement MyJam bridge protocol v1 (found {found:?})")]
    UnsupportedBridgeVersion { found: Option<u64> },

    #[error("bridge event channel closed")]
    EventChannelClosed,

    // --- Protocol --------------------------------------------------------
    #[error("unsupported session protocol version {found}")]
    UnsupportedProtocolVersion { found: u16 },

    #[error("JSON serialization failed")]
    Json(#[from] serde_json::Error),
}
