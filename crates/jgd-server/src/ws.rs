//! WebSocket relay between browser clients and the Hub.

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};

use jgd_protocol::message::{Message, ResizeMessage};

use crate::hub::HubHandle;

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
        while let Ok(msg) = broadcast_rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if ws_tx.send(WsMessage::text(json)).await.is_err() {
                break;
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
