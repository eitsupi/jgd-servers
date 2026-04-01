//! Drawing operation types.

use serde::{Deserialize, Serialize};

use crate::gc::GraphicsContext;

/// A single drawing operation within a plot frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum DrawingOp {
    /// Set clipping rectangle.
    Clip {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
    },

    /// Single line segment.
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        gc: GraphicsContext,
    },

    /// Connected line segments (open path).
    Polyline {
        x: Vec<f64>,
        y: Vec<f64>,
        gc: GraphicsContext,
    },

    /// Closed polygon.
    Polygon {
        x: Vec<f64>,
        y: Vec<f64>,
        gc: GraphicsContext,
    },

    /// Rectangle.
    Rect {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        gc: GraphicsContext,
    },

    /// Circle.
    Circle {
        x: f64,
        y: f64,
        r: f64,
        gc: GraphicsContext,
    },

    /// Text rendering.
    Text {
        x: f64,
        y: f64,
        str: String,
        rot: f64,
        hadj: f64,
        gc: GraphicsContext,
    },

    /// Compound path with fill rule.
    Path {
        winding: FillRule,
        subpaths: Vec<Vec<[f64; 2]>>,
        gc: GraphicsContext,
    },

    /// Raster image.
    Raster {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        rot: f64,
        interpolate: bool,
        pw: u32,
        ph: u32,
        data: String,
    },

    /// Start of a rendering group.
    BeginGroup {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ext: Option<serde_json::Value>,
    },

    /// End of a rendering group.
    EndGroup,
}

/// Path fill rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FillRule {
    Nonzero,
    Evenodd,
}
