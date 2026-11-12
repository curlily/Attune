//! One WebSocket connection: handshake, then forwarding to its session.

use std::{net::IpAddr, sync::Arc};

use attune_protocol::{ClientMessage, ErrorCode, ServerMessage};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt, stream::SplitStream};
use tokio::{sync::mpsc, time::timeout};
use tracing::debug;

use crate::hub::{Binding, ConnectionHandle, Hub, Rejection, SessionCommand};

/// Messages waiting to be written to one client. Far more than a healthy
/// client ever has in flight; a full queue means it can't keep up.
const OUTGOING_CAPACITY: usize = 256;

fn error_message(code: ErrorCode, message: &str) -> ServerMessage {
    ServerMessage::Error {
        code,
        message: message.to_owned(),
    }
}

pub(crate) async fn serve(socket: WebSocket, hub: Arc<Hub>, ip: IpAddr) {
    let connection_id = hub.next_connection_id();
    let (mut sink, mut stream) = socket.split();
    let (outgoing, mut queue) = mpsc::channel::<ServerMessage>(OUTGOING_CAPACITY);

    // The writer ends when every sender is gone: the session dropped this
    // connection (left, replaced by a rejoin, too slow) or the reader ended.
    let mut writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            let Ok(text) = serde_json::to_string(&message) else {
                continue;
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                return;
            }
        }
        let _ = sink.close().await;
    });

    let handle = ConnectionHandle {
        id: connection_id,
        outgoing: outgoing.clone(),
    };
    let binding = match timeout(
        hub.config().handshake_timeout,
        handshake(&mut stream, &hub, handle, ip),
    )
    .await
    {
        Ok(Ok(binding)) => binding,
        Ok(Err(Some(reply))) => {
            let _ = outgoing.send(*reply).await;
            drop(outgoing);
            let _ = writer.await;
            return;
        }
        Ok(Err(None)) | Err(_) => {
            drop(outgoing);
            let _ = writer.await;
            return;
        }
    };

    // From here only the session holds a strong sender. Keep a weak one for
    // replying to unparsable frames without keeping the writer alive.
    let replies = outgoing.downgrade();
    drop(outgoing);
    let Binding {
        log_id,
        client_id,
        commands,
    } = binding;

    loop {
        tokio::select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMessage>(text.as_str()) {
                    Ok(message) => {
                        let command = SessionCommand::Message {
                            client_id: client_id.clone(),
                            connection_id,
                            message,
                        };
                        if commands.send(command).await.is_err() {
                            break; // session ended
                        }
                    }
                    Err(_) => {
                        // Never log the payload: it can contain listening data.
                        debug!(session = %log_id, %client_id, "Unparsable message from client");
                        if let Some(replies) = replies.upgrade() {
                            let _ = replies.try_send(error_message(ErrorCode::InvalidMessage, "unparsable message"));
                        }
                    }
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {} // ping (answered automatically), pong, binary
            },
            // The session dropped this connection.
            _ = &mut writer => break,
        }
    }

    let _ = commands
        .send(SessionCommand::Disconnected {
            client_id,
            connection_id,
        })
        .await;
    writer.abort();
}

/// Read the first message and create or join a session. `Err(Some(reply))`
/// carries an error to send before closing; `Err(None)` means the socket
/// closed first.
async fn handshake(
    stream: &mut SplitStream<WebSocket>,
    hub: &Arc<Hub>,
    connection: ConnectionHandle,
    ip: IpAddr,
) -> Result<Binding, Option<Rejection>> {
    let message = loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                break serde_json::from_str::<ClientMessage>(text.as_str()).map_err(|_| {
                    Some(Box::new(error_message(
                        ErrorCode::InvalidMessage,
                        "unparsable message",
                    )))
                })?;
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Err(None),
            Some(Ok(_)) => {}
        }
    };

    match message {
        ClientMessage::Create(request) => hub.create(request, connection).map_err(Some),
        ClientMessage::Join(request) => hub.join(request, connection, ip).await.map_err(Some),
        _ => Err(Some(Box::new(error_message(
            ErrorCode::NotJoined,
            "create or join a session first",
        )))),
    }
}
