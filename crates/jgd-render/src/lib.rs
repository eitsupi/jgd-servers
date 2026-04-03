//! Renderer trait and backends (SVG, tiny-skia PNG) for jgd.

pub mod color;
pub mod raster;
pub mod svg;

use jgd_protocol::Plot;

pub use raster::RasterRenderer;
pub use svg::SvgRenderer;

/// Render a [`Plot`] into a concrete output format.
pub trait Renderer {
    /// The rendered output (e.g. `String` for SVG, `Vec<u8>` for PNG).
    type Output;
    /// Backend-specific error type.
    type Error: std::error::Error;

    /// Render the plot.
    fn render(&self, plot: &Plot) -> Result<Self::Output, Self::Error>;
}
