//! Socket address types and URI parsing for jgd connections.
//!
//! The jgd protocol uses URI-formatted socket addresses:
//!
//! - `unix:///path/to/socket` — Unix domain socket
//! - `npipe:////./pipe/name` — Windows Named Pipe (Docker convention)
//! - `tcp://host:port` — TCP socket
//!
//! Raw Unix paths without a scheme (e.g. `/tmp/jgd.sock`) are accepted
//! for backward compatibility.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::message::Transport;

/// A parsed jgd socket address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketAddr {
    /// Unix domain socket.
    Unix(PathBuf),
    /// Windows Named Pipe (full pipe name, e.g. `\\.\pipe\jgd-1234`).
    Npipe(String),
    /// TCP socket (`host:port`).
    Tcp(String),
}

/// Error returned when a socket address string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseSocketAddrError(String);

impl SocketAddr {
    /// Parse a URI or raw path into a [`SocketAddr`].
    ///
    /// Accepts:
    /// - `unix:///path` → [`SocketAddr::Unix`]
    /// - `npipe:////./pipe/name` → [`SocketAddr::Npipe`] (path converted to `\\.\pipe\name`)
    /// - `tcp://host:port` → [`SocketAddr::Tcp`]
    /// - `/path/to/socket` (no scheme) → [`SocketAddr::Unix`] (legacy compat)
    pub fn parse(s: &str) -> Result<Self, ParseSocketAddrError> {
        if let Some(rest) = s.strip_prefix("unix://") {
            if rest.is_empty() {
                return Err(ParseSocketAddrError(
                    "unix:// URI requires a path".into(),
                ));
            }
            Ok(SocketAddr::Unix(PathBuf::from(rest)))
        } else if let Some(rest) = s.strip_prefix("npipe://") {
            if rest.is_empty() {
                return Err(ParseSocketAddrError(
                    "npipe:// URI requires a pipe path".into(),
                ));
            }
            // Convert forward slashes to backslashes for Windows pipe path.
            // npipe:////./pipe/name → \\.\pipe\name
            let pipe_name = rest.replace('/', "\\");
            Ok(SocketAddr::Npipe(pipe_name))
        } else if let Some(rest) = s.strip_prefix("tcp://") {
            if rest.is_empty() {
                return Err(ParseSocketAddrError(
                    "tcp:// URI requires host:port".into(),
                ));
            }
            Ok(SocketAddr::Tcp(rest.into()))
        } else if s.starts_with('/') || s.starts_with('.') {
            // Legacy: raw Unix path without scheme.
            Ok(SocketAddr::Unix(PathBuf::from(s)))
        } else {
            Err(ParseSocketAddrError(format!(
                "unsupported socket address: {s}\n\
                 expected: unix:///path, npipe:////./pipe/name, or tcp://host:port"
            )))
        }
    }

    /// Return the [`Transport`] variant corresponding to this address.
    pub fn transport(&self) -> Transport {
        match self {
            SocketAddr::Unix(_) => Transport::Unix,
            SocketAddr::Npipe(_) => Transport::Npipe,
            SocketAddr::Tcp(_) => Transport::Tcp,
        }
    }
}

impl FromStr for SocketAddr {
    type Err = ParseSocketAddrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SocketAddr::parse(s)
    }
}

impl fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SocketAddr::Unix(path) => write!(f, "unix://{}", path.display()),
            SocketAddr::Npipe(name) => {
                // Convert backslashes back to forward slashes for URI form.
                let uri_path = name.replace('\\', "/");
                write!(f, "npipe://{uri_path}")
            }
            SocketAddr::Tcp(addr) => write!(f, "tcp://{addr}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unix() {
        let addr = SocketAddr::parse("unix:///tmp/jgd.sock").unwrap();
        assert_eq!(addr, SocketAddr::Unix(PathBuf::from("/tmp/jgd.sock")));
        assert_eq!(addr.transport(), Transport::Unix);
    }

    #[test]
    fn parse_unix_display_roundtrip() {
        let addr = SocketAddr::parse("unix:///tmp/jgd.sock").unwrap();
        assert_eq!(addr.to_string(), "unix:///tmp/jgd.sock");
    }

    #[test]
    fn parse_npipe() {
        let addr = SocketAddr::parse("npipe:////./pipe/jgd-1234").unwrap();
        assert_eq!(
            addr,
            SocketAddr::Npipe(r"\\.\pipe\jgd-1234".into())
        );
        assert_eq!(addr.transport(), Transport::Npipe);
    }

    #[test]
    fn parse_npipe_display_roundtrip() {
        let uri = "npipe:////./pipe/jgd-1234";
        let addr = SocketAddr::parse(uri).unwrap();
        assert_eq!(addr.to_string(), uri);
    }

    #[test]
    fn parse_tcp() {
        let addr = SocketAddr::parse("tcp://127.0.0.1:9000").unwrap();
        assert_eq!(addr, SocketAddr::Tcp("127.0.0.1:9000".into()));
        assert_eq!(addr.transport(), Transport::Tcp);
    }

    #[test]
    fn parse_legacy_unix_path() {
        let addr = SocketAddr::parse("/tmp/jgd.sock").unwrap();
        assert_eq!(addr, SocketAddr::Unix(PathBuf::from("/tmp/jgd.sock")));
    }

    #[test]
    fn parse_legacy_relative_path() {
        let addr = SocketAddr::parse("./jgd.sock").unwrap();
        assert_eq!(addr, SocketAddr::Unix(PathBuf::from("./jgd.sock")));
    }

    #[test]
    fn parse_empty_unix_fails() {
        assert!(SocketAddr::parse("unix://").is_err());
    }

    #[test]
    fn parse_empty_tcp_fails() {
        assert!(SocketAddr::parse("tcp://").is_err());
    }

    #[test]
    fn parse_unknown_scheme_fails() {
        assert!(SocketAddr::parse("http://localhost:3000").is_err());
    }

    #[test]
    fn from_str_works() {
        let addr: SocketAddr = "tcp://localhost:8080".parse().unwrap();
        assert_eq!(addr, SocketAddr::Tcp("localhost:8080".into()));
    }
}
