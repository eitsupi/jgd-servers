//! Central Hub for R session management and message routing.
//!
//! The Hub is an actor that coordinates communication between R sessions
//! (connected via Unix socket / Named Pipe / TCP) and browser clients
//! (connected via WebSocket).
//!
//! Responsibilities:
//! - R session lifecycle (register, unregister, session ID extraction)
//! - Frame broadcasting to browser clients
//! - Resize routing from browsers to R sessions (with dedup)
//! - Session ID reuse detection and remapping
//! - Server-side font metrics computation (via parley)

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};

use jgd_protocol::Plot;
use jgd_protocol::message::{FrameMessage, Message, ResizeMessage};

/// Connection-scoped numeric identifier for an R session.
pub type ConnId = u64;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Summary of a stored plot, returned by the REST API.
#[derive(Debug, Clone, Serialize)]
pub struct PlotSummary {
    pub session_id: String,
    pub plot_index: usize,
    pub width: f64,
    pub height: f64,
    pub op_count: usize,
}

/// Clonable handle for communicating with the Hub actor.
#[derive(Clone)]
pub struct HubHandle {
    cmd_tx: mpsc::UnboundedSender<HubCommand>,
    broadcast_tx: broadcast::Sender<Message>,
}

impl HubHandle {
    /// Register a new R session.
    ///
    /// Returns the connection ID and a receiver for messages the Hub sends
    /// back to R (resize commands, metrics responses, pong, etc.).
    pub fn register_session(&self) -> (ConnId, mpsc::UnboundedReceiver<Message>) {
        let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = self
            .cmd_tx
            .send(HubCommand::RegisterSession { conn_id, tx });
        (conn_id, rx)
    }

    /// Unregister an R session when the connection closes.
    pub fn unregister_session(&self, conn_id: ConnId) {
        let _ = self.cmd_tx.send(HubCommand::UnregisterSession { conn_id });
    }

    /// Forward a message received from an R session to the Hub for routing.
    pub fn r_message(&self, conn_id: ConnId, msg: Message) {
        let _ = self.cmd_tx.send(HubCommand::RMessage { conn_id, msg });
    }

    /// Forward a resize message from a browser client.
    pub fn client_resize(&self, msg: ResizeMessage) {
        let _ = self.cmd_tx.send(HubCommand::ClientResize(msg));
    }

    /// Subscribe to messages broadcast to browser clients (frames, close, etc.).
    pub fn subscribe(&self) -> broadcast::Receiver<Message> {
        self.broadcast_tx.subscribe()
    }

    /// List all stored plots with metadata.
    pub async fn get_plots(&self) -> Vec<PlotSummary> {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HubCommand::GetPlots { reply: tx });
        rx.await.unwrap_or_default()
    }

    /// Get a stored plot by session ID and optional plot index.
    ///
    /// When `plot_index` is `None`, returns the latest plot for the session.
    pub async fn get_plot(&self, session_id: &str, plot_index: Option<usize>) -> Option<Plot> {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HubCommand::GetPlot {
            session_id: session_id.to_owned(),
            plot_index,
            reply: tx,
        });
        rx.await.ok().flatten()
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

enum HubCommand {
    RegisterSession {
        conn_id: ConnId,
        tx: mpsc::UnboundedSender<Message>,
    },
    UnregisterSession {
        conn_id: ConnId,
    },
    RMessage {
        conn_id: ConnId,
        msg: Message,
    },
    ClientResize(ResizeMessage),
    GetPlots {
        reply: oneshot::Sender<Vec<PlotSummary>>,
    },
    GetPlot {
        session_id: String,
        plot_index: Option<usize>,
        reply: oneshot::Sender<Option<Plot>>,
    },
}

struct SessionState {
    session_id: Option<String>,
    tx: mpsc::UnboundedSender<Message>,
    last_resize_w: f64,
    last_resize_h: f64,
    last_resize_had_plot_index: bool,
    remapped: bool,
    /// DPI from the R device (set from the first frame's DeviceInfo).
    dpi: Option<f64>,
}

struct HubState {
    sessions: HashMap<ConnId, SessionState>,
    session_id_to_conn: HashMap<String, ConnId>,
    retired_session_ids: HashSet<String>,
    session_reuse_counter: u64,
    broadcast_tx: broadcast::Sender<Message>,
    /// Plot history per session, keyed by session_id.
    plots: HashMap<String, Vec<Plot>>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Spawn the Hub actor, returning a handle for communication.
///
/// The Hub runs as a background tokio task and processes commands
/// sequentially. All state mutations happen inside this single task,
/// so no locking is needed.
pub fn spawn() -> HubHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (broadcast_tx, _) = broadcast::channel(256);
    let handle = HubHandle {
        cmd_tx,
        broadcast_tx: broadcast_tx.clone(),
    };
    tokio::spawn(run(cmd_rx, broadcast_tx));
    handle
}

// ---------------------------------------------------------------------------
// Actor loop
// ---------------------------------------------------------------------------

async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<HubCommand>,
    broadcast_tx: broadcast::Sender<Message>,
) {
    let mut state = HubState {
        sessions: HashMap::new(),
        session_id_to_conn: HashMap::new(),
        retired_session_ids: HashSet::new(),
        session_reuse_counter: 0,
        broadcast_tx,
        plots: HashMap::new(),
    };

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            HubCommand::RegisterSession { conn_id, tx } => {
                tracing::debug!(conn_id, "session registered");
                state.sessions.insert(
                    conn_id,
                    SessionState {
                        session_id: None,
                        tx,
                        last_resize_w: 0.0,
                        last_resize_h: 0.0,
                        last_resize_had_plot_index: false,
                        remapped: false,
                        dpi: None,
                    },
                );
            }
            HubCommand::UnregisterSession { conn_id } => {
                state.unregister_session(conn_id);
            }
            HubCommand::RMessage { conn_id, msg } => {
                state.handle_r_message(conn_id, msg);
            }
            HubCommand::ClientResize(msg) => {
                state.handle_client_resize(msg);
            }
            HubCommand::GetPlots { reply } => {
                let summaries = state
                    .plots
                    .iter()
                    .flat_map(|(sid, plots)| {
                        plots.iter().enumerate().map(|(idx, plot)| PlotSummary {
                            session_id: sid.clone(),
                            plot_index: idx,
                            width: plot.device.width,
                            height: plot.device.height,
                            op_count: plot.ops.len(),
                        })
                    })
                    .collect();
                let _ = reply.send(summaries);
            }
            HubCommand::GetPlot {
                session_id,
                plot_index,
                reply,
            } => {
                let result = state.plots.get(&session_id).and_then(|plots| {
                    match plot_index {
                        Some(idx) => plots.get(idx),
                        None => plots.last(),
                    }
                    .cloned()
                });
                let _ = reply.send(result);
            }
        }
    }

    tracing::debug!("hub shutting down");
}

// ---------------------------------------------------------------------------
// State logic
// ---------------------------------------------------------------------------

impl HubState {
    /// Remove a session and retire its ID.
    fn unregister_session(&mut self, conn_id: ConnId) {
        let Some(session) = self.sessions.remove(&conn_id) else {
            return;
        };

        tracing::debug!(conn_id, session_id = ?session.session_id, "session unregistered");

        if let Some(id) = session.session_id {
            self.session_id_to_conn.remove(&id);
            self.plots.remove(&id);
            self.retired_session_ids.insert(id);

            // Cap retired set to prevent unbounded growth.  Evict an
            // arbitrary half (HashSet has no defined order) rather than
            // clearing entirely.  Some recently-retired IDs may be
            // evicted, but the window for missed reuse detection is
            // narrow and the consequence is only a cosmetic session-ID
            // overlap on the browser side.
            if self.retired_session_ids.len() > 1000 {
                let retain_count = self.retired_session_ids.len() / 2;
                let to_remove: Vec<_> = self
                    .retired_session_ids
                    .iter()
                    .skip(retain_count)
                    .cloned()
                    .collect();
                for id in to_remove {
                    self.retired_session_ids.remove(&id);
                }
            }
        }
    }

    /// Assign a session ID to a connection, handling reuse detection.
    ///
    /// R session IDs are PID-based (e.g. `r-12345-1`) and can be reused
    /// when a process reopens a device. The Hub detects this and appends
    /// a disambiguation suffix (e.g. `r-12345-1:2`).
    fn update_session_id(&mut self, conn_id: ConnId, raw_id: String) {
        let Some(session) = self.sessions.get_mut(&conn_id) else {
            return;
        };

        // Already assigned — ignore subsequent frames.
        if session.session_id.is_some() {
            return;
        }

        let final_id = if self.retired_session_ids.contains(&raw_id)
            || self.session_id_to_conn.contains_key(&raw_id)
        {
            self.session_reuse_counter += 1;
            session.remapped = true;
            format!("{}:{}", raw_id, self.session_reuse_counter)
        } else {
            raw_id
        };

        tracing::debug!(conn_id, %final_id, "session ID assigned");
        session.session_id = Some(final_id.clone());
        self.session_id_to_conn.insert(final_id, conn_id);
    }

    /// Route a message from an R session.
    fn handle_r_message(&mut self, conn_id: ConnId, msg: Message) {
        match msg {
            Message::Frame(frame) => self.handle_frame(conn_id, frame),
            Message::MetricsRequest(req) => {
                // Compute metrics server-side via parley — no browser round-trip.
                let dpi = self.sessions.get(&conn_id).and_then(|s| s.dpi);
                let resp = jgd_font_metrics::compute_metrics(&req, dpi);
                if let Some(session) = self.sessions.get(&conn_id) {
                    let _ = session.tx.send(Message::MetricsResponse(resp));
                }
            }
            Message::Ping => {
                if let Some(session) = self.sessions.get(&conn_id) {
                    let _ = session.tx.send(Message::Pong);
                }
            }
            Message::Close => {
                let _ = self.broadcast_tx.send(Message::Close);
            }
            other => {
                // Forward unrecognized messages to browser clients.
                let _ = self.broadcast_tx.send(other);
            }
        }
    }

    /// Handle a frame from R: extract session ID, inject/remap, and broadcast.
    fn handle_frame(&mut self, conn_id: ConnId, mut frame: FrameMessage) {
        // Store DPI from the device info for metrics computation.
        if let Some(dpi) = frame.plot.device.dpi
            && let Some(session) = self.sessions.get_mut(&conn_id)
        {
            session.dpi = Some(dpi);
        }

        // Extract session ID from the first frame for lazy assignment.
        if let Some(ref session_id) = frame.plot.session_id {
            self.update_session_id(conn_id, session_id.clone());
        }

        // Inject or replace session ID in outgoing frame.
        if let Some(session) = self.sessions.get(&conn_id)
            && let Some(ref session_id) = session.session_id
            && (session.remapped || frame.plot.session_id.is_none())
        {
            frame.plot.session_id = Some(session_id.clone());
        }

        // Store the plot in history for REST API access.
        if let Some(ref sid) = frame.plot.session_id {
            let plots = self.plots.entry(sid.clone()).or_default();

            let plot_idx = if plots.is_empty() || frame.new_page == Some(true) {
                // New plot page — append to history.
                plots.push(frame.plot.clone());
                plots.len() - 1
            } else if let Some(idx) = frame.plot_index.map(|i| i as usize) {
                // Targeted update (e.g. resize replay) — update specific entry.
                if let Some(p) = plots.get_mut(idx) {
                    *p = frame.plot.clone();
                    idx
                } else {
                    tracing::warn!(
                        session_id = sid.as_str(),
                        idx,
                        len = plots.len(),
                        "plot_index out of bounds, updating last"
                    );
                    let last = plots.len() - 1;
                    plots[last] = frame.plot.clone();
                    last
                }
            } else if frame.incremental {
                // Incremental update — append new ops to the latest entry.
                let last = plots.len() - 1;
                plots[last].ops.extend(frame.plot.ops.iter().cloned());
                plots[last].device = frame.plot.device.clone();
                last
            } else {
                // Complete (non-incremental) update — replace the latest entry.
                let last = plots.len() - 1;
                plots[last] = frame.plot.clone();
                last
            };

            frame.plot_index = Some(plot_idx as u32);
        }

        let _ = self.broadcast_tx.send(Message::Frame(frame));
    }

    /// Route a resize from a browser client to R session(s).
    fn handle_client_resize(&mut self, msg: ResizeMessage) {
        // plotIndex resize: route only to the owning session.
        if let Some(plot_index) = msg.plot_index {
            let Some(session_id) = &msg.session_id else {
                return; // No session to route to.
            };
            let Some(&conn_id) = self.session_id_to_conn.get(session_id) else {
                return; // Target session is dead.
            };
            let Some(session) = self.sessions.get_mut(&conn_id) else {
                return;
            };

            session.last_resize_w = msg.width;
            session.last_resize_h = msg.height;
            session.last_resize_had_plot_index = true;

            let _ = session.tx.send(Message::Resize(ResizeMessage {
                width: msg.width,
                height: msg.height,
                plot_index: Some(plot_index),
                session_id: None, // R doesn't need the session ID.
            }));
            return;
        }

        // Normal resize: broadcast to all sessions with per-session dedup.
        for session in self.sessions.values_mut() {
            // Dedup: skip if same dimensions AND the previous resize was
            // NOT a plotIndex resize. After a plotIndex resize, one normal
            // resize at the same dimensions must pass through (it targets
            // the current display list, not the historical snapshot).
            if msg.width == session.last_resize_w
                && msg.height == session.last_resize_h
                && !session.last_resize_had_plot_index
            {
                continue;
            }

            session.last_resize_had_plot_index = false;
            session.last_resize_w = msg.width;
            session.last_resize_h = msg.height;

            let _ = session.tx.send(Message::Resize(ResizeMessage {
                width: msg.width,
                height: msg.height,
                plot_index: None,
                session_id: None,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::message::{DeviceInfo, Plot};

    fn make_frame(session_id: Option<&str>) -> FrameMessage {
        make_frame_opts(session_id, None, None)
    }

    fn make_frame_opts(
        session_id: Option<&str>,
        new_page: Option<bool>,
        plot_index: Option<u32>,
    ) -> FrameMessage {
        FrameMessage {
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
            new_page,
            resize_replay: None,
            plot_index,
            plot_number: None,
            ext: None,
        }
    }

    #[tokio::test]
    async fn register_and_unregister_session() {
        let hub = spawn();
        let (conn_id, _rx) = hub.register_session();
        assert!(conn_id > 0);
        hub.unregister_session(conn_id);

        // Give the actor a moment to process.
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn frame_broadcast_to_subscriber() {
        let hub = spawn();
        let (conn_id, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        let frame = make_frame(Some("r-100-1"));
        hub.r_message(conn_id, Message::Frame(frame));

        let msg = sub.recv().await.unwrap();
        match msg {
            Message::Frame(f) => {
                assert_eq!(f.plot.session_id.as_deref(), Some("r-100-1"));
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_id_injected_when_missing() {
        let hub = spawn();
        let (conn_id, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // First frame with session ID sets the assignment.
        hub.r_message(conn_id, Message::Frame(make_frame(Some("r-200-1"))));
        let _ = sub.recv().await.unwrap();

        // Second frame without session ID gets it injected.
        hub.r_message(conn_id, Message::Frame(make_frame(None)));
        let msg = sub.recv().await.unwrap();
        match msg {
            Message::Frame(f) => {
                assert_eq!(f.plot.session_id.as_deref(), Some("r-200-1"));
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_id_reuse_remapped() {
        let hub = spawn();
        let mut sub = hub.subscribe();

        // Session 1 registers and sends a frame.
        let (conn1, _rx1) = hub.register_session();
        hub.r_message(conn1, Message::Frame(make_frame(Some("r-300-1"))));
        let _ = sub.recv().await.unwrap();

        // Session 1 disconnects.
        hub.unregister_session(conn1);
        tokio::task::yield_now().await;

        // Session 2 reuses the same R session ID.
        let (conn2, _rx2) = hub.register_session();
        hub.r_message(conn2, Message::Frame(make_frame(Some("r-300-1"))));
        let msg = sub.recv().await.unwrap();
        match msg {
            Message::Frame(f) => {
                // Should be remapped with suffix.
                let sid = f.plot.session_id.unwrap();
                assert!(
                    sid.starts_with("r-300-1:"),
                    "expected remapped ID, got {sid}"
                );
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ping_responds_with_pong() {
        let hub = spawn();
        let (conn_id, mut rx) = hub.register_session();

        hub.r_message(conn_id, Message::Ping);

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, Message::Pong);
    }

    #[tokio::test]
    async fn normal_resize_broadcast_to_all_sessions() {
        let hub = spawn();
        let (_conn1, mut rx1) = hub.register_session();
        let (_conn2, mut rx2) = hub.register_session();

        hub.client_resize(ResizeMessage {
            width: 1024.0,
            height: 768.0,
            plot_index: None,
            session_id: None,
        });

        let msg1 = rx1.recv().await.unwrap();
        let msg2 = rx2.recv().await.unwrap();
        assert!(matches!(msg1, Message::Resize(_)));
        assert!(matches!(msg2, Message::Resize(_)));
    }

    #[tokio::test]
    async fn resize_dedup_skips_duplicate_dimensions() {
        let hub = spawn();
        let (_conn, mut rx) = hub.register_session();

        let resize = ResizeMessage {
            width: 800.0,
            height: 600.0,
            plot_index: None,
            session_id: None,
        };

        hub.client_resize(resize.clone());
        let _ = rx.recv().await.unwrap(); // First goes through.

        hub.client_resize(resize.clone());
        // Second should be deduped — use a timeout to verify no message.
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(result.is_err(), "expected timeout (deduped), got message");
    }

    #[tokio::test]
    async fn resize_after_plot_index_not_deduped() {
        let hub = spawn();
        let (conn, mut rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // Assign session ID via frame.
        hub.r_message(conn, Message::Frame(make_frame(Some("r-400-1"))));
        let _ = sub.recv().await.unwrap();

        // plotIndex resize at 800x600.
        hub.client_resize(ResizeMessage {
            width: 800.0,
            height: 600.0,
            plot_index: Some(0),
            session_id: Some("r-400-1".into()),
        });
        let _ = rx.recv().await.unwrap();

        // Normal resize at same dimensions — should NOT be deduped.
        hub.client_resize(ResizeMessage {
            width: 800.0,
            height: 600.0,
            plot_index: None,
            session_id: None,
        });
        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(
            msg.is_ok(),
            "expected resize to pass through after plotIndex"
        );
    }

    #[tokio::test]
    async fn plot_index_resize_routed_to_owning_session() {
        let hub = spawn();
        let (conn1, mut rx1) = hub.register_session();
        let (_conn2, mut rx2) = hub.register_session();
        let mut sub = hub.subscribe();

        // Assign session ID to conn1.
        hub.r_message(conn1, Message::Frame(make_frame(Some("r-500-1"))));
        let _ = sub.recv().await.unwrap();

        // plotIndex resize targeting conn1's session.
        hub.client_resize(ResizeMessage {
            width: 640.0,
            height: 480.0,
            plot_index: Some(2),
            session_id: Some("r-500-1".into()),
        });

        // conn1 should receive the resize.
        let msg = rx1.recv().await.unwrap();
        match msg {
            Message::Resize(r) => {
                assert_eq!(r.plot_index, Some(2));
                assert!(r.session_id.is_none()); // Stripped for R.
            }
            other => panic!("expected Resize, got {other:?}"),
        }

        // conn2 should NOT receive it.
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx2.recv()).await;
        assert!(result.is_err(), "conn2 should not receive plotIndex resize");
    }

    #[tokio::test]
    async fn close_broadcast_to_clients() {
        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        hub.r_message(conn, Message::Close);

        let msg = sub.recv().await.unwrap();
        assert_eq!(msg, Message::Close);
    }

    #[tokio::test]
    async fn metrics_computed_server_side() {
        let hub = spawn();
        let (conn, mut rx) = hub.register_session();

        let req = jgd_protocol::message::MetricsRequest {
            id: 42,
            kind: jgd_protocol::message::MetricsKind::StrWidth,
            str: Some("Hello".into()),
            c: None,
            gc: jgd_protocol::GraphicsContext::default(),
        };
        hub.r_message(conn, Message::MetricsRequest(req));

        let msg = rx.recv().await.unwrap();
        match msg {
            Message::MetricsResponse(resp) => {
                assert_eq!(resp.id, 42);
                assert!(resp.width > 0.0);
            }
            other => panic!("expected MetricsResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn plot_history_new_page_appends() {
        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // First frame (no new_page) — creates first entry.
        hub.r_message(conn, Message::Frame(make_frame(Some("r-600-1"))));
        let _ = sub.recv().await.unwrap();

        // Second frame with new_page=true — appends.
        hub.r_message(
            conn,
            Message::Frame(make_frame_opts(Some("r-600-1"), Some(true), None)),
        );
        let _ = sub.recv().await.unwrap();

        let plots = hub.get_plots().await;
        assert_eq!(plots.len(), 2);
        assert_eq!(
            plots.iter().filter(|p| p.session_id == "r-600-1").count(),
            2
        );
    }

    #[tokio::test]
    async fn plot_history_update_replaces_latest() {
        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // First frame.
        hub.r_message(conn, Message::Frame(make_frame(Some("r-700-1"))));
        let _ = sub.recv().await.unwrap();

        // Second frame without new_page — replaces.
        hub.r_message(conn, Message::Frame(make_frame(Some("r-700-1"))));
        let _ = sub.recv().await.unwrap();

        let plots = hub.get_plots().await;
        assert_eq!(plots.len(), 1);
    }

    #[tokio::test]
    async fn get_plot_by_index() {
        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // Create two plots.
        hub.r_message(conn, Message::Frame(make_frame(Some("r-800-1"))));
        let _ = sub.recv().await.unwrap();
        hub.r_message(
            conn,
            Message::Frame(make_frame_opts(Some("r-800-1"), Some(true), None)),
        );
        let _ = sub.recv().await.unwrap();

        // Get by index.
        assert!(hub.get_plot("r-800-1", Some(0)).await.is_some());
        assert!(hub.get_plot("r-800-1", Some(1)).await.is_some());
        assert!(hub.get_plot("r-800-1", Some(2)).await.is_none());

        // None returns latest (index 1).
        assert!(hub.get_plot("r-800-1", None).await.is_some());
    }

    #[tokio::test]
    async fn broadcast_frame_includes_plot_index() {
        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // First frame.
        hub.r_message(conn, Message::Frame(make_frame(Some("r-900-1"))));
        let msg = sub.recv().await.unwrap();
        match msg {
            Message::Frame(f) => assert_eq!(f.plot_index, Some(0)),
            other => panic!("expected Frame, got {other:?}"),
        }

        // New page.
        hub.r_message(
            conn,
            Message::Frame(make_frame_opts(Some("r-900-1"), Some(true), None)),
        );
        let msg = sub.recv().await.unwrap();
        match msg {
            Message::Frame(f) => assert_eq!(f.plot_index, Some(1)),
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn incremental_frame_appends_ops() {
        use jgd_protocol::DrawingOp;

        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // First (complete) frame with one op.
        let mut f1 = make_frame(Some("r-inc-1"));
        f1.plot.ops.push(DrawingOp::Clip {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        });
        hub.r_message(conn, Message::Frame(f1));
        let _ = sub.recv().await.unwrap();

        // Second (incremental) frame with a different op.
        let mut f2 = make_frame(Some("r-inc-1"));
        f2.incremental = true;
        f2.plot.ops.push(DrawingOp::Clip {
            x0: 10.0,
            y0: 10.0,
            x1: 90.0,
            y1: 90.0,
        });
        hub.r_message(conn, Message::Frame(f2));
        let _ = sub.recv().await.unwrap();

        // The stored plot should have both ops accumulated.
        let plot = hub.get_plot("r-inc-1", Some(0)).await.unwrap();
        assert_eq!(plot.ops.len(), 2, "incremental frame should append ops");

        // Still only one plot entry (not two pages).
        let plots = hub.get_plots().await;
        let count = plots.iter().filter(|p| p.session_id == "r-inc-1").count();
        assert_eq!(count, 1, "incremental frame should not create new page");
    }

    #[tokio::test]
    async fn non_incremental_frame_replaces_ops() {
        use jgd_protocol::DrawingOp;

        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();

        // First frame with two ops.
        let mut f1 = make_frame(Some("r-rep-1"));
        f1.plot.ops.push(DrawingOp::Clip {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        });
        f1.plot.ops.push(DrawingOp::Clip {
            x0: 5.0,
            y0: 5.0,
            x1: 95.0,
            y1: 95.0,
        });
        hub.r_message(conn, Message::Frame(f1));
        let _ = sub.recv().await.unwrap();

        // Second frame (non-incremental) with one op — should replace.
        let mut f2 = make_frame(Some("r-rep-1"));
        f2.plot.ops.push(DrawingOp::Clip {
            x0: 10.0,
            y0: 10.0,
            x1: 90.0,
            y1: 90.0,
        });
        hub.r_message(conn, Message::Frame(f2));
        let _ = sub.recv().await.unwrap();

        let plot = hub.get_plot("r-rep-1", Some(0)).await.unwrap();
        assert_eq!(
            plot.ops.len(),
            1,
            "non-incremental frame should replace ops"
        );
    }
}
