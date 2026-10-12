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

use crate::history::{PlotHistory, PlotId, StoredPlot};
use jgd_protocol::message::{FrameMessage, Message, ResizeMessage};

/// Connection-scoped numeric identifier for an R session.
pub type ConnId = u64;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Summary of a stored plot, returned by the REST API.
#[derive(Debug, Clone, Serialize)]
pub struct PlotSummary {
    pub session_id: String,
    pub plot_index: usize,
    pub plot_id: PlotId,
    pub revision: u64,
    pub connected: bool,
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

    /// Get an immutable drawing snapshot by its stable server identity.
    pub async fn get_snapshot(&self, plot_id: PlotId) -> Option<StoredPlot> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(HubCommand::GetSnapshot { plot_id, reply: tx });
        rx.await.ok().flatten()
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
    GetSnapshot {
        plot_id: PlotId,
        reply: oneshot::Sender<Option<StoredPlot>>,
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
    history: PlotHistory,
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
        history: PlotHistory::default(),
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
                let mut summaries: Vec<_> = state
                    .history
                    .sessions
                    .iter()
                    .flat_map(|(sid, plots)| {
                        let connected = state.session_id_to_conn.contains_key(sid);
                        plots
                            .iter()
                            .enumerate()
                            .map(move |(idx, stored)| PlotSummary {
                                session_id: sid.clone(),
                                plot_index: idx,
                                plot_id: stored.id,
                                revision: stored.revision,
                                connected,
                                width: stored.plot.device.width,
                                height: stored.plot.device.height,
                                op_count: stored.plot.ops.len(),
                            })
                    })
                    .collect();
                summaries.sort_by_key(|plot| plot.plot_id);
                let _ = reply.send(summaries);
            }
            HubCommand::GetSnapshot { plot_id, reply } => {
                let result = state
                    .history
                    .sessions
                    .values()
                    .flatten()
                    .find(|stored| stored.id == plot_id)
                    .cloned();
                let _ = reply.send(result);
            }
            HubCommand::GetPlot {
                session_id,
                plot_index,
                reply,
            } => {
                let result = state
                    .history
                    .get(&session_id, plot_index)
                    .map(|stored| stored.plot.clone());
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
            self.history.disconnect(id.clone());
            self.retired_session_ids.insert(id);

            // Keep identity tombstones for this process lifetime: even after a
            // drawing is evicted, a stale client target must never route to a
            // different R connection.
        }
        // Legacy notification has no session ID; clients resynchronize summaries.
        let _ = self.broadcast_tx.send(Message::Close);
    }

    /// Assign a session ID to a connection, handling reuse detection.
    ///
    /// R session IDs are opaque strings and can be reused when a process
    /// reopens a device. The Hub detects this and appends
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
            || self.history.sessions.contains_key(&raw_id)
        {
            loop {
                self.session_reuse_counter += 1;
                let candidate = format!("{}:{}", raw_id, self.session_reuse_counter);
                if !self.session_id_to_conn.contains_key(&candidate)
                    && !self.history.sessions.contains_key(&candidate)
                    && !self.retired_session_ids.contains(&candidate)
                {
                    break candidate;
                }
            }
        } else {
            raw_id
        };

        tracing::debug!(conn_id, %final_id, "session ID assigned");
        session.session_id = Some(final_id.clone());
        self.session_id_to_conn.insert(final_id, conn_id);
    }

    /// Route a message from an R session.
    fn handle_r_message(&mut self, conn_id: ConnId, msg: Message) {
        if !self.sessions.contains_key(&conn_id) {
            return;
        }
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
                self.unregister_session(conn_id);
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
        {
            frame.plot.session_id = Some(session_id.clone());
        }

        // Only a registered connection's assigned identity may own retained data.
        let Some(sid) = frame.plot.session_id.as_deref() else {
            return;
        };
        let Some(plot_idx) = self.history.apply(sid, &frame) else {
            tracing::warn!(
                sid,
                "ignoring frame with unknown or conflicting plot identity"
            );
            return;
        };
        // The renderer-facing v1 adapter keeps its local array index.
        frame.plot_index = Some(plot_idx as u32);

        let _ = self.broadcast_tx.send(Message::Frame(frame));
    }

    /// Route a resize from a browser client to R session(s).
    fn handle_client_resize(&mut self, msg: ResizeMessage) {
        if !msg.width.is_finite()
            || !msg.height.is_finite()
            || msg.width <= 0.0
            || msg.height <= 0.0
            || msg.width > 10000.0
            || msg.height > 10000.0
        {
            return;
        }

        // plotIndex resize: route only to the owning session.
        if let Some(plot_index) = msg.plot_index {
            let Some(session_id) = &msg.session_id else {
                return; // No session to route to.
            };
            let Some(&conn_id) = self.session_id_to_conn.get(session_id) else {
                return; // Target session is dead.
            };
            let Some(replay_number) = self
                .history
                .get(session_id, Some(plot_index as usize))
                .and_then(|plot| plot.replay_number)
            else {
                return;
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
                plot_index: Some(replay_number),
                session_id: None, // R doesn't need the session ID.
            }));
            return;
        }

        // Normal resize: broadcast to all sessions with per-session dedup.
        for session in self.sessions.values_mut() {
            if let Some(target) = &msg.session_id
                && session.session_id.as_ref() != Some(target)
            {
                continue;
            }
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
        assert_eq!(sub.recv().await.unwrap(), Message::Close);

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
        let mut frame = make_frame(Some("r-400-1"));
        frame.plot_number = Some(17);
        hub.r_message(conn, Message::Frame(frame));
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
        let mut frame = make_frame(Some("r-500-1"));
        frame.plot_number = Some(17);
        hub.r_message(conn1, Message::Frame(frame));
        let _ = sub.recv().await.unwrap();

        // plotIndex resize targeting conn1's session.
        hub.client_resize(ResizeMessage {
            width: 640.0,
            height: 480.0,
            plot_index: Some(0),
            session_id: Some("r-500-1".into()),
        });

        // conn1 should receive the resize.
        let msg = rx1.recv().await.unwrap();
        match msg {
            Message::Resize(r) => {
                assert_eq!(r.plot_index, Some(17));
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

    #[tokio::test]
    async fn close_and_eof_retain_history_and_disable_routing() {
        for graceful in [false, true] {
            let hub = spawn();
            let (conn, mut rx) = hub.register_session();
            let mut frame = make_frame(Some("offline"));
            frame.plot_number = Some(12);
            hub.r_message(conn, Message::Frame(frame));
            let before = hub.get_plots().await;
            assert!(before[0].connected);
            if graceful {
                hub.r_message(conn, Message::Close);
            } else {
                hub.unregister_session(conn);
            }
            hub.unregister_session(conn); // Idempotent after Close or EOF.
            let after = hub.get_plots().await;
            assert_eq!(after.len(), 1);
            assert!(!after[0].connected);
            assert_eq!(after[0].plot_id, before[0].plot_id);
            assert_eq!(after[0].revision, before[0].revision);
            assert!(hub.get_plot("offline", Some(0)).await.is_some());
            let snapshot = hub.get_snapshot(before[0].plot_id).await.unwrap();
            assert_eq!(snapshot.id, before[0].plot_id);
            assert_eq!(snapshot.revision, before[0].revision);
            hub.client_resize(ResizeMessage {
                width: 640.0,
                height: 480.0,
                plot_index: Some(0),
                session_id: Some("offline".into()),
            });
            hub.r_message(conn, Message::Frame(make_frame(Some("offline"))));
            let unchanged = hub.get_plots().await;
            assert_eq!(unchanged[0].revision, before[0].revision);
            assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected));
        }
    }

    #[tokio::test]
    async fn assigned_identity_cannot_be_changed_by_later_frames() {
        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        hub.r_message(conn, Message::Frame(make_frame(Some("owner"))));
        hub.r_message(conn, Message::Frame(make_frame(Some("other"))));
        let plots = hub.get_plots().await;
        assert_eq!(plots.len(), 1);
        assert_eq!(plots[0].session_id, "owner");
        assert_eq!(plots[0].revision, 2);
        assert!(hub.get_plot("other", None).await.is_none());
    }

    #[tokio::test]
    async fn targeted_normal_resize_does_not_broadcast_or_fall_back() {
        let hub = spawn();
        let (one, mut rx_one) = hub.register_session();
        let (two, mut rx_two) = hub.register_session();
        hub.r_message(one, Message::Frame(make_frame(Some("one"))));
        hub.r_message(two, Message::Frame(make_frame(Some("two"))));
        hub.client_resize(ResizeMessage {
            width: 640.0,
            height: 480.0,
            plot_index: None,
            session_id: Some("one".into()),
        });
        let _ = hub.get_plots().await;
        assert!(matches!(rx_one.try_recv(), Ok(Message::Resize(_))));
        assert_eq!(rx_two.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        hub.client_resize(ResizeMessage {
            width: 700.0,
            height: 500.0,
            plot_index: None,
            session_id: Some("missing".into()),
        });
        let _ = hub.get_plots().await;
        assert_eq!(rx_one.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        assert_eq!(rx_two.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn targeted_resize_routes_zero_replay_number() {
        let hub = spawn();
        let (conn, mut rx) = hub.register_session();
        hub.r_message(conn, Message::Frame(make_frame(Some("s"))));
        let mut frame = make_frame_opts(Some("s"), Some(true), None);
        frame.plot_number = Some(0);
        hub.r_message(conn, Message::Frame(frame));
        hub.client_resize(ResizeMessage {
            width: 640.0,
            height: 480.0,
            plot_index: Some(1),
            session_id: Some("s".into()),
        });
        let _ = hub.get_plots().await;
        let Message::Resize(resize) = rx.try_recv().unwrap() else {
            panic!("expected targeted resize");
        };
        assert_eq!(resize.plot_index, Some(0));
        assert_eq!(resize.session_id, None);
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn invalid_resize_and_unknown_history_targets_are_ignored() {
        let hub = spawn();
        let (conn, mut rx) = hub.register_session();
        hub.r_message(conn, Message::Frame(make_frame(Some("s"))));
        for (width, height) in [(0.0, 100.0), (100.0, f64::NAN), (10001.0, 100.0)] {
            hub.client_resize(ResizeMessage {
                width,
                height,
                plot_index: None,
                session_id: None,
            });
        }
        // Neither a missing replay number nor an out-of-range local index is safe.
        for index in [0, 99] {
            hub.client_resize(ResizeMessage {
                width: 640.0,
                height: 480.0,
                plot_index: Some(index),
                session_id: Some("s".into()),
            });
        }
        let _ = hub.get_plots().await;
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn reused_identity_cannot_route_old_history_to_new_connection() {
        let hub = spawn();
        let (old, _old_rx) = hub.register_session();
        let mut frame = make_frame(Some("same"));
        frame.plot_number = Some(0);
        hub.r_message(old, Message::Frame(frame.clone()));
        hub.unregister_session(old);
        let (new, mut new_rx) = hub.register_session();
        hub.r_message(new, Message::Frame(frame));
        let plots = hub.get_plots().await;
        assert_eq!(plots.len(), 2);
        assert_eq!(plots[0].session_id, "same");
        assert!(!plots[0].connected);
        assert!(plots[1].connected);
        assert_ne!(plots[0].session_id, plots[1].session_id);
        assert!(plots[0].plot_id < plots[1].plot_id);
        hub.client_resize(ResizeMessage {
            width: 640.0,
            height: 480.0,
            plot_index: Some(0),
            session_id: Some("same".into()),
        });
        let _ = hub.get_plots().await;
        assert_eq!(new_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn historical_replay_broadcast_uses_local_index_and_keeps_current_plot() {
        let hub = spawn();
        let (conn, _rx) = hub.register_session();
        let mut sub = hub.subscribe();
        for number in [17, 23] {
            let mut frame = make_frame_opts(Some("s"), Some(true), None);
            frame.plot_number = Some(number);
            hub.r_message(conn, Message::Frame(frame));
            let _ = sub.recv().await.unwrap();
        }
        let before = hub.get_plots().await;
        let mut replay = make_frame_opts(Some("s"), None, Some(17));
        replay.resize_replay = Some(true);
        replay.plot.device.width = 400.0;
        hub.r_message(conn, Message::Frame(replay));
        let Message::Frame(frame) = sub.recv().await.unwrap() else {
            panic!("expected replay frame");
        };
        assert_eq!(frame.plot_index, Some(0));
        let after = hub.get_plots().await;
        assert_eq!(after[0].plot_id, before[0].plot_id);
        assert_eq!(after[0].revision, before[0].revision + 1);
        assert_eq!(after[0].width, 400.0);
        assert_eq!(after[1].revision, before[1].revision);
        assert_eq!(after[1].width, 800.0);
        assert_eq!(hub.get_plot("s", None).await.unwrap().device.width, 800.0);
    }

    #[tokio::test]
    async fn evicted_history_does_not_allow_old_session_alias_reuse() {
        let hub = spawn();
        for index in 0..1002 {
            let (conn, _rx) = hub.register_session();
            let sid = format!("session-{index}");
            hub.r_message(conn, Message::Frame(make_frame(Some(&sid))));
            hub.unregister_session(conn);
        }
        let plots = hub.get_plots().await;
        assert_eq!(plots.len(), crate::history::MAX_OFFLINE_PLOTS);
        assert!(hub.get_plot("session-0", None).await.is_none());
        let (conn, _rx) = hub.register_session();
        hub.r_message(conn, Message::Frame(make_frame(Some("session-0"))));
        let plots = hub.get_plots().await;
        let live = plots.iter().find(|plot| plot.connected).unwrap();
        assert_ne!(live.session_id, "session-0");
        assert!(live.session_id.starts_with("session-0:"));
    }

    #[tokio::test]
    async fn generated_alias_does_not_collide_with_an_opaque_raw_id() {
        let hub = spawn();
        for sid in ["s", "s:1", "s"] {
            let (conn, _rx) = hub.register_session();
            hub.r_message(conn, Message::Frame(make_frame(Some(sid))));
        }
        let plots = hub.get_plots().await;
        assert_eq!(plots.len(), 3);
        let names: HashSet<_> = plots.iter().map(|plot| &plot.session_id).collect();
        assert_eq!(names.len(), 3);
    }
}
