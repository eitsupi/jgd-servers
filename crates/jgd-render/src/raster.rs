//! Raster (PNG) rendering backend using tiny-skia.

use tiny_skia::{
    Color, FillRule, LineCap as TsLineCap, LineJoin as TsLineJoin, Mask, Paint, PathBuilder,
    Pixmap, PixmapPaint, Rect, Stroke, StrokeDash, Transform,
};

use jgd_protocol::{DrawingOp, FillRule as OpFillRule, GraphicsContext, LineCap, LineJoin, Plot};

use crate::Renderer;
use crate::color::parse_rgba;

/// Renders a [`Plot`] as PNG bytes via tiny-skia.
#[derive(Debug, Clone, Default)]
pub struct RasterRenderer {
    /// Desired output width in pixels.  Falls back to device width.
    pub output_width: Option<u32>,
    /// Desired output height in pixels.  Falls back to device height.
    pub output_height: Option<u32>,
}

/// Error type for raster rendering.
#[derive(Debug, thiserror::Error)]
pub enum RasterError {
    #[error("failed to create pixmap ({0}x{1})")]
    PixmapCreation(u32, u32),
    #[error("PNG encoding failed")]
    PngEncode,
    #[error("text rendering: {0}")]
    Text(String),
}

impl Renderer for RasterRenderer {
    type Output = Vec<u8>;
    type Error = RasterError;

    fn render(&self, plot: &Plot) -> Result<Vec<u8>, RasterError> {
        let dev_w = plot.device.width;
        let dev_h = plot.device.height;
        let dpi = plot.device.dpi.unwrap_or(96.0);
        let out_w = self.output_width.unwrap_or(dev_w as u32);
        let out_h = self.output_height.unwrap_or(dev_h as u32);

        let mut pixmap =
            Pixmap::new(out_w, out_h).ok_or(RasterError::PixmapCreation(out_w, out_h))?;

        // Scale from device coords to output pixels.
        let sx = out_w as f32 / dev_w as f32;
        let sy = out_h as f32 / dev_h as f32;
        let base_transform = Transform::from_scale(sx, sy);

        // Background.
        if let Some(bg) = &plot.device.bg
            && let Some(color) = parse_rgba(bg)
        {
            pixmap.fill(color);
        }

        // Clip mask stack: each Clip op pushes a new mask, close pops.
        let mut clip_stack: Vec<Mask> = Vec::new();
        // For group save/restore: save the clip stack depth.
        let mut group_clip_depths: Vec<usize> = Vec::new();

        for op in &plot.ops {
            match op {
                DrawingOp::Clip { x0, y0, x1, y1 } => {
                    // Close the innermost clip (same as SVG backend's
                    // close_innermost_clip): pop one clip level, but not
                    // below the enclosing group boundary.
                    let depth = group_clip_depths.last().copied().unwrap_or(0);
                    if clip_stack.len() > depth {
                        clip_stack.pop();
                    }

                    let rx = x0.min(*x1) as f32;
                    let ry = y0.min(*y1) as f32;
                    let rw = (x1 - x0).abs() as f32;
                    let rh = (y1 - y0).abs() as f32;

                    let mut mask =
                        Mask::new(out_w, out_h).ok_or(RasterError::PixmapCreation(out_w, out_h))?;

                    if let Some(rect_path) = {
                        let mut pb = PathBuilder::new();
                        if let Some(r) = Rect::from_xywh(rx, ry, rw, rh) {
                            pb.push_rect(r);
                        }
                        pb.finish()
                    } {
                        mask.fill_path(&rect_path, FillRule::Winding, true, base_transform);
                    }

                    // Intersect with parent mask if any.
                    if let Some(parent) = clip_stack.last() {
                        intersect_masks(&mut mask, parent);
                    }

                    clip_stack.push(mask);
                }

                DrawingOp::Line { x1, y1, x2, y2, gc } => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(*x1 as f32, *y1 as f32);
                    pb.line_to(*x2 as f32, *y2 as f32);
                    if let Some(path) = pb.finish()
                        && let Some((paint, stroke)) = stroke_from_gc(gc)
                    {
                        pixmap.stroke_path(
                            &path,
                            &paint,
                            &stroke,
                            base_transform,
                            clip_stack.last(),
                        );
                    }
                }

                DrawingOp::Polyline { x, y, gc } => {
                    if let Some(path) = build_polypath(x, y, false)
                        && let Some((paint, stroke)) = stroke_from_gc(gc)
                    {
                        pixmap.stroke_path(
                            &path,
                            &paint,
                            &stroke,
                            base_transform,
                            clip_stack.last(),
                        );
                    }
                }

                DrawingOp::Polygon { x, y, gc } => {
                    if let Some(path) = build_polypath(x, y, true) {
                        fill_and_stroke(
                            &mut pixmap,
                            &path,
                            gc,
                            FillRule::Winding,
                            base_transform,
                            clip_stack.last(),
                        );
                    }
                }

                DrawingOp::Rect { x0, y0, x1, y1, gc } => {
                    let rx = x0.min(*x1) as f32;
                    let ry = y0.min(*y1) as f32;
                    let rw = (x1 - x0).abs() as f32;
                    let rh = (y1 - y0).abs() as f32;

                    // Fill.
                    if let Some(rect) = Rect::from_xywh(rx, ry, rw, rh)
                        && let Some(paint) = fill_paint(gc)
                    {
                        pixmap.fill_rect(rect, &paint, base_transform, clip_stack.last());
                    }
                    // Stroke.
                    if let Some(rect) = Rect::from_xywh(rx, ry, rw, rh) {
                        let mut pb = PathBuilder::new();
                        pb.push_rect(rect);
                        if let Some(path) = pb.finish()
                            && let Some((paint, stroke)) = stroke_from_gc(gc)
                        {
                            pixmap.stroke_path(
                                &path,
                                &paint,
                                &stroke,
                                base_transform,
                                clip_stack.last(),
                            );
                        }
                    }
                }

                DrawingOp::Circle { x, y, r, gc } => {
                    let mut pb = PathBuilder::new();
                    pb.push_circle(*x as f32, *y as f32, *r as f32);
                    if let Some(path) = pb.finish() {
                        fill_and_stroke(
                            &mut pixmap,
                            &path,
                            gc,
                            FillRule::Winding,
                            base_transform,
                            clip_stack.last(),
                        );
                    }
                }

                DrawingOp::Text {
                    x,
                    y,
                    r#str: text,
                    rot,
                    hadj,
                    gc,
                } => {
                    render_text(
                        &mut pixmap,
                        text,
                        *x as f32,
                        *y as f32,
                        *rot as f32,
                        *hadj as f32,
                        gc,
                        dpi,
                        base_transform,
                        clip_stack.last(),
                    );
                }

                DrawingOp::Path {
                    winding,
                    subpaths,
                    gc,
                } => {
                    let mut pb = PathBuilder::new();
                    for subpath in subpaths {
                        for (i, pt) in subpath.iter().enumerate() {
                            if i == 0 {
                                pb.move_to(pt[0] as f32, pt[1] as f32);
                            } else {
                                pb.line_to(pt[0] as f32, pt[1] as f32);
                            }
                        }
                        pb.close();
                    }
                    if let Some(path) = pb.finish() {
                        let rule = match winding {
                            OpFillRule::Nonzero => FillRule::Winding,
                            OpFillRule::Evenodd => FillRule::EvenOdd,
                        };
                        fill_and_stroke(
                            &mut pixmap,
                            &path,
                            gc,
                            rule,
                            base_transform,
                            clip_stack.last(),
                        );
                    }
                }

                DrawingOp::Raster {
                    x,
                    y,
                    w,
                    h,
                    rot,
                    pw,
                    ph,
                    data,
                    ..
                } => {
                    render_raster_image(
                        &mut pixmap,
                        data,
                        *pw,
                        *ph,
                        *x as f32,
                        *y as f32,
                        *w as f32,
                        *h as f32,
                        *rot as f32,
                        base_transform,
                        clip_stack.last(),
                    );
                }

                DrawingOp::BeginGroup { .. } => {
                    group_clip_depths.push(clip_stack.len());
                }
                DrawingOp::EndGroup => {
                    if let Some(depth) = group_clip_depths.pop() {
                        clip_stack.truncate(depth);
                    }
                }
            }
        }

        pixmap.encode_png().map_err(|_| RasterError::PngEncode)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_polypath(x: &[f64], y: &[f64], close: bool) -> Option<tiny_skia::Path> {
    if x.is_empty() {
        return None;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(x[0] as f32, y[0] as f32);
    for (px, py) in x.iter().zip(y.iter()).skip(1) {
        pb.line_to(*px as f32, *py as f32);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

fn fill_paint(gc: &GraphicsContext) -> Option<Paint<'static>> {
    let color = parse_rgba(gc.fill.as_deref()?)?;
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    Some(paint)
}

fn stroke_from_gc(gc: &GraphicsContext) -> Option<(Paint<'static>, Stroke)> {
    let color = parse_rgba(gc.col.as_deref()?)?;
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let mut stroke = Stroke {
        width: gc.lwd as f32,
        line_cap: match gc.lend {
            LineCap::Round => TsLineCap::Round,
            LineCap::Butt => TsLineCap::Butt,
            LineCap::Square => TsLineCap::Square,
        },
        line_join: match gc.ljoin {
            LineJoin::Round => TsLineJoin::Round,
            LineJoin::Miter => TsLineJoin::Miter,
            LineJoin::Bevel => TsLineJoin::Bevel,
        },
        miter_limit: gc.lmitre as f32,
        ..Stroke::default()
    };

    if !gc.lty.is_empty() {
        let intervals: Vec<f32> = gc.lty.iter().map(|v| *v as f32).collect();
        stroke.dash = StrokeDash::new(intervals, 0.0);
    }

    Some((paint, stroke))
}

fn fill_and_stroke(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    gc: &GraphicsContext,
    fill_rule: FillRule,
    transform: Transform,
    mask: Option<&Mask>,
) {
    if let Some(paint) = fill_paint(gc) {
        pixmap.fill_path(path, &paint, fill_rule, transform, mask);
    }
    if let Some((paint, stroke)) = stroke_from_gc(gc) {
        pixmap.stroke_path(path, &paint, &stroke, transform, mask);
    }
}

fn intersect_masks(target: &mut Mask, parent: &Mask) {
    let target_data = target.data_mut();
    let parent_data = parent.data();
    for (t, p) in target_data.iter_mut().zip(parent_data.iter()) {
        *t = ((*t as u16 * *p as u16) / 255) as u8;
    }
}

// ---------------------------------------------------------------------------
// Text rendering via parley + skrifa
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_text(
    pixmap: &mut Pixmap,
    text: &str,
    x: f32,
    y: f32,
    rot: f32,
    hadj: f32,
    gc: &GraphicsContext,
    dpi: f64,
    base_transform: Transform,
    mask: Option<&Mask>,
) {
    use jgd_font_metrics::text_to_paths;

    let color = gc
        .col
        .as_deref()
        .and_then(parse_rgba)
        .unwrap_or(Color::BLACK);

    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let glyphs = text_to_paths(text, &gc.font, dpi);

    // Compute total text width for horizontal adjustment.
    let total_width: f32 = glyphs.iter().map(|g| g.advance).sum();
    let x_offset = -total_width * hadj;

    // Build transform: base scaling, then position, then rotation.
    let mut transform = base_transform;
    if rot.abs() > 1e-6 {
        // R uses counter-clockwise degrees; tiny-skia uses clockwise.
        transform = transform.pre_concat(Transform::from_rotate_at(-rot, x, y));
    }

    for glyph in &glyphs {
        if let Some(path) = &glyph.path {
            let glyph_transform =
                transform.pre_concat(Transform::from_translate(x + x_offset + glyph.x, y));
            pixmap.fill_path(path, &paint, FillRule::Winding, glyph_transform, mask);
        }
    }
}

// ---------------------------------------------------------------------------
// Raster image rendering
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_raster_image(
    pixmap: &mut Pixmap,
    data: &str,
    _pw: u32,
    _ph: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rot: f32,
    base_transform: Transform,
    mask: Option<&Mask>,
) {
    use std::io::Read;
    // Decode base64 → PNG → Pixmap.
    let decoded = {
        let mut decoder = base64_decode_reader(data.as_bytes());
        let mut buf = Vec::new();
        if decoder.read_to_end(&mut buf).is_err() {
            return;
        }
        buf
    };
    let Some(src) = Pixmap::decode_png(&decoded).ok() else {
        return;
    };

    let sx = w / src.width() as f32;
    let sy = h / src.height() as f32;

    let mut transform = base_transform;
    if rot.abs() > 1e-6 {
        transform = transform.pre_concat(Transform::from_rotate_at(-rot, x, y));
    }
    transform = transform.pre_concat(Transform::from_translate(x, y));
    transform = transform.pre_concat(Transform::from_scale(sx, sy));

    pixmap.draw_pixmap(0, 0, src.as_ref(), &PixmapPaint::default(), transform, mask);
}

/// Simple base64 decoder (no external dep needed; R sends standard base64).
fn base64_decode_reader(input: &[u8]) -> Base64Reader<'_> {
    Base64Reader {
        input,
        pos: 0,
        buf: [0; 3],
        buf_len: 0,
        buf_pos: 0,
    }
}

struct Base64Reader<'a> {
    input: &'a [u8],
    pos: usize,
    buf: [u8; 3],
    buf_len: usize,
    buf_pos: usize,
}

impl std::io::Read for Base64Reader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < out.len() {
            if self.buf_pos < self.buf_len {
                out[written] = self.buf[self.buf_pos];
                self.buf_pos += 1;
                written += 1;
                continue;
            }
            // Decode next 4 base64 chars.
            let mut quad = [0u8; 4];
            let mut qi = 0;
            while qi < 4 {
                if self.pos >= self.input.len() {
                    return Ok(written);
                }
                let ch = self.input[self.pos];
                self.pos += 1;
                if ch == b'\n' || ch == b'\r' || ch == b' ' {
                    continue;
                }
                quad[qi] = ch;
                qi += 1;
            }
            let vals: [u8; 4] = quad.map(b64_val);
            self.buf[0] = (vals[0] << 2) | (vals[1] >> 4);
            self.buf[1] = (vals[1] << 4) | (vals[2] >> 2);
            self.buf[2] = (vals[2] << 6) | vals[3];
            self.buf_len = if quad[3] == b'=' {
                if quad[2] == b'=' { 1 } else { 2 }
            } else {
                3
            };
            self.buf_pos = 0;
        }
        Ok(written)
    }
}

fn b64_val(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::message::DeviceInfo;

    fn device(w: f64, h: f64) -> DeviceInfo {
        DeviceInfo {
            width: w,
            height: h,
            dpi: Some(96.0),
            bg: None,
        }
    }

    fn simple_gc() -> GraphicsContext {
        GraphicsContext {
            col: Some("rgba(0,0,0,1)".into()),
            fill: Some("rgba(255,0,0,1)".into()),
            lwd: 2.0,
            ..Default::default()
        }
    }

    #[test]
    fn empty_plot_produces_valid_png() {
        let plot = Plot {
            session_id: None,
            ops: vec![],
            device: device(100.0, 80.0),
        };
        let png = RasterRenderer::default().render(&plot).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn background_fill() {
        let plot = Plot {
            session_id: None,
            ops: vec![],
            device: DeviceInfo {
                width: 10.0,
                height: 10.0,
                dpi: None,
                bg: Some("rgba(255,255,255,1)".into()),
            },
        };
        let png = RasterRenderer::default().render(&plot).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        // Verify it's not empty (has some pixel data beyond header).
        assert!(png.len() > 50);
    }

    #[test]
    fn line_op_renders() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 50.0,
                y2: 50.0,
                gc: simple_gc(),
            }],
            device: device(100.0, 100.0),
        };
        let png = RasterRenderer::default().render(&plot).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn rect_and_circle_render() {
        let gc = simple_gc();
        let plot = Plot {
            session_id: None,
            ops: vec![
                DrawingOp::Rect {
                    x0: 10.0,
                    y0: 10.0,
                    x1: 50.0,
                    y1: 50.0,
                    gc: gc.clone(),
                },
                DrawingOp::Circle {
                    x: 70.0,
                    y: 70.0,
                    r: 20.0,
                    gc,
                },
            ],
            device: device(100.0, 100.0),
        };
        let png = RasterRenderer::default().render(&plot).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn custom_output_dimensions() {
        let plot = Plot {
            session_id: None,
            ops: vec![],
            device: device(800.0, 600.0),
        };
        let renderer = RasterRenderer {
            output_width: Some(400),
            output_height: Some(300),
        };
        let png = renderer.render(&plot).unwrap();
        // Decode PNG to verify dimensions.
        let pm = Pixmap::decode_png(&png).unwrap();
        assert_eq!(pm.width(), 400);
        assert_eq!(pm.height(), 300);
    }

    #[test]
    fn clip_restricts_drawing() {
        let plot = Plot {
            session_id: None,
            ops: vec![
                DrawingOp::Clip {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 50.0,
                    y1: 50.0,
                },
                DrawingOp::Rect {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 100.0,
                    y1: 100.0,
                    gc: GraphicsContext {
                        fill: Some("rgba(255,0,0,1)".into()),
                        ..Default::default()
                    },
                },
            ],
            device: device(100.0, 100.0),
        };
        let png = RasterRenderer::default().render(&plot).unwrap();
        let pm = Pixmap::decode_png(&png).unwrap();
        // Pixel at (75, 75) should be transparent (outside clip).
        let px = pm.pixel(75, 75).unwrap();
        assert_eq!(px.alpha(), 0, "pixel outside clip should be transparent");
    }

    #[test]
    fn text_rendering_dpi_check() {
        let gc = GraphicsContext {
            col: Some("rgba(0,0,0,1)".into()),
            fill: Some("rgba(255,255,255,1)".into()),
            font: jgd_protocol::gc::FontContext {
                family: "sans".into(),
                face: 1,
                size: 12.0,
                lineheight: 1.2,
            },
            lwd: 1.0,
            ..Default::default()
        };
        let plot = Plot {
            session_id: None,
            ops: vec![
                DrawingOp::Rect {
                    x0: 0.0, y0: 0.0, x1: 768.0, y1: 576.0,
                    gc: GraphicsContext {
                        fill: Some("rgba(255,255,255,1)".into()),
                        ..Default::default()
                    },
                },
                DrawingOp::Text {
                    x: 384.0, y: 550.0, r#str: "speed".into(),
                    rot: 0.0, hadj: 0.5, gc: gc.clone(),
                },
                DrawingOp::Text {
                    x: 30.0, y: 288.0, r#str: "dist".into(),
                    rot: 90.0, hadj: 0.5, gc: gc.clone(),
                },
            ],
            device: DeviceInfo {
                width: 768.0, height: 576.0,
                dpi: Some(96.0),
                bg: Some("rgba(255,255,255,1)".into()),
            },
        };
        let png = RasterRenderer::default().render(&plot).unwrap();

        // Verify text pixels are present: scan for non-white pixels in the
        // "speed" label area (around y=540-560, x=340-430).
        let pm = Pixmap::decode_png(&png).unwrap();
        let mut text_pixels = 0u32;
        let mut speed_min_x = u32::MAX;
        let mut speed_max_x = 0u32;
        for py in 520..576 {
            for px in 300..470 {
                let pixel = pm.pixel(px, py).unwrap();
                if pixel.red() < 200 && pixel.alpha() > 128 {
                    text_pixels += 1;
                    speed_min_x = speed_min_x.min(px);
                    speed_max_x = speed_max_x.max(px);
                }
            }
        }
        assert!(text_pixels > 50, "expected visible text pixels for 'speed' label, got {text_pixels}");

        // Verify glyphs are properly spaced (not all overlapping).
        assert!(
            speed_max_x >= speed_min_x,
            "no dark pixels found for bounding-box measurement"
        );
        let speed_width = speed_max_x - speed_min_x + 1;
        assert!(
            speed_width > 30,
            "'speed' label width should be >30px (glyphs properly spaced), got {speed_width}px"
        );
    }
}
