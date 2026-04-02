//! Test helpers for integration testing.
//!
//! Provides [`TestServer`] (starts a Hub + Listener on a temp Unix socket)
//! and [`RClient`] (connects to the server, sends/receives JSONL messages).

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_util::codec::Framed;

use jgd_protocol::codec::JsonLinesCodec;
use jgd_protocol::message::{Message, Transport};

use crate::hub::{self, HubHandle};
use crate::listener::Listener;
use crate::serve;

/// Default timeout for receiving messages in tests.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// A test server that runs a Hub + Listener on a temporary Unix socket.
///
/// The socket file and temp directory are cleaned up on drop.
pub struct TestServer {
    hub: HubHandle,
    socket_path: PathBuf,
    _tmp_dir: tempfile::TempDir,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TestServer {
    /// Start a test server on a random Unix socket.
    #[cfg(unix)]
    pub async fn start() -> Self {
        Self::start_with_name("test-server").await
    }

    /// Start a test server with a custom server name.
    #[cfg(unix)]
    pub async fn start_with_name(server_name: &str) -> Self {
        let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let socket_path = tmp_dir.path().join("jgd-test.sock");

        let listener = Listener::bind_unix(&socket_path).expect("failed to bind unix socket");
        let hub = hub::spawn();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let hub_clone = hub.clone();
        let name = server_name.to_owned();
        tokio::spawn(async move {
            serve::serve(listener, hub_clone, name, Transport::Unix, async {
                let _ = shutdown_rx.await;
            })
            .await;
        });

        TestServer {
            hub,
            socket_path,
            _tmp_dir: tmp_dir,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Get the Unix socket path for client connections.
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Get a handle to the Hub.
    pub fn hub(&self) -> &HubHandle {
        &self.hub
    }

    /// Shut down the server gracefully.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A simulated R client that connects to a test server via Unix socket.
pub struct RClient {
    framed: Framed<tokio::net::UnixStream, JsonLinesCodec>,
    /// The server_info welcome message, if received.
    pub server_info: Option<Message>,
}

impl RClient {
    /// Connect to a test server's Unix socket.
    #[cfg(unix)]
    pub async fn connect(socket_path: impl AsRef<std::path::Path>) -> Self {
        let stream = tokio::net::UnixStream::connect(socket_path.as_ref())
            .await
            .expect("failed to connect to test server");
        RClient {
            framed: Framed::new(stream, JsonLinesCodec::new()),
            server_info: None,
        }
    }

    /// Send a message to the server.
    pub async fn send(&mut self, msg: Message) {
        self.framed.send(msg).await.expect("failed to send message");
    }

    /// Receive the next message, with the default timeout.
    ///
    /// Panics if no message arrives within the timeout or the connection is closed.
    pub async fn recv(&mut self) -> Message {
        self.recv_timeout(DEFAULT_TIMEOUT)
            .await
            .expect("no message received within timeout")
    }

    /// Receive the next message within the given timeout.
    ///
    /// Returns `None` if the timeout elapses or the connection is closed.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Option<Message> {
        match tokio::time::timeout(timeout, self.framed.next()).await {
            Ok(Some(Ok(msg))) => Some(msg),
            Ok(Some(Err(e))) => panic!("read error: {e}"),
            Ok(None) => None, // connection closed
            Err(_) => None,   // timeout
        }
    }

    /// Trigger the deferred welcome by sending Ping, then wait for ServerInfo.
    ///
    /// Returns the ServerInfo message. Also stores it in `self.server_info`.
    pub async fn wait_for_welcome(&mut self) -> Message {
        self.send(Message::Ping).await;

        let msg = self.recv().await;
        assert!(
            matches!(&msg, Message::ServerInfo(_)),
            "expected ServerInfo, got {msg:?}"
        );
        self.server_info = Some(msg.clone());
        msg
    }

    /// Receive the next message, skipping any ServerInfo welcome messages.
    #[allow(dead_code)]
    pub async fn recv_skip_welcome(&mut self) -> Message {
        loop {
            let msg = self.recv().await;
            if matches!(&msg, Message::ServerInfo(_)) {
                self.server_info = Some(msg);
                continue;
            }
            return msg;
        }
    }

    /// Assert that no message arrives within the given duration.
    #[allow(dead_code)]
    pub async fn assert_no_message(&mut self, duration: Duration) {
        let result = self.recv_timeout(duration).await;
        assert!(result.is_none(), "expected no message, got {result:?}");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use jgd_protocol::GraphicsContext;
    use jgd_protocol::message::{
        DeviceInfo, FrameMessage, MetricsKind, MetricsRequest, Plot, ResizeMessage,
    };

    fn make_frame(session_id: Option<&str>) -> Message {
        Message::Frame(FrameMessage {
            plot: Plot {
                session_id: session_id.map(String::from),
                ops: vec![],
                device: DeviceInfo {
                    width: 800.0,
                    height: 600.0,
                    dpi: None,
                    bg: None,
                },
            },
            incremental: false,
            new_page: None,
            resize_replay: None,
            plot_index: None,
            plot_number: None,
            ext: None,
        })
    }

    #[tokio::test]
    async fn test_server_starts_and_accepts_connection() {
        let server = TestServer::start().await;
        let mut client = RClient::connect(server.socket_path()).await;

        let welcome = client.wait_for_welcome().await;
        assert!(matches!(welcome, Message::ServerInfo(_)));
    }

    #[tokio::test]
    async fn test_ping_pong_through_hub() {
        let server = TestServer::start().await;
        let mut client = RClient::connect(server.socket_path()).await;
        client.wait_for_welcome().await;

        // The Ping that triggered welcome also gets routed to Hub,
        // which responds with Pong.
        let pong = client.recv().await;
        assert_eq!(pong, Message::Pong);
    }

    #[tokio::test]
    async fn test_metrics_through_hub() {
        let server = TestServer::start().await;
        let mut client = RClient::connect(server.socket_path()).await;
        client.wait_for_welcome().await;
        // Consume pong from the initial ping.
        let _ = client.recv().await;

        let req = Message::MetricsRequest(MetricsRequest {
            id: 7,
            kind: MetricsKind::StrWidth,
            str: Some("test".into()),
            c: None,
            gc: GraphicsContext::default(),
        });
        client.send(req).await;

        let resp = client.recv().await;
        match resp {
            Message::MetricsResponse(r) => {
                assert_eq!(r.id, 7);
                assert!(r.width > 0.0);
            }
            other => panic!("expected MetricsResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_frame_broadcast_and_resize_routing() {
        let server = TestServer::start().await;
        let mut client = RClient::connect(server.socket_path()).await;
        client.wait_for_welcome().await;
        let _ = client.recv().await; // pong

        // Subscribe to broadcasts before sending frame.
        let mut sub = server.hub().subscribe();

        // Send frame from R.
        client.send(make_frame(Some("r-test-1"))).await;

        // Browser subscriber should receive it.
        let msg = tokio::time::timeout(DEFAULT_TIMEOUT, sub.recv())
            .await
            .expect("timeout")
            .expect("recv error");
        assert!(matches!(msg, Message::Frame(_)));

        // Browser sends resize → R client should receive it.
        server.hub().client_resize(ResizeMessage {
            width: 1024.0,
            height: 768.0,
            plot_index: None,
            session_id: None,
        });

        let resize = client.recv().await;
        match resize {
            Message::Resize(r) => {
                assert_eq!(r.width, 1024.0);
                assert_eq!(r.height, 768.0);
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_multiple_clients() {
        let server = TestServer::start().await;
        let mut client1 = RClient::connect(server.socket_path()).await;
        let mut client2 = RClient::connect(server.socket_path()).await;

        client1.wait_for_welcome().await;
        client2.wait_for_welcome().await;

        // Resize should reach both.
        server.hub().client_resize(ResizeMessage {
            width: 500.0,
            height: 400.0,
            plot_index: None,
            session_id: None,
        });

        // Both should receive pong (from welcome ping) and resize.
        let _ = client1.recv().await; // pong
        let _ = client2.recv().await; // pong
        let r1 = client1.recv().await;
        let r2 = client2.recv().await;
        assert!(matches!(r1, Message::Resize(_)));
        assert!(matches!(r2, Message::Resize(_)));
    }
}
