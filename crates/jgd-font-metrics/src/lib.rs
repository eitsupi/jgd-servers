//! Server-side font metrics via parley for jgd.
//!
//! Provides `strWidth` (text string width) and `metricInfo` (character
//! ascent/descent/width) computation using the parley text layout engine,
//! eliminating the browser round-trip required by the Deno reference server.

use std::sync::{LazyLock, Mutex};

use jgd_protocol::gc::FontContext as ProtocolFontContext;
use jgd_protocol::message::{MetricsKind, MetricsRequest, MetricsResponse};
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, GenericFamily, LayoutContext, LineHeight,
    StyleProperty,
};

/// Global font context shared across all metrics computations.
///
/// `FontContext` caches font discovery and parsed font data, so sharing
/// a single instance avoids redundant filesystem scans.
static FONT_CTX: LazyLock<Mutex<FontContext>> =
    LazyLock::new(|| Mutex::new(FontContext::new()));

/// Compute font metrics for a [`MetricsRequest`] and return a [`MetricsResponse`].
pub fn compute_metrics(req: &MetricsRequest) -> MetricsResponse {
    match req.kind {
        MetricsKind::StrWidth => {
            let text = req.str.as_deref().unwrap_or("");
            let width = str_width(text, &req.gc.font);
            MetricsResponse {
                id: req.id,
                width,
                ascent: 0.0,
                descent: 0.0,
            }
        }
        MetricsKind::MetricInfo => {
            let c = req
                .c
                .and_then(char::from_u32)
                .unwrap_or('\0');
            let metric = char_metric(c, &req.gc.font);
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

/// Compute the width of a text string in device units (pixels).
fn str_width(text: &str, font: &ProtocolFontContext) -> f64 {
    let layout = build_layout(text, font);
    layout.width() as f64
}

/// Compute ascent, descent, and width for a single character.
fn char_metric(c: char, font: &ProtocolFontContext) -> TextMetric {
    if c == '\0' {
        return TextMetric {
            ascent: 0.0,
            descent: 0.0,
            width: 0.0,
        };
    }

    let text = c.to_string();
    let layout = build_layout(&text, font);

    match layout.lines().next() {
        Some(line) => {
            let metrics = line.metrics();
            TextMetric {
                ascent: metrics.ascent as f64,
                descent: metrics.descent as f64,
                width: layout.width() as f64,
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
fn build_layout(text: &str, font: &ProtocolFontContext) -> parley::Layout<[u8; 4]> {
    let mut font_ctx = FONT_CTX.lock().unwrap();
    let mut layout_ctx: LayoutContext<[u8; 4]> = LayoutContext::new();
    let (weight, style) = fontface_to_weight_and_style(font.face);

    let family = map_font_family(&font.family);

    let mut builder = layout_ctx.ranged_builder(&mut font_ctx, text, 1.0, false);
    builder.push_default(StyleProperty::FontSize(font.size as f32));
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
    match family.to_lowercase().as_str() {
        "" | "sans" | "sans-serif" => GenericFamily::SansSerif.into(),
        "serif" => GenericFamily::Serif.into(),
        "mono" | "monospace" => GenericFamily::Monospace.into(),
        "symbol" => GenericFamily::Fantasy.into(),
        _ => FontFamily::named(family),
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

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::gc::FontContext as ProtocolFontContext;
    use jgd_protocol::message::{MetricsKind, MetricsRequest};
    use jgd_protocol::GraphicsContext;

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
    fn str_width_returns_positive_for_nonempty() {
        let req = MetricsRequest {
            id: 1,
            kind: MetricsKind::StrWidth,
            str: Some("Hello".into()),
            c: None,
            gc: make_gc(),
        };
        let resp = compute_metrics(&req);
        assert_eq!(resp.id, 1);
        assert!(resp.width > 0.0, "width should be positive, got {}", resp.width);
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
        let resp = compute_metrics(&req);
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
        let resp = compute_metrics(&req);
        assert_eq!(resp.id, 3);
        assert!(resp.ascent > 0.0, "ascent should be positive, got {}", resp.ascent);
        assert!(resp.width > 0.0, "width should be positive, got {}", resp.width);
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
        let resp = compute_metrics(&req);
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
        let w_plain = str_width(text, &gc_plain.font);
        let w_bold = str_width(text, &gc_bold.font);
        assert!(
            w_bold >= w_plain,
            "bold ({w_bold}) should be >= plain ({w_plain})"
        );
    }

    #[test]
    fn larger_font_wider() {
        let mut gc_small = make_gc();
        gc_small.font.size = 10.0;
        let mut gc_large = make_gc();
        gc_large.font.size = 20.0;

        let text = "Test";
        let w_small = str_width(text, &gc_small.font);
        let w_large = str_width(text, &gc_large.font);
        assert!(
            w_large > w_small,
            "20pt ({w_large}) should be wider than 10pt ({w_small})"
        );
    }
}
