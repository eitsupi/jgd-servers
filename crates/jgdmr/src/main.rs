mod http;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(name = "jgdmr", about = "jgd graphics device server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP server for R graphics.
    Http {
        /// Unix socket path for R connections.
        /// If omitted, a temporary PID-based path is generated automatically.
        #[arg(long)]
        socket: Option<PathBuf>,

        /// HTTP host address to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// HTTP port for browser/API access.
        #[arg(long, default_value_t = 3000)]
        port: u16,

        /// Headless mode: REST API only, no browser frontend.
        #[arg(long)]
        headless: bool,
    },

    /// Start the TUI plot viewer.
    Tui {
        /// Unix socket path for R connections.
        /// If omitted, a temporary PID-based path is generated automatically.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,jgd_server=debug".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Http {
            socket,
            host,
            port,
            headless,
        } => run_http(socket, &host, port, headless).await,
        Command::Tui { socket } => run_tui(socket).await,
    }
}

async fn run_http(
    socket_override: Option<PathBuf>,
    host: &str,
    port: u16,
    headless: bool,
) -> Result<()> {
    let socket = socket_override.unwrap_or_else(jgd_server::discovery::default_socket_path);

    let hub = jgd_server::hub::spawn();

    // Start R connection listener.
    let listener = jgd_server::listener::Listener::bind_unix(&socket)?;
    tracing::info!(?socket, "listening for R connections");

    // Start HTTP listener before writing the discovery file so that
    // the advertised URL is actually reachable.
    let addr = format!("{host}:{port}");
    let tcp_listener = TcpListener::bind(&addr).await?;
    let local_addr = tcp_listener.local_addr()?;
    tracing::info!(%local_addr, "HTTP server listening");

    // Write discovery file so R clients can auto-connect.
    let discovery_path = jgd_server::discovery::default_path();
    if let Some(path) = &discovery_path {
        let http_url = format!("http://{local_addr}/");
        let mut server_info = serde_json::Map::new();
        server_info.insert("httpUrl".into(), serde_json::Value::String(http_url));

        let info = jgd_server::discovery::DiscoveryInfo {
            server_name: "jgdmr".into(),
            socket_path: format!("unix://{}", socket.display()),
            pid: std::process::id(),
            server_info: Some(server_info),
            extra: Default::default(),
        };
        if let Err(e) = jgd_server::discovery::write(path, &info) {
            tracing::warn!(%e, "failed to write discovery file");
        }
    }

    let hub_clone = hub.clone();
    let shutdown_signal = async {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("shutting down");
    };

    // Spawn the R connection serve loop with a shutdown channel so it
    // stops cooperatively when the HTTP server exits.
    let (serve_shutdown_tx, serve_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_hub = hub.clone();
    let server_name = "jgdmr".to_owned();
    let serve_handle = tokio::spawn(async move {
        jgd_server::serve::serve(
            listener,
            serve_hub,
            server_name,
            jgd_protocol::Transport::Unix,
            async {
                let _ = serve_shutdown_rx.await;
            },
        )
        .await;
    });

    // Build HTTP router.
    let app = if headless {
        tracing::info!("headless mode: REST API only");
        http::api::router(hub_clone)
    } else {
        http::api::full_router(hub_clone)
    };

    axum::serve(tcp_listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    // Signal the serve loop to stop and wait for it to finish.
    let _ = serve_shutdown_tx.send(());
    let _ = serve_handle.await;

    // Clean up discovery file and socket.
    if let Some(path) = &discovery_path {
        let _ = jgd_server::discovery::remove(path);
    }
    let _ = std::fs::remove_file(&socket);

    Ok(())
}

async fn run_tui(socket_override: Option<PathBuf>) -> Result<()> {
    let socket = socket_override.unwrap_or_else(jgd_server::discovery::default_socket_path);

    let hub = jgd_server::hub::spawn();

    // Start R connection listener.
    let listener = jgd_server::listener::Listener::bind_unix(&socket)?;
    tracing::info!(?socket, "listening for R connections");

    // Write discovery file so R clients can auto-connect.
    let discovery_path = jgd_server::discovery::default_path();
    if let Some(path) = &discovery_path {
        let info = jgd_server::discovery::DiscoveryInfo {
            server_name: "jgdmr".into(),
            socket_path: format!("unix://{}", socket.display()),
            pid: std::process::id(),
            server_info: None,
            extra: Default::default(),
        };
        if let Err(e) = jgd_server::discovery::write(path, &info) {
            tracing::warn!(%e, "failed to write discovery file");
        }
    }

    // Spawn the R connection serve loop with a shutdown channel so it
    // stops cooperatively when the TUI exits.
    let (serve_shutdown_tx, serve_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_hub = hub.clone();
    let server_name = "jgdmr".to_owned();
    let serve_handle = tokio::spawn(async move {
        jgd_server::serve::serve(
            listener,
            serve_hub,
            server_name,
            jgd_protocol::Transport::Unix,
            async {
                let _ = serve_shutdown_rx.await;
            },
        )
        .await;
    });

    // Run the TUI event loop (blocks until the user quits).
    let tui_result = tui::run(hub).await;

    // Signal the serve loop to stop and wait for it to finish.
    let _ = serve_shutdown_tx.send(());
    let _ = serve_handle.await;

    // Clean up discovery file and socket regardless of TUI exit status.
    // Without this, an error exit would leave a stale discovery file that
    // misleads R clients into connecting to a dead server.
    if let Some(path) = &discovery_path {
        let _ = jgd_server::discovery::remove(path);
    }
    let _ = std::fs::remove_file(&socket);

    tui_result
}
