//! Server-side font metrics via parley for jgd.
//!
//! Provides `strWidth` (text string width) and `metricInfo` (character
//! ascent/descent/width) computation using the parley text layout engine,
//! eliminating the browser round-trip required by the Deno reference server.
//!
//! Also provides [`text_to_paths`] for converting text to tiny-skia paths
//! for raster rendering.

use std::sync::{LazyLock, Mutex};

use jgd_protocol::gc::FontContext as ProtocolFontContext;
use jgd_protocol::message::{MetricsKind, MetricsRequest, MetricsResponse};
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, GenericFamily, LayoutContext, LineHeight,
    PositionedLayoutItem, StyleProperty,
};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::FontRef as ReadFontsRef;
use skrifa::{GlyphId, MetadataProvider};

/// Shared font and layout contexts for metrics computations.
///
/// `FontContext` caches font discovery and parsed font data.
/// `LayoutContext` caches shaping/layout data across calls.
/// Bundled together under a single mutex to preserve caching benefits.
static CTX: LazyLock<Mutex<(FontContext, LayoutContext<[u8; 4]>)>> =
    LazyLock::new(|| Mutex::new((FontContext::new(), LayoutContext::new())));

/// Default DPI matching the jgd R device default.
const DEFAULT_DPI: f64 = 96.0;

/// Compute font metrics for a [`MetricsRequest`] and return a [`MetricsResponse`].
///
/// `dpi` should match the R graphics device DPI (default 96).
/// R sends font size in points; metrics must be returned in device units
/// (pixels at the given DPI), so we scale by `dpi / 72`.
pub fn compute_metrics(req: &MetricsRequest, dpi: Option<f64>) -> MetricsResponse {
    let dpi = dpi.unwrap_or(DEFAULT_DPI);
    match req.kind {
        MetricsKind::StrWidth => {
            let text = req.str.as_deref().unwrap_or("");
            let width = str_width(text, &req.gc.font, dpi);
            MetricsResponse {
                id: req.id,
                width,
                ascent: 0.0,
                descent: 0.0,
            }
        }
        MetricsKind::MetricInfo => {
            let c = req.c.and_then(char::from_u32).unwrap_or('\0');
            let metric = char_metric(c, &req.gc.font, dpi);
            MetricsResponse {
                id: req.id,
                width: metric.width,
                ascent: metric.ascent,
                descent: metric.descent,
            }
        }
    }
}

/// Result of a single-character metric query.
struct TextMetric {
    ascent: f64,
    descent: f64,
    width: f64,
}

/// Compute the width of a text string in device units (pixels at given DPI).
///
/// Uses the sum of glyph advances rather than `layout.width()` because
/// parley trims trailing whitespace from reported width.
fn str_width(text: &str, font: &ProtocolFontContext, dpi: f64) -> f64 {
    let layout = build_layout(text, font, dpi);
    total_advance(&layout) as f64
}

/// Sum all glyph advances in a layout (not trimmed like `layout.width()`).
fn total_advance(layout: &parley::Layout<[u8; 4]>) -> f32 {
    let mut total = 0.0f32;
    for line in layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                for glyph in run.glyphs() {
                    total += glyph.advance;
                }
            }
        }
    }
    total
}

/// Compute ascent, descent, and width for a single character.
fn char_metric(c: char, font: &ProtocolFontContext, dpi: f64) -> TextMetric {
    if c == '\0' {
        return TextMetric {
            ascent: 0.0,
            descent: 0.0,
            width: 0.0,
        };
    }

    let text = c.to_string();
    let layout = build_layout(&text, font, dpi);

    match layout.lines().next() {
        Some(line) => {
            let metrics = line.metrics();
            TextMetric {
                ascent: metrics.ascent as f64,
                descent: metrics.descent as f64,
                width: total_advance(&layout) as f64,
            }
        }
        None => TextMetric {
            ascent: 0.0,
            descent: 0.0,
            width: 0.0,
        },
    }
}

/// Build a parley layout for the given text and font parameters.
///
/// `dpi` is the R device DPI.  R sends font size in points (1/72 inch);
/// parley needs the size in device pixels, so we scale by `dpi / 72`.
fn build_layout(text: &str, font: &ProtocolFontContext, dpi: f64) -> parley::Layout<[u8; 4]> {
    let mut ctx = CTX.lock().unwrap();
    let (ref mut font_ctx, ref mut layout_ctx) = *ctx;
    let (weight, style) = fontface_to_weight_and_style(font.face);

    let family = map_font_family(&font.family);

    // Convert font size from points to device pixels.
    let font_size_px = font.size as f32 * (dpi as f32 / 72.0);
    let mut builder = layout_ctx.ranged_builder(font_ctx, text, 1.0, false);
    builder.push_default(StyleProperty::FontSize(font_size_px));
    builder.push_default(LineHeight::FontSizeRelative(font.lineheight as f32));
    builder.push_default(family);
    builder.push_default(StyleProperty::FontWeight(weight));
    builder.push_default(StyleProperty::FontStyle(style));

    let mut layout = parley::Layout::new();
    builder.build_into(&mut layout, text);
    layout.break_all_lines(None);
    layout.align(None, Alignment::Start, AlignmentOptions::default());
    layout
}

/// Map R font family names to parley `FontFamily`.
fn map_font_family(family: &str) -> FontFamily<'_> {
    if family.is_empty()
        || family.eq_ignore_ascii_case("sans")
        || family.eq_ignore_ascii_case("sans-serif")
    {
        GenericFamily::SansSerif.into()
    } else if family.eq_ignore_ascii_case("serif") {
        GenericFamily::Serif.into()
    } else if family.eq_ignore_ascii_case("mono") || family.eq_ignore_ascii_case("monospace") {
        GenericFamily::Monospace.into()
    } else if family.eq_ignore_ascii_case("symbol") {
        GenericFamily::Fantasy.into()
    } else {
        FontFamily::named(family)
    }
}

/// Map R font face integer to parley weight and style.
///
/// | face | Meaning     | Weight | Style  |
/// |------|-------------|--------|--------|
/// | 1    | Plain       | NORMAL | Normal |
/// | 2    | Bold        | BOLD   | Normal |
/// | 3    | Italic      | NORMAL | Italic |
/// | 4    | Bold-Italic | BOLD   | Italic |
/// | 5    | Symbol      | NORMAL | Normal |
fn fontface_to_weight_and_style(face: u8) -> (parley::FontWeight, parley::FontStyle) {
    match face {
        1 => (parley::FontWeight::NORMAL, parley::FontStyle::Normal),
        2 => (parley::FontWeight::BOLD, parley::FontStyle::Normal),
        3 => (parley::FontWeight::NORMAL, parley::FontStyle::Italic),
        4 => (parley::FontWeight::BOLD, parley::FontStyle::Italic),
        _ => (parley::FontWeight::NORMAL, parley::FontStyle::Normal),
    }
}

// ---------------------------------------------------------------------------
// Text-to-path conversion for raster rendering
// ---------------------------------------------------------------------------

/// A positioned glyph with its outline converted to a tiny-skia path.
pub struct GlyphPath {
    /// Horizontal offset from the text origin.
    pub x: f32,
    /// Vertical offset (typically 0 for single-line).
    pub y: f32,
    /// Horizontal advance width.
    pub advance: f32,
    /// The glyph outline as a tiny-skia path, or `None` if the glyph has no
    /// outline (e.g. space character).
    pub path: Option<tiny_skia::Path>,
}

/// Convert text into positioned glyph outlines suitable for tiny-skia rendering.
///
/// Each glyph's `x`/`y` is relative to the text origin. The caller applies
/// the final positioning transform (including rotation and horizontal adjustment).
///
/// `dpi` is the R device DPI (default 96); font size is scaled by `dpi / 72`.
pub fn text_to_paths(text: &str, font: &ProtocolFontContext, dpi: f64) -> Vec<GlyphPath> {
    let layout = build_layout(text, font, dpi);
    let mut result = Vec::new();

    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let font_size = run.font_size();
            let font_data = run.font();
            let raw_coords = run.normalized_coords();

            // SAFETY NOTE: `ReadFontsRef` borrows `font_data.data`, so `font_ref`,
            // `outlines`, and each `outline` must not outlive `font_data` (owned by
            // `run`, which is alive for the duration of this loop iteration).
            let font_ref = match ReadFontsRef::from_index(font_data.data.as_ref(), font_data.index)
            {
                Ok(f) => f,
                Err(_) => continue,
            };
            let outlines = font_ref.outline_glyphs();
            // Convert i16 normalized coords to F2Dot14 (same bit representation).
            let coords: Vec<skrifa::instance::NormalizedCoord> = raw_coords
                .iter()
                .map(|&c| skrifa::instance::NormalizedCoord::from_bits(c))
                .collect();
            let run_offset = glyph_run.offset();
            let size = Size::new(font_size);

            let mut cursor_x = 0.0f32;
            for glyph in glyph_run.glyphs() {
                let glyph_id = GlyphId::from(glyph.id);
                // glyph.x is a per-glyph offset (kerning etc.), not cumulative.
                // The cumulative position comes from advancing the cursor.
                let gx = run_offset + cursor_x + glyph.x;
                let gy = glyph.y;

                let path = outlines.get(glyph_id).and_then(|outline| {
                    let location = LocationRef::new(&coords);
                    let settings = DrawSettings::unhinted(size, location);
                    let mut pen = TinySkiaPen::new();
                    outline.draw(settings, &mut pen).ok()?;
                    pen.finish()
                });

                result.push(GlyphPath {
                    x: gx,
                    y: gy,
                    advance: glyph.advance,
                    path,
                });

                cursor_x += glyph.advance;
            }
        }
    }
    result
}

/// Pen that converts glyph outline commands to a tiny-skia [`PathBuilder`].
struct TinySkiaPen {
    pb: tiny_skia::PathBuilder,
}

impl TinySkiaPen {
    fn new() -> Self {
        Self {
            pb: tiny_skia::PathBuilder::new(),
        }
    }

    fn finish(self) -> Option<tiny_skia::Path> {
        self.pb.finish()
    }
}

impl OutlinePen for TinySkiaPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(x, -y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(x, -y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.pb.quad_to(cx0, -cy0, x, -y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.pb.cubic_to(cx0, -cy0, cx1, -cy1, x, -y);
    }

    fn close(&mut self) {
        self.pb.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::GraphicsContext;
    use jgd_protocol::gc::FontContext as ProtocolFontContext;
    use jgd_protocol::message::{MetricsKind, MetricsRequest};

    fn make_gc() -> GraphicsContext {
        GraphicsContext {
            font: ProtocolFontContext {
                family: "sans".into(),
                face: 1,
                size: 12.0,
                lineheight: 1.0,
            },
            ..Default::default()
        }
    }

    #[test]
    fn space_char_has_positive_width() {
        let gc = make_gc();
        let m = char_metric(' ', &gc.font, 96.0);
        assert!(
            m.width > 0.0,
            "space width should be positive, got {}",
            m.width
        );
    }

    #[test]
    fn str_width_returns_positive_for_nonempty() {
        let req = MetricsRequest {
            id: 1,
            kind: MetricsKind::StrWidth,
            str: Some("Hello".into()),
            c: None,
            gc: make_gc(),
        };
        let resp = compute_metrics(&req, None);
        assert_eq!(resp.id, 1);
        assert!(
            resp.width > 0.0,
            "width should be positive, got {}",
            resp.width
        );
    }

    #[test]
    fn str_width_empty_is_zero() {
        let req = MetricsRequest {
            id: 2,
            kind: MetricsKind::StrWidth,
            str: Some(String::new()),
            c: None,
            gc: make_gc(),
        };
        let resp = compute_metrics(&req, None);
        assert_eq!(resp.width, 0.0);
    }

    #[test]
    fn metric_info_returns_positive_ascent() {
        let req = MetricsRequest {
            id: 3,
            kind: MetricsKind::MetricInfo,
            str: None,
            c: Some(b'M' as u32),
            gc: make_gc(),
        };
        let resp = compute_metrics(&req, None);
        assert_eq!(resp.id, 3);
        assert!(
            resp.ascent > 0.0,
            "ascent should be positive, got {}",
            resp.ascent
        );
        assert!(
            resp.width > 0.0,
            "width should be positive, got {}",
            resp.width
        );
    }

    #[test]
    fn metric_info_null_char_returns_zero() {
        let req = MetricsRequest {
            id: 4,
            kind: MetricsKind::MetricInfo,
            str: None,
            c: Some(0),
            gc: make_gc(),
        };
        let resp = compute_metrics(&req, None);
        assert_eq!(resp.width, 0.0);
        assert_eq!(resp.ascent, 0.0);
        assert_eq!(resp.descent, 0.0);
    }

    #[test]
    fn bold_text_wider_than_plain() {
        let mut gc_plain = make_gc();
        gc_plain.font.face = 1;
        let mut gc_bold = make_gc();
        gc_bold.font.face = 2;

        let text = "Hello, World!";
        let w_plain = str_width(text, &gc_plain.font, 96.0);
        let w_bold = str_width(text, &gc_bold.font, 96.0);
        assert!(
            w_bold >= w_plain,
            "bold ({w_bold}) should be >= plain ({w_plain})"
        );
    }

    #[test]
    fn text_to_paths_returns_glyphs() {
        let gc = make_gc();
        let paths = text_to_paths("Hello", &gc.font, 96.0);
        assert!(
            !paths.is_empty(),
            "should return glyph paths for non-empty text"
        );
        assert!(
            paths.iter().any(|g| g.path.is_some()),
            "at least one glyph should have an outline path"
        );
    }

    #[test]
    fn text_to_paths_empty_string() {
        let gc = make_gc();
        let paths = text_to_paths("", &gc.font, 96.0);
        assert!(paths.is_empty());
    }

    #[test]
    fn larger_font_wider() {
        let mut gc_small = make_gc();
        gc_small.font.size = 10.0;
        let mut gc_large = make_gc();
        gc_large.font.size = 20.0;

        let text = "Test";
        let w_small = str_width(text, &gc_small.font, 96.0);
        let w_large = str_width(text, &gc_large.font, 96.0);
        assert!(
            w_large > w_small,
            "20pt ({w_large}) should be wider than 10pt ({w_small})"
        );
    }
}
