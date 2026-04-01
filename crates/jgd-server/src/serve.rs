//! Server accept loop: ties together [`Listener`], [`Hub`](crate::hub),
//! and per-connection session handling.

use futures_util::{SinkExt, StreamExt};

use jgd_protocol::message::Transport;

use crate::hub::HubHandle;
use crate::listener::{Connection, Listener};
use crate::session;

/// Run the server accept loop until `shutdown` completes.
///
/// Accepts connections on `listener`, registers each with the Hub, and
/// spawns a per-connection task that handles the deferred welcome and
/// bidirectional message routing.
pub async fn serve(
    mut listener: Listener,
    hub: HubHandle,
    server_name: String,
    transport: Transport,
    shutdown: impl std::future::Future<Output = ()>,
) {
    tokio::select! {
        () = accept_loop(&mut listener, &hub, &server_name, transport) => {}
        () = shutdown => {
            tracing::info!("server shutting down");
        }
    }
}

async fn accept_loop(
    listener: &mut Listener,
    hub: &HubHandle,
    server_name: &str,
    transport: Transport,
) {
    loop {
        match listener.accept().await {
            Ok(conn) => {
                let hub = hub.clone();
                let name = server_name.to_owned();
                tokio::spawn(async move {
                    handle_connection(conn, hub, &name, transport).await;
                });
            }
            Err(e) => {
                tracing::warn!("accept error: {e}");
            }
        }
    }
}

/// Handle one R client connection: deferred welcome + bidirectional Hub routing.
async fn handle_connection(
    conn: Connection,
    hub: HubHandle,
    server_name: &str,
    transport: Transport,
) {
    let mut framed = session::framed(conn);
    let (conn_id, mut hub_rx) = hub.register_session();
    let mut welcome_sent = false;

    loop {
        tokio::select! {
            result = framed.next() => {
                match result {
                    Some(Ok(msg)) => {
                        if !welcome_sent {
                            welcome_sent = true;
                            let welcome = session::server_info_message(server_name, transport);
                            if framed.send(welcome).await.is_err() {
                                break;
                            }
                        }
                        hub.r_message(conn_id, msg);
                    }
                    Some(Err(e)) => {
                        tracing::warn!(conn_id, "session read error: {e}");
                        break;
                    }
                    None => break,
                }
            }
            Some(msg) = hub_rx.recv() => {
                if framed.send(msg).await.is_err() {
                    break;
                }
            }
        }
    }

    hub.unregister_session(conn_id);
    tracing::debug!(conn_id, "connection closed");
}
