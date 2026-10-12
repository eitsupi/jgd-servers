//! Bind REST API listeners before publishing their addresses.

use anyhow::{Context, Result, bail};
use jgd_protocol::SocketAddr;

/// Parse a `--listen` URI string, or generate a supported platform default.
pub fn parse_listen_uri(uri: Option<&str>) -> Result<SocketAddr> {
    let addr = match uri {
        Some(s) => SocketAddr::parse(s).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => default_api_addr()?,
    };
    match &addr {
        #[cfg(unix)]
        SocketAddr::Unix(_) => Ok(addr),
        SocketAddr::Tcp(_) => Ok(addr),
        _ => bail!(
            "REST API address {addr} is unsupported; use tcp://127.0.0.1:0 or, on Unix, unix:///path"
        ),
    }
}

fn default_api_addr() -> Result<SocketAddr> {
    #[cfg(unix)]
    {
        Ok(jgd_server::discovery::default_socket_addr("jgdmr", "-api")?)
    }
    #[cfg(not(unix))]
    {
        // The API client supports TCP, not Windows named pipes. Port zero
        // chooses an available local port; advertise the actual bound port.
        Ok(SocketAddr::Tcp("127.0.0.1:0".into()))
    }
}

pub enum ApiListener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener, SocketPath),
    Tcp(tokio::net::TcpListener),
}

/// Removes only the socket created by this listener, even on error exits.
#[cfg(unix)]
pub struct SocketPath {
    path: std::path::PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl Drop for SocketPath {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = std::fs::symlink_metadata(&self.path)
            && meta.dev() == self.device
            && meta.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl ApiListener {
    pub async fn bind(addr: &SocketAddr) -> Result<Self> {
        match addr {
            #[cfg(unix)]
            SocketAddr::Unix(path) => {
                use std::os::unix::fs::MetadataExt;
                // Never unlink an existing path: it may belong to a live
                // server, be a symlink, or contain user data. Stale sockets
                // must be removed explicitly after checking their owner.
                let listener = tokio::net::UnixListener::bind(path)
                    .with_context(|| format!("failed to bind Unix socket: {}", path.display()))?;
                let meta = std::fs::symlink_metadata(path)?;
                Ok(Self::Unix(
                    listener,
                    SocketPath {
                        path: path.clone(),
                        device: meta.dev(),
                        inode: meta.ino(),
                    },
                ))
            }
            SocketAddr::Tcp(host_port) => {
                let listener = tokio::net::TcpListener::bind(host_port.as_str())
                    .await
                    .with_context(|| format!("failed to bind TCP: {host_port}"))?;
                Ok(Self::Tcp(listener))
            }
            _ => bail!("REST API address {addr} is unsupported on this platform"),
        }
    }

    pub fn address(&self) -> Result<SocketAddr> {
        match self {
            #[cfg(unix)]
            Self::Unix(_, path) => Ok(SocketAddr::Unix(path.path.clone())),
            Self::Tcp(listener) => Ok(SocketAddr::Tcp(listener.local_addr()?.to_string())),
        }
    }

    pub async fn serve(
        self,
        app: axum::Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(listener, _path) => {
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown)
                    .await?;
            }
            Self::Tcp(listener) => {
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown)
                    .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn parse_explicit_unix() {
        assert!(matches!(
            parse_listen_uri(Some("unix:///tmp/test.sock")).unwrap(),
            SocketAddr::Unix(_)
        ));
    }

    #[test]
    fn parse_explicit_tcp() {
        assert!(matches!(
            parse_listen_uri(Some("tcp://127.0.0.1:8080")).unwrap(),
            SocketAddr::Tcp(_)
        ));
    }

    #[test]
    fn reject_named_pipe_api() {
        assert!(parse_listen_uri(Some("npipe:////./pipe/jgdmr-api")).is_err());
    }

    #[test]
    fn parse_none_gives_supported_default() {
        let addr = parse_listen_uri(None).unwrap();
        #[cfg(unix)]
        assert!(matches!(addr, SocketAddr::Unix(_)));
        #[cfg(windows)]
        assert_eq!(addr.to_string(), "tcp://127.0.0.1:0");
    }

    #[tokio::test]
    async fn tcp_address_reports_assigned_port() {
        let listener = ApiListener::bind(&SocketAddr::Tcp("127.0.0.1:0".into()))
            .await
            .unwrap();
        let SocketAddr::Tcp(addr) = listener.address().unwrap() else {
            panic!("expected TCP");
        };
        assert_ne!(addr.parse::<std::net::SocketAddr>().unwrap().port(), 0);
    }
}
