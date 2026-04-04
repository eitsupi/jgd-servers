//! Headless server: REST API only, no browser frontend or TUI.

use anyhow::Result;
use jgd_protocol::SocketAddr;

use crate::listen;

pub async fn run(
    socket_override: Option<&str>,
    listen_uri: Option<&str>,
    json: bool,
) -> Result<()> {
    let socket_addr = match socket_override {
        Some(s) => SocketAddr::parse(s).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => jgd_server::discovery::default_socket_addr("jgdmr", "")?,
    };
    let listen_addr = listen::parse_listen_uri(listen_uri)?;

    let hub = jgd_server::hub::spawn();

    // Start R connection listener.
    let listener = jgd_server::listener::Listener::bind(&socket_addr).await?;
    tracing::info!(%socket_addr, "listening for R connections");

    // Spawn the R connection serve loop with a shutdown channel.
    let (serve_shutdown_tx, serve_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_hub = hub.clone();
    let server_name = "jgdmr".to_owned();
    let transport = socket_addr.transport();
    let serve_handle = tokio::spawn(async move {
        jgd_server::serve::serve(listener, serve_hub, server_name, transport, async {
            let _ = serve_shutdown_rx.await;
        })
        .await;
    });

    // Build REST API router (headless: no WebSocket or static files).
    let app = crate::http::api::router(hub.clone());

    // Prepare graceful shutdown on SIGINT or SIGTERM.
    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutting down");
        let _ = api_shutdown_tx.send(());
    });

    // Write discovery file so R clients and API clients can find us.
    let discovery_path = jgd_server::discovery::default_path();
    if let Some(path) = &discovery_path {
        let mut server_info = serde_json::Map::new();
        server_info.insert(
            "listenUrl".into(),
            serde_json::Value::String(listen_addr.to_string()),
        );

        let info = jgd_server::discovery::DiscoveryInfo {
            server_name: "jgdmr".into(),
            socket_path: socket_addr.to_string(),
            pid: std::process::id(),
            server_info: Some(server_info),
            extra: Default::default(),
        };
        if let Err(e) = jgd_server::discovery::write(path, &info) {
            tracing::warn!(%e, "failed to write discovery file");
        }
    }

    // Output startup info as JSON for programmatic consumers.
    if json {
        let startup = serde_json::json!({
            "pid": std::process::id(),
            "listen": listen_addr.to_string(),
            "socket_path": socket_addr.to_string(),
        });
        println!("{}", serde_json::to_string(&startup)?);
    }

    // Serve REST API (blocks until shutdown).
    listen::serve(&listen_addr, app, async {
        let _ = api_shutdown_rx.await;
    })
    .await?;

    // Signal the R serve loop to stop and wait for it to finish.
    let _ = serve_shutdown_tx.send(());
    let _ = serve_handle.await;

    // Clean up.
    if let Some(path) = &discovery_path {
        let _ = jgd_server::discovery::remove(path);
    }

    Ok(())
}

/// Wait for SIGINT (ctrl-c) or SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}
