//! Graphics context types.

use serde::{Deserialize, Serialize};

/// Graphics context controlling style for drawing operations.
///
/// Mirrors the R graphics context (pGEcontext) serialized by the C device.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphicsContext {
    /// Stroke color as `"rgba(R,G,B,A)"` or `null` if transparent.
    #[serde(default)]
    pub col: Option<String>,

    /// Fill color as `"rgba(R,G,B,A)"` or `null` if transparent.
    #[serde(default)]
    pub fill: Option<String>,

    /// Line width.
    #[serde(default = "default_lwd")]
    pub lwd: f64,

    /// Line dash pattern. Empty = solid line.
    #[serde(default)]
    pub lty: Vec<f64>,

    /// Line end cap style.
    #[serde(default = "default_lend")]
    pub lend: LineCap,

    /// Line join style.
    #[serde(default = "default_ljoin")]
    pub ljoin: LineJoin,

    /// Miter limit.
    #[serde(default = "default_lmitre")]
    pub lmitre: f64,

    /// Font properties.
    #[serde(default)]
    pub font: FontContext,

    /// Extension fields (experimental, user-provided).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<serde_json::Value>,
}

fn default_lwd() -> f64 {
    1.0
}

fn default_lmitre() -> f64 {
    10.0
}

fn default_lend() -> LineCap {
    LineCap::Round
}

fn default_ljoin() -> LineJoin {
    LineJoin::Round
}

/// Line end cap style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineCap {
    #[default]
    Round,
    Butt,
    Square,
}

/// Line join style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineJoin {
    #[default]
    Round,
    Miter,
    Bevel,
}

/// Font properties within a graphics context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FontContext {
    /// Font family name. Empty string = default.
    #[serde(default)]
    pub family: String,

    /// Font face: 1=plain, 2=bold, 3=italic, 4=bold-italic, 5=symbol.
    #[serde(default = "default_face")]
    pub face: u8,

    /// Font size in points (cex * ps).
    #[serde(default = "default_font_size")]
    pub size: f64,

    /// Line height multiplier.
    #[serde(default = "default_lineheight")]
    pub lineheight: f64,
}

impl Default for FontContext {
    fn default() -> Self {
        Self {
            family: String::new(),
            face: default_face(),
            size: default_font_size(),
            lineheight: default_lineheight(),
        }
    }
}

fn default_face() -> u8 {
    1
}

fn default_font_size() -> f64 {
    12.0
}

fn default_lineheight() -> f64 {
    1.0
}
