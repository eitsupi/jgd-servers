//! Protocol message types for R ↔ Server ↔ Browser communication.

use serde::{Deserialize, Serialize};

use crate::gc::GraphicsContext;
use crate::ops::DrawingOp;

// ---------------------------------------------------------------------------
// Composite types used in messages
// ---------------------------------------------------------------------------

/// Device metadata within a plot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub width: f64,
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
}

/// Plot data within a frame message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub ops: Vec<DrawingOp>,
    pub device: DeviceInfo,
}

// ---------------------------------------------------------------------------
// R → Server messages
// ---------------------------------------------------------------------------

/// Frame message containing plot operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMessage {
    pub plot: Plot,
    #[serde(default)]
    pub incremental: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_page: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize_replay: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<serde_json::Value>,
}

/// Metrics request kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetricsKind {
    StrWidth,
    MetricInfo,
}

/// Font metrics request from R.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsRequest {
    pub id: u64,
    pub kind: MetricsKind,
    /// Text string (for strWidth).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub str: Option<String>,
    /// Unicode code point (for metricInfo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c: Option<u32>,
    /// Graphics context with font info.
    pub gc: GraphicsContext,
}

/// Font metrics response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsResponse {
    pub id: u64,
    pub width: f64,
    pub ascent: f64,
    pub descent: f64,
}

/// Resize message from browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizeMessage {
    pub width: f64,
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Welcome message sent to R on connect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfoMessage {
    pub server_name: String,
    pub protocol_version: u32,
    pub transport: Transport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Transport type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Unix,
    Tcp,
    Npipe,
}

// ---------------------------------------------------------------------------
// Envelope: tagged union over all protocol messages
// ---------------------------------------------------------------------------

/// Any protocol message, internally tagged by `"type"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Heartbeat / ordering probe.
    Ping,
    /// Heartbeat response.
    Pong,
    /// Drawing operations frame.
    Frame(FrameMessage),
    /// Font metrics request.
    MetricsRequest(MetricsRequest),
    /// Font metrics response.
    MetricsResponse(MetricsResponse),
    /// Viewport resize.
    Resize(ResizeMessage),
    /// Welcome message.
    ServerInfo(ServerInfoMessage),
    /// Device close.
    Close,
}
