//! R client session handling with deferred welcome and JSONL framing.

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

use jgd_protocol::codec::JsonLinesCodec;
use jgd_protocol::message::{Message, ServerInfoMessage, Transport};

/// Create a JSONL-framed stream/sink pair from an async I/O handle.
pub fn framed<T>(io: T) -> Framed<T, JsonLinesCodec>
where
    T: AsyncRead + AsyncWrite,
{
    Framed::new(io, JsonLinesCodec::new())
}

/// Build the deferred welcome message.
///
/// Per the jgd protocol, the server must NOT write to the connection
/// before receiving the first message from R (deferred welcome).
/// On Windows Named Pipes, writing before the first read causes data loss.
pub fn server_info_message(server_name: &str, transport: Transport) -> Message {
    Message::ServerInfo(ServerInfoMessage {
        server_name: server_name.to_owned(),
        protocol_version: 1,
        transport,
        server_info: None,
    })
}

/// Handle one R client session on the given framed connection.
///
/// Reads messages from R, sends the deferred welcome after the first
/// message, and routes subsequent messages through the provided callback.
///
/// Returns `Ok(())` on clean shutdown (client closed connection) or
/// mid-session read/write errors (logged as warnings). Returns `Err`
/// only if the welcome message cannot be sent (broken connection before
/// any useful work).
pub async fn run_session<T, F, Fut>(
    framed: &mut Framed<T, JsonLinesCodec>,
    server_name: &str,
    transport: Transport,
    mut on_message: F,
) -> anyhow::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(Message) -> Fut,
    Fut: std::future::Future<Output = Option<Message>>,
{
    let mut welcome_sent = false;

    while let Some(result) = framed.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!("session read error: {e}");
                break;
            }
        };

        // Deferred welcome: send server_info after first message from R.
        if !welcome_sent {
            welcome_sent = true;
            let welcome = server_info_message(server_name, transport);
            framed
                .send(welcome)
                .await
                .map_err(|e| anyhow::anyhow!("failed to send welcome: {e}"))?;
        }

        // Route the message and optionally send a response.
        if let Some(response) = on_message(msg).await
            && let Err(e) = framed.send(response).await
        {
            tracing::warn!("failed to send response: {e}");
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::message::{MetricsKind, MetricsRequest, MetricsResponse};
    use jgd_protocol::GraphicsContext;
    use tokio::io::duplex;

    #[tokio::test]
    async fn deferred_welcome_sent_after_first_message() {
        let (client_io, server_io) = duplex(8192);
        let mut client = framed(client_io);
        let mut server = framed(server_io);

        // Client sends ping.
        client.send(Message::Ping).await.unwrap();

        // Server runs session in a task.
        let handle = tokio::spawn(async move {
            run_session(&mut server, "test-server", Transport::Unix, |msg| async move {
                match msg {
                    Message::Ping => Some(Message::Pong),
                    _ => None,
                }
            })
            .await
            .unwrap();
        });

        // Client should receive: 1) server_info (welcome), 2) pong.
        let msg1 = client.next().await.unwrap().unwrap();
        assert!(
            matches!(msg1, Message::ServerInfo(_)),
            "expected ServerInfo, got {msg1:?}"
        );

        let msg2 = client.next().await.unwrap().unwrap();
        assert_eq!(msg2, Message::Pong);

        // Close client to terminate session.
        drop(client);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn metrics_request_response_roundtrip() {
        let (client_io, server_io) = duplex(8192);
        let mut client = framed(client_io);
        let mut server = framed(server_io);

        let req = Message::MetricsRequest(MetricsRequest {
            id: 1,
            kind: MetricsKind::StrWidth,
            str: Some("Hello".into()),
            c: None,
            gc: GraphicsContext::default(),
        });
        client.send(req).await.unwrap();

        let handle = tokio::spawn(async move {
            run_session(
                &mut server,
                "test-server",
                Transport::Unix,
                |msg| async move {
                    match msg {
                        Message::MetricsRequest(req) => {
                            let resp = jgd_font_metrics::compute_metrics(&req);
                            Some(Message::MetricsResponse(resp))
                        }
                        _ => None,
                    }
                },
            )
            .await
            .unwrap();
        });

        // Skip welcome.
        let _welcome = client.next().await.unwrap().unwrap();

        // Receive metrics response.
        let resp = client.next().await.unwrap().unwrap();
        match resp {
            Message::MetricsResponse(MetricsResponse { id: 1, width, .. }) => {
                assert!(width > 0.0, "expected positive width, got {width}");
            }
            other => panic!("expected MetricsResponse, got {other:?}"),
        }

        drop(client);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_json_terminates_gracefully() {
        let (client_io, server_io) = duplex(8192);
        let mut server = framed(server_io);

        // Write raw invalid JSON directly to the client side.
        let mut raw_client = client_io;
        tokio::io::AsyncWriteExt::write_all(&mut raw_client, b"not valid json\n")
            .await
            .unwrap();
        drop(raw_client);

        // Session should handle the error and return Ok.
        let result = run_session(
            &mut server,
            "test-server",
            Transport::Unix,
            |_msg| async { None },
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn client_disconnect_before_response() {
        let (client_io, server_io) = duplex(8192);
        let mut client = framed(client_io);
        let mut server = framed(server_io);

        // Client sends a message then immediately disconnects.
        client.send(Message::Ping).await.unwrap();
        drop(client);

        // Session should complete without error.
        let result = run_session(
            &mut server,
            "test-server",
            Transport::Unix,
            |msg| async move {
                match msg {
                    Message::Ping => Some(Message::Pong),
                    _ => None,
                }
            },
        )
        .await;
        // Welcome send may succeed or fail depending on timing; either is OK.
        // The important thing is no panic.
        let _ = result;
    }
}
