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
        #[arg(long, default_value = "/tmp/jgd.sock")]
        socket: PathBuf,

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
    }
}

async fn run_http(socket: PathBuf, host: &str, port: u16, headless: bool) -> Result<()> {
    let hub = jgd_server::hub::spawn();

    // Start R connection listener.
    let listener = jgd_server::listener::Listener::bind_unix(&socket)?;
    tracing::info!(?socket, "listening for R connections");

    let hub_clone = hub.clone();
    let shutdown_signal = async {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("shutting down");
    };

    // Spawn the R connection serve loop.
    let serve_hub = hub.clone();
    let server_name = "jgdmr".to_owned();
    tokio::spawn(async move {
        jgd_server::serve::serve(
            listener,
            serve_hub,
            server_name,
            jgd_protocol::Transport::Unix,
            std::future::pending(),
        )
        .await;
    });

    // Build HTTP router.
    let app = if headless {
        tracing::info!("headless mode: REST API only");
        jgd_server::api::router(hub_clone)
    } else {
        jgd_server::api::full_router(hub_clone)
    };

    let addr = format!("{host}:{port}");
    let tcp_listener = TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "HTTP server listening");

    axum::serve(tcp_listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    // Clean up socket file.
    let _ = std::fs::remove_file(&socket);

    Ok(())
}
