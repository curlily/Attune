//! Discord's local RPC connection: the named pipe (Windows) or Unix socket
//! the Discord desktop client listens on, as `discord-ipc-0` to `-9`.
//!
//! Every message is a frame: a little-endian `u32` opcode, a little-endian
//! `u32` length, then that many bytes of JSON. The client first sends a
//! handshake with the application's ID; Discord answers with a `READY`
//! event. Commands such as `SET_ACTIVITY` go in `FRAME`s, and Discord may
//! `PING`, which is answered with a `PONG` carrying the same payload.

use std::io;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const HANDSHAKE: u32 = 0;
const FRAME: u32 = 1;
const CLOSE: u32 = 2;
const PING: u32 = 3;
const PONG: u32 = 4;

/// Far larger than anything Discord sends; guards against a corrupt length.
const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug)]
pub enum Incoming {
    /// A response or event.
    Frame(Value),
    /// Must be answered with a `PONG` carrying this payload.
    Ping(Value),
    /// Discord is closing the connection (e.g. a bad client ID).
    Close(Value),
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    opcode: u32,
    payload: &Value,
) -> io::Result<()> {
    let body = serde_json::to_vec(payload)?;
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&opcode.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame).await?;
    writer.flush().await
}

/// Read one frame. Not cancel-safe: read from a task of its own.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Incoming> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).await?;
    let opcode = u32::from_le_bytes(header[..4].try_into().expect("4 bytes"));
    let length = u32::from_le_bytes(header[4..].try_into().expect("4 bytes")) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    let payload = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)?
    };
    match opcode {
        FRAME => Ok(Incoming::Frame(payload)),
        PING => Ok(Incoming::Ping(payload)),
        CLOSE => Ok(Incoming::Close(payload)),
        // A handshake or pong from Discord's side isn't expected; skip it.
        _ => Ok(Incoming::Frame(Value::Null)),
    }
}

pub async fn send_handshake<W: AsyncWrite + Unpin>(
    writer: &mut W,
    client_id: &str,
) -> io::Result<()> {
    write_frame(
        writer,
        HANDSHAKE,
        &json!({ "v": 1, "client_id": client_id }),
    )
    .await
}

pub async fn send_pong<W: AsyncWrite + Unpin>(writer: &mut W, payload: &Value) -> io::Result<()> {
    write_frame(writer, PONG, payload).await
}

/// Set (or with `None`, clear) this process's activity.
pub async fn send_activity<W: AsyncWrite + Unpin>(
    writer: &mut W,
    activity: Option<&Value>,
    nonce: u64,
) -> io::Result<()> {
    let command = json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": nonce.to_string(),
    });
    write_frame(writer, FRAME, &command).await
}

/// Open Discord's IPC endpoint: the first of `discord-ipc-0..9` that answers.
#[cfg(windows)]
pub async fn open() -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;
    let mut last = io::Error::new(io::ErrorKind::NotFound, "Discord isn't running");
    for index in 0..10 {
        match ClientOptions::new().open(format!(r"\\.\pipe\discord-ipc-{index}")) {
            Ok(pipe) => return Ok(pipe),
            Err(error) => last = error,
        }
    }
    Err(last)
}

#[cfg(unix)]
pub async fn open() -> io::Result<tokio::net::UnixStream> {
    let base = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .unwrap_or_else(|| "/tmp".to_owned());
    let mut last = io::Error::new(io::ErrorKind::NotFound, "Discord isn't running");
    for index in 0..10 {
        match tokio::net::UnixStream::connect(format!("{base}/discord-ipc-{index}")).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        send_handshake(&mut client, "123").await.unwrap();

        // Read the raw bytes on the other end: opcode 0, length, JSON.
        let mut header = [0u8; 8];
        server.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..4], &0u32.to_le_bytes());
        let length = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
        let mut body = vec![0u8; length];
        server.read_exact(&mut body).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, json!({ "v": 1, "client_id": "123" }));

        write_frame(&mut server, PING, &json!({ "n": 1 }))
            .await
            .unwrap();
        match read_frame(&mut client).await.unwrap() {
            Incoming::Ping(payload) => assert_eq!(payload, json!({ "n": 1 })),
            other => panic!("expected a ping, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refuses_oversized_frames() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let mut header = Vec::new();
        header.extend_from_slice(&FRAME.to_le_bytes());
        header.extend_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        server.write_all(&header).await.unwrap();
        assert!(read_frame(&mut client).await.is_err());
    }
}
