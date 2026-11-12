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

    /// The provider's browser window didn't come up with the debug port.
    /// Usually a window of the same profile is already open without it
    /// (e.g. the one used for signing in).
    #[error(
        "{name} didn't open in a window MyJam can connect to. Close every {name} window MyJam opened and try again."
    )]
    BrowserWindowUnavailable { name: String },

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

    /// The provider could not find the current track. Nothing was changed.
    /// `entry_ids` lists every session entry it could not find, current first.
    #[error("{message}")]
    TrackUnavailable {
        entry_ids: Vec<String>,
        message: String,
    },

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

    // --- Session networking ---------------------------------------------
    #[error("invalid session server URL: {reason}")]
    InvalidServerUrl { reason: &'static str },

    #[error("a join code is four letters, like KWPZ")]
    InvalidJoinCode,

    #[error("WebSocket connection failed")]
    WebSocket(#[source] Box<tokio_tungstenite::tungstenite::Error>),

    #[error("TLS setup failed")]
    Tls(#[source] rustls::Error),

    #[error("no trusted root certificates are available for wss:// connections")]
    NoRootCertificates,

    #[error("the connection to the session server closed")]
    TransportClosed,

    #[error("the session client has stopped")]
    SessionClosed,

    #[error("the session server sent a message that could not be parsed")]
    InvalidServerMessage(#[source] serde_json::Error),

    #[error("the session server violated the protocol: {reason}")]
    ProtocolViolation { reason: &'static str },

    #[error("the session server sent an invalid session state")]
    InvalidRemoteState(#[source] jam_protocol::state::StateError),

    /// A structured error from the server. `message` is display text only.
    #[error("the session server refused the request ({code:?}): {message}")]
    Remote {
        code: jam_protocol::ErrorCode,
        message: String,
    },

    #[error("a {len}-byte message exceeds the {max}-byte limit")]
    MessageTooLarge { len: usize, max: usize },

    #[error("too many requests are waiting for the session server")]
    TooManyPendingRequests,

    /// A queue edit made in MyJam that the session didn't take, e.g. because
    /// the song was removed meanwhile. Nothing changed.
    #[error("the session didn't take this queue edit: {message}")]
    EditRejected {
        code: jam_protocol::RejectCode,
        message: String,
    },
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(error))
    }
}
