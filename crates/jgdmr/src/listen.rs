//! Shared listen-address handling for `headless` and (future) `tui` subcommands.
//!
//! Parses `--listen` URI into a [`jgd_protocol::SocketAddr`] and serves an
//! axum [`Router`] on it.

use anyhow::{Context, Result, bail};
use jgd_protocol::SocketAddr;

/// Parse a `--listen` URI string, or generate a platform default.
pub fn parse_listen_uri(uri: Option<&str>) -> Result<SocketAddr> {
    match uri {
        Some(s) => SocketAddr::parse(s).map_err(|e| anyhow::anyhow!("{e}")),
        None => Ok(default_api_addr()),
    }
}

/// Generate a default API socket address (separate from the R connection socket).
fn default_api_addr() -> SocketAddr {
    jgd_server::discovery::default_socket_addr("jgdmr", "-api")
}

/// Serve an axum [`Router`] on the given address until `shutdown` resolves.
pub async fn serve(
    addr: &SocketAddr,
    app: axum::Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    match addr {
        #[cfg(unix)]
        SocketAddr::Unix(path) => {
            // Remove stale socket if it exists.
            if path.exists() {
                std::fs::remove_file(path)
                    .with_context(|| format!("failed to remove stale socket: {}", path.display()))?;
            }
            let listener = tokio::net::UnixListener::bind(path)
                .with_context(|| format!("failed to bind Unix socket: {}", path.display()))?;
            tracing::info!(%addr, "REST API listening");

            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await?;

            let _ = std::fs::remove_file(path);
        }
        SocketAddr::Tcp(host_port) => {
            let listener = tokio::net::TcpListener::bind(host_port.as_str())
                .await
                .with_context(|| format!("failed to bind TCP: {host_port}"))?;
            tracing::info!(%addr, "REST API listening");

            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await?;
        }
        #[allow(unreachable_patterns)]
        _ => bail!("listen address {addr} is not supported on this platform"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_explicit_unix() {
        let addr = parse_listen_uri(Some("unix:///tmp/test.sock")).unwrap();
        assert!(matches!(addr, SocketAddr::Unix(_)));
    }

    #[test]
    fn parse_explicit_tcp() {
        let addr = parse_listen_uri(Some("tcp://127.0.0.1:8080")).unwrap();
        assert!(matches!(addr, SocketAddr::Tcp(_)));
    }

    #[test]
    fn parse_none_gives_default() {
        let addr = parse_listen_uri(None).unwrap();
        // On Unix, default is a Unix socket.
        #[cfg(unix)]
        assert!(matches!(addr, SocketAddr::Unix(_)));
    }
}
