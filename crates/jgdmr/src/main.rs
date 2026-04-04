mod client;
mod headless;
mod http;
mod listen;
mod tui;

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
        /// R connection socket address (URI).
        ///
        /// Supported: `unix:///path`, `npipe:////./pipe/name`, `tcp://host:port`.
        /// If omitted, a platform-appropriate default is generated.
        #[arg(long)]
        socket: Option<String>,

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

    /// Start the headless server (REST API over Unix socket or TCP).
    Headless {
        /// R connection socket address (URI).
        ///
        /// Supported: `unix:///path`, `npipe:////./pipe/name`, `tcp://host:port`.
        /// If omitted, a platform-appropriate default is generated.
        #[arg(long)]
        socket: Option<String>,

        /// REST API listen address as a URI.
        ///
        /// Supported schemes:
        /// - `unix:///path/to/socket` (Unix domain socket, default on Unix)
        /// - `npipe:////./pipe/name` (Windows Named Pipe, default on Windows)
        /// - `tcp://host:port`
        ///
        /// If omitted, a platform-appropriate default is generated automatically.
        #[arg(long)]
        listen: Option<String>,

        /// Print startup info as JSON to stdout.
        #[arg(long)]
        json: bool,
    },

    /// Start the TUI plot viewer.
    Tui {
        /// R connection socket address (URI).
        ///
        /// Supported: `unix:///path`, `npipe:////./pipe/name`, `tcp://host:port`.
        /// If omitted, a platform-appropriate default is generated.
        #[arg(long)]
        socket: Option<String>,
    },

    /// List active jgdmr sessions.
    #[command(visible_alias = "ls")]
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Interact with a running jgdmr server via its REST API.
    Api {
        #[command(subcommand)]
        action: ApiCommand,

        /// Target server: a URI (unix://..., tcp://...) or PID.
        /// If omitted, auto-detected from the discovery file.
        #[arg(long, global = true)]
        target: Option<String>,
    },
}

#[derive(Subcommand)]
enum ApiCommand {
    /// List plots on the server.
    Plots {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Render a plot to a file or stdout.
    Render {
        /// Plot ID (session_id). If omitted, uses the most recent plot.
        plot_id: Option<String>,

        /// Output format.
        #[arg(long, default_value = "svg")]
        format: String,

        /// Output width in pixels.
        #[arg(long)]
        width: Option<f64>,

        /// Output height in pixels.
        #[arg(long)]
        height: Option<f64>,

        /// Output file path, or `-` for stdout (default: stdout).
        #[arg(long, short)]
        output: Option<String>,
    },

    /// Shut down a running server (sends SIGTERM / taskkill).
    Shutdown,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Server subcommands need full tracing; client subcommands don't.
    let is_server = matches!(
        cli.command,
        Command::Http { .. } | Command::Headless { .. } | Command::Tui { .. }
    );
    if is_server {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info,jgd_server=debug".into()),
            )
            .init();
    }

    match cli.command {
        Command::Http {
            socket,
            host,
            port,
            headless,
        } => run_http(socket.as_deref(), &host, port, headless).await,
        Command::Headless {
            socket,
            listen,
            json,
        } => headless::run(socket.as_deref(), listen.as_deref(), json).await,
        Command::Tui { socket } => run_tui(socket.as_deref()).await,
        Command::List { json } => client::run_list(json),
        Command::Api { action, target } => run_api(action, target.as_deref()).await,
    }
}

async fn run_api(action: ApiCommand, target: Option<&str>) -> Result<()> {
    match action {
        ApiCommand::Plots { json } => client::run_api_plots(target, json).await,
        ApiCommand::Render {
            plot_id,
            format,
            width,
            height,
            output,
        } => {
            client::run_api_render(
                target,
                plot_id.as_deref(),
                &format,
                width,
                height,
                output.as_deref(),
            )
            .await
        }
        ApiCommand::Shutdown => client::run_api_shutdown(target),
    }
}

async fn run_http(
    socket_override: Option<&str>,
    host: &str,
    port: u16,
    headless: bool,
) -> Result<()> {
    let socket_addr = match socket_override {
        Some(s) => jgd_protocol::SocketAddr::parse(s)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        None => jgd_server::discovery::default_socket_addr(),
    };

    let hub = jgd_server::hub::spawn();

    // Start R connection listener.
    let listener = jgd_server::listener::Listener::bind(&socket_addr).await?;
    tracing::info!(%socket_addr, "listening for R connections");

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
            socket_path: socket_addr.to_string(),
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
    let transport = socket_addr.transport();
    let serve_handle = tokio::spawn(async move {
        jgd_server::serve::serve(
            listener,
            serve_hub,
            server_name,
            transport,
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

    Ok(())
}

async fn run_tui(socket_override: Option<&str>) -> Result<()> {
    let socket_addr = match socket_override {
        Some(s) => jgd_protocol::SocketAddr::parse(s)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        None => jgd_server::discovery::default_socket_addr(),
    };

    let hub = jgd_server::hub::spawn();

    // Start R connection listener.
    let listener = jgd_server::listener::Listener::bind(&socket_addr).await?;
    tracing::info!(%socket_addr, "listening for R connections");

    // Write discovery file so R clients can auto-connect.
    let discovery_path = jgd_server::discovery::default_path();
    if let Some(path) = &discovery_path {
        let info = jgd_server::discovery::DiscoveryInfo {
            server_name: "jgdmr".into(),
            socket_path: socket_addr.to_string(),
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
    let transport = socket_addr.transport();
    let serve_handle = tokio::spawn(async move {
        jgd_server::serve::serve(
            listener,
            serve_hub,
            server_name,
            transport,
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

    tui_result
}
