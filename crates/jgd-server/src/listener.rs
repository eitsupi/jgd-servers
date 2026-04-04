//! Transport listeners for R client connections.
//!
//! Provides a unified [`Listener`] abstraction over:
//! - Unix domain sockets (Linux/macOS)
//! - Windows Named Pipes
//! - TCP sockets (opt-in via the `tcp` feature)

use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};

/// A transport-agnostic connection type.
pub enum Connection {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    #[cfg(windows)]
    NamedPipe(tokio::net::windows::named_pipe::NamedPipeServer),
    #[cfg(feature = "tcp")]
    Tcp(tokio::net::TcpStream),
}

impl AsyncRead for Connection {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Connection::Unix(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            #[cfg(windows)]
            Connection::NamedPipe(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            #[cfg(feature = "tcp")]
            Connection::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Connection::Unix(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            #[cfg(windows)]
            Connection::NamedPipe(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            #[cfg(feature = "tcp")]
            Connection::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Connection::Unix(s) => std::pin::Pin::new(s).poll_flush(cx),
            #[cfg(windows)]
            Connection::NamedPipe(s) => std::pin::Pin::new(s).poll_flush(cx),
            #[cfg(feature = "tcp")]
            Connection::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Connection::Unix(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            #[cfg(windows)]
            Connection::NamedPipe(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            #[cfg(feature = "tcp")]
            Connection::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// A transport-agnostic listener that accepts R client connections.
pub enum Listener {
    #[cfg(unix)]
    Unix {
        inner: tokio::net::UnixListener,
        path: PathBuf,
    },
    /// Windows Named Pipe listener.
    ///
    /// Following the arf pattern: hold a pre-created `NamedPipeServer` that
    /// waits for a client via `connect()`. On each accepted connection, the
    /// current instance is handed off and a new one is created for the next
    /// client.
    #[cfg(windows)]
    NamedPipe {
        pipe_name: String,
        /// The next server instance waiting for a client connection.
        next_server: tokio::net::windows::named_pipe::NamedPipeServer,
    },
    #[cfg(feature = "tcp")]
    Tcp(tokio::net::TcpListener),
}

impl Listener {
    /// Bind a listener from a parsed [`jgd_protocol::SocketAddr`].
    ///
    /// This is the preferred entry point.  TCP requires the `tcp` feature.
    pub async fn bind(addr: &jgd_protocol::SocketAddr) -> std::io::Result<Self> {
        match addr {
            #[cfg(unix)]
            jgd_protocol::SocketAddr::Unix(path) => Self::bind_unix(path),
            jgd_protocol::SocketAddr::Npipe(name) => {
                #[cfg(windows)]
                {
                    Self::bind_named_pipe(name)
                }
                #[cfg(not(windows))]
                {
                    let _ = name;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "npipe:// is only supported on Windows",
                    ))
                }
            }
            jgd_protocol::SocketAddr::Tcp(host_port) => {
                #[cfg(feature = "tcp")]
                {
                    Self::bind_tcp(host_port).await
                }
                #[cfg(not(feature = "tcp"))]
                {
                    let _ = host_port;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "TCP support requires the `tcp` feature",
                    ))
                }
            }
        }
    }

    /// Bind a Unix domain socket listener.
    ///
    /// Removes a stale socket file if one exists at the given path.
    #[cfg(unix)]
    pub fn bind_unix(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Remove stale socket if it exists, but only if it is actually a socket.
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            use std::os::unix::fs::FileTypeExt as _;
            if meta.file_type().is_socket() {
                std::fs::remove_file(&path)?;
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("path exists but is not a socket: {}", path.display()),
                ));
            }
        }

        let inner = tokio::net::UnixListener::bind(&path)?;
        tracing::info!(?path, "listening on Unix socket");
        Ok(Listener::Unix { inner, path })
    }

    /// Create a Named Pipe listener.
    ///
    /// The pipe name should follow the Windows convention:
    /// `\\.\pipe\jgd-<unique-id>`.
    #[cfg(windows)]
    pub fn bind_named_pipe(pipe_name: impl Into<String>) -> std::io::Result<Self> {
        use tokio::net::windows::named_pipe::ServerOptions;

        let pipe_name = pipe_name.into();
        let next_server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)?;
        tracing::info!(%pipe_name, "listening on Named Pipe");
        Ok(Listener::NamedPipe {
            pipe_name,
            next_server,
        })
    }

    /// Bind a TCP listener.
    #[cfg(feature = "tcp")]
    pub async fn bind_tcp(addr: impl tokio::net::ToSocketAddrs) -> std::io::Result<Self> {
        let inner = tokio::net::TcpListener::bind(addr).await?;
        let local_addr = inner.local_addr()?;
        tracing::info!(%local_addr, "listening on TCP");
        Ok(Listener::Tcp(inner))
    }

    /// Accept the next incoming connection.
    pub async fn accept(&mut self) -> std::io::Result<Connection> {
        match self {
            #[cfg(unix)]
            Listener::Unix { inner, .. } => {
                let (stream, _addr) = inner.accept().await?;
                Ok(Connection::Unix(stream))
            }
            #[cfg(windows)]
            Listener::NamedPipe {
                pipe_name,
                next_server,
            } => {
                use tokio::net::windows::named_pipe::ServerOptions;

                // Wait for a client to connect to the current instance.
                next_server.connect().await?;

                // Hand off the connected instance and create a new one for
                // the next client (following the arf pattern).
                let connected =
                    std::mem::replace(next_server, ServerOptions::new().create(pipe_name)?);
                Ok(Connection::NamedPipe(connected))
            }
            #[cfg(feature = "tcp")]
            Listener::Tcp(inner) => {
                let (stream, _addr) = inner.accept().await?;
                Ok(Connection::Tcp(stream))
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        match self {
            #[cfg(unix)]
            Listener::Unix { path, .. } => {
                if let Err(e) = std::fs::remove_file(&*path) {
                    tracing::warn!(?path, %e, "failed to remove Unix socket on drop");
                }
            }
            // Named Pipes are cleaned up by the OS when all handles are closed.
            #[cfg(windows)]
            Listener::NamedPipe { .. } => {}
            #[cfg(feature = "tcp")]
            Listener::Tcp(_) => {}
        }
    }
}
