//! WebSocket relay between browser clients and the Hub.

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;

use jgd_protocol::message::{Message, ResizeMessage};
use jgd_server::hub::HubHandle;

/// Build a router with the WebSocket upgrade endpoint.
pub fn router(hub: HubHandle) -> Router {
    Router::new().route("/ws", get(upgrade)).with_state(hub)
}

async fn upgrade(ws: WebSocketUpgrade, State(hub): State<HubHandle>) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, hub))
}

async fn handle_ws(socket: WebSocket, hub: HubHandle) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut broadcast_rx = hub.subscribe();

    // Forward broadcast messages to the WebSocket client.
    let send_task = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(msg) => {
                    let json = match serde_json::to_string(&msg) {
                        Ok(j) => j,
                        Err(_) => continue,
                    };
                    if ws_tx.send(WsMessage::text(json)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "WebSocket client lagged, skipping messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Receive messages from the WebSocket client.
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            WsMessage::Text(t) => t,
            WsMessage::Close(_) => break,
            _ => continue,
        };

        // Parse resize messages from the browser.
        if let Ok(parsed) = serde_json::from_str::<Message>(&text) {
            if let Message::Resize(resize) = parsed {
                hub.client_resize(resize);
            }
        } else if let Ok(resize) = serde_json::from_str::<ResizeMessage>(&text) {
            // Also accept bare resize objects (without "type" wrapper).
            hub.client_resize(resize);
        }
    }

    send_task.abort();
    tracing::debug!("WebSocket client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as TungMessage;

    use jgd_protocol::message::{DeviceInfo, FrameMessage};
    use jgd_protocol::{DrawingOp, GraphicsContext, Plot};

    use jgd_server::hub;

    fn make_frame(session_id: &str) -> Message {
        Message::Frame(FrameMessage {
            plot: Plot {
                session_id: Some(session_id.into()),
                ops: vec![DrawingOp::Line {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 100.0,
                    y2: 100.0,
                    gc: GraphicsContext {
                        col: Some("rgba(0,0,0,1)".into()),
                        ..Default::default()
                    },
                }],
                device: DeviceInfo {
                    width: 800.0,
                    height: 600.0,
                    dpi: None,
                    bg: None,
                },
            },
            incremental: false,
            new_page: None,
            resize_replay: None,
            plot_index: None,
            plot_number: None,
            ext: None,
        })
    }

    #[tokio::test]
    async fn ws_receives_broadcast_frame() {
        let hub = hub::spawn();
        let app = super::super::api::full_router(hub.clone());

        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });

        let url = format!("ws://{addr}/ws");
        let (mut ws, _) = connect_async(&url).await.unwrap();

        // Inject a frame via the Hub.
        let (conn_id, _rx) = hub.register_session();
        hub.r_message(conn_id, make_frame("ws-test"));

        // The WebSocket client should receive the broadcast.
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .expect("timeout waiting for WS message")
            .expect("stream ended")
            .expect("WS error");

        if let TungMessage::Text(text) = msg {
            let parsed: Message = serde_json::from_str(&text).unwrap();
            assert!(
                matches!(parsed, Message::Frame(_)),
                "expected Frame, got {parsed:?}"
            );
        } else {
            panic!("expected text message, got {msg:?}");
        }
    }
}
