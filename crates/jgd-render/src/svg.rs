//! SVG rendering backend.

use std::fmt::Write;

use jgd_protocol::{
    DrawingOp, FillRule, GraphicsContext, LineCap, LineJoin, Plot, gc::FontContext,
};

use crate::Renderer;

/// Renders a [`Plot`] as an SVG document string.
#[derive(Debug, Clone, Default)]
pub struct SvgRenderer;

impl Renderer for SvgRenderer {
    type Output = String;
    type Error = std::fmt::Error;

    fn render(&self, plot: &Plot) -> Result<String, std::fmt::Error> {
        let w = plot.device.width;
        let h = plot.device.height;

        let mut defs = String::new();
        let mut body = String::new();
        let mut clip_id: usize = 0;
        // Track nesting so that clip <g> and group <g> don't interfere.
        let mut nesting: Vec<NestingKind> = Vec::new();

        // Background.
        if let Some(bg) = &plot.device.bg {
            writeln!(
                body,
                "  <rect width=\"{w}\" height=\"{h}\" fill=\"{}\"/>",
                xml_escape(bg),
            )?;
        }

        for op in &plot.ops {
            match op {
                DrawingOp::Clip { x0, y0, x1, y1 } => {
                    // Close only the innermost clip group (skip over any
                    // intervening user groups so they remain correctly nested).
                    close_innermost_clip(&mut body, &mut nesting)?;

                    let cid = clip_id;
                    clip_id += 1;
                    let cx = x0.min(*x1);
                    let cy = y0.min(*y1);
                    let cw = (x1 - x0).abs();
                    let ch = (y1 - y0).abs();
                    writeln!(
                        defs,
                        "    <clipPath id=\"clip-{cid}\"><rect x=\"{cx}\" y=\"{cy}\" \
                         width=\"{cw}\" height=\"{ch}\"/></clipPath>",
                    )?;
                    writeln!(body, "  <g clip-path=\"url(#clip-{cid})\">")?;
                    nesting.push(NestingKind::Clip);
                }

                DrawingOp::Line { x1, y1, x2, y2, gc } => {
                    write!(
                        body,
                        "    <line x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\""
                    )?;
                    write_stroke_attrs(&mut body, gc)?;
                    body.push_str("/>\n");
                }

                DrawingOp::Polyline { x, y, gc } => {
                    body.push_str("    <polyline points=\"");
                    write_points(&mut body, x, y)?;
                    body.push('"');
                    write_stroke_attrs(&mut body, gc)?;
                    body.push_str(" fill=\"none\"/>\n");
                }

                DrawingOp::Polygon { x, y, gc } => {
                    body.push_str("    <polygon points=\"");
                    write_points(&mut body, x, y)?;
                    body.push('"');
                    write_stroke_attrs(&mut body, gc)?;
                    write_fill_attr(&mut body, gc)?;
                    body.push_str("/>\n");
                }

                DrawingOp::Rect { x0, y0, x1, y1, gc } => {
                    let rx = x0.min(*x1);
                    let ry = y0.min(*y1);
                    let rw = (x1 - x0).abs();
                    let rh = (y1 - y0).abs();
                    write!(
                        body,
                        "    <rect x=\"{rx}\" y=\"{ry}\" width=\"{rw}\" height=\"{rh}\"",
                    )?;
                    write_stroke_attrs(&mut body, gc)?;
                    write_fill_attr(&mut body, gc)?;
                    body.push_str("/>\n");
                }

                DrawingOp::Circle { x, y, r, gc } => {
                    write!(body, "    <circle cx=\"{x}\" cy=\"{y}\" r=\"{r}\"")?;
                    write_stroke_attrs(&mut body, gc)?;
                    write_fill_attr(&mut body, gc)?;
                    body.push_str("/>\n");
                }

                DrawingOp::Text {
                    x,
                    y,
                    r#str: text,
                    rot,
                    hadj,
                    gc,
                } => {
                    body.push_str("    <text");
                    write!(body, " x=\"{x}\" y=\"{y}\"")?;

                    // text-anchor from hadj.
                    let anchor = if *hadj >= 0.9 {
                        "end"
                    } else if *hadj >= 0.4 {
                        "middle"
                    } else {
                        "start"
                    };
                    if anchor != "start" {
                        write!(body, " text-anchor=\"{anchor}\"")?;
                    }

                    // Rotation (R uses counter-clockwise degrees, SVG clockwise).
                    if rot.abs() > 1e-6 {
                        write!(body, " transform=\"rotate({},{x},{y})\"", -rot)?;
                    }

                    // Text color comes from gc.col in R.
                    match &gc.col {
                        Some(c) => write!(body, " fill=\"{}\"", xml_escape(c))?,
                        None => body.push_str(" fill=\"none\""),
                    }

                    write_font_attrs(&mut body, &gc.font)?;
                    body.push('>');
                    body.push_str(&xml_escape(text));
                    body.push_str("</text>\n");
                }

                DrawingOp::Path {
                    winding,
                    subpaths,
                    gc,
                } => {
                    body.push_str("    <path d=\"");
                    for subpath in subpaths {
                        for (i, pt) in subpath.iter().enumerate() {
                            if i == 0 {
                                write!(body, "M{} {}", pt[0], pt[1])?;
                            } else {
                                write!(body, " L{} {}", pt[0], pt[1])?;
                            }
                        }
                        body.push_str(" Z");
                    }
                    body.push('"');

                    let rule = match winding {
                        FillRule::Nonzero => "nonzero",
                        FillRule::Evenodd => "evenodd",
                    };
                    write!(body, " fill-rule=\"{rule}\" clip-rule=\"{rule}\"")?;
                    write_stroke_attrs(&mut body, gc)?;
                    write_fill_attr(&mut body, gc)?;
                    body.push_str("/>\n");
                }

                DrawingOp::Raster {
                    x,
                    y,
                    w,
                    h,
                    rot,
                    interpolate,
                    data,
                    ..
                } => {
                    write!(
                        body,
                        "    <image x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\"",
                    )?;
                    if rot.abs() > 1e-6 {
                        write!(body, " transform=\"rotate({},{x},{y})\"", -rot)?;
                    }
                    let rendering = if *interpolate {
                        "optimizeQuality"
                    } else {
                        "optimizeSpeed"
                    };
                    write!(body, " image-rendering=\"{rendering}\"")?;
                    writeln!(
                        body,
                        " href=\"data:image/png;base64,{}\"/>",
                        xml_escape(data),
                    )?;
                }

                // Intentionally ignores BeginGroup.ext — no SVG mapping yet.
                DrawingOp::BeginGroup { .. } => {
                    body.push_str("    <g>\n");
                    nesting.push(NestingKind::Group);
                }
                DrawingOp::EndGroup => {
                    if let Some(NestingKind::Group) = nesting.last() {
                        nesting.pop();
                    }
                    body.push_str("    </g>\n");
                }
            }
        }

        // Close any remaining clip group.
        close_innermost_clip(&mut body, &mut nesting)?;

        // Assemble final SVG document.
        let mut svg = String::new();
        writeln!(
            svg,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\">",
        )?;
        if !defs.is_empty() {
            svg.push_str("  <defs>\n");
            svg.push_str(&defs);
            svg.push_str("  </defs>\n");
        }
        svg.push_str(&body);
        svg.push_str("</svg>\n");
        Ok(svg)
    }
}

// ---------------------------------------------------------------------------
// Nesting
// ---------------------------------------------------------------------------

/// Tracks whether an open `<g>` belongs to a clip region or a user group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestingKind {
    Clip,
    Group,
}

/// Close the innermost clip `<g>`, temporarily re-closing and re-opening
/// any intervening user groups so the SVG nesting stays valid.
fn close_innermost_clip(body: &mut String, nesting: &mut Vec<NestingKind>) -> std::fmt::Result {
    // Find the innermost Clip.
    let clip_pos = nesting.iter().rposition(|k| *k == NestingKind::Clip);
    let Some(pos) = clip_pos else {
        return Ok(());
    };

    // Close everything from the top down to (and including) the clip.
    let groups_above = nesting.len() - pos - 1;
    for _ in 0..groups_above {
        body.push_str("    </g>\n"); // close intervening group
    }
    body.push_str("  </g>\n"); // close clip

    // Remove the clip entry; keep the group entries so they get re-opened.
    nesting.remove(pos);

    // Re-open the intervening groups.
    for _ in 0..groups_above {
        body.push_str("    <g>\n");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Intentionally ignores gc.ext — no SVG mapping for extension fields yet.
fn write_stroke_attrs(buf: &mut String, gc: &GraphicsContext) -> std::fmt::Result {
    match &gc.col {
        Some(c) => write!(buf, " stroke=\"{}\"", xml_escape(c))?,
        None => buf.push_str(" stroke=\"none\""),
    }
    write!(buf, " stroke-width=\"{}\"", gc.lwd)?;
    if !gc.lty.is_empty() {
        buf.push_str(" stroke-dasharray=\"");
        for (i, v) in gc.lty.iter().enumerate() {
            if i > 0 {
                buf.push(',');
            }
            write!(buf, "{v}")?;
        }
        buf.push('"');
    }
    let cap = match gc.lend {
        LineCap::Round => "round",
        LineCap::Butt => "butt",
        LineCap::Square => "square",
    };
    write!(buf, " stroke-linecap=\"{cap}\"")?;
    let join = match gc.ljoin {
        LineJoin::Round => "round",
        LineJoin::Miter => "miter",
        LineJoin::Bevel => "bevel",
    };
    write!(buf, " stroke-linejoin=\"{join}\"")?;
    if gc.ljoin == LineJoin::Miter {
        write!(buf, " stroke-miterlimit=\"{}\"", gc.lmitre)?;
    }
    Ok(())
}

fn write_fill_attr(buf: &mut String, gc: &GraphicsContext) -> std::fmt::Result {
    match &gc.fill {
        Some(c) => write!(buf, " fill=\"{}\"", xml_escape(c)),
        None => {
            buf.push_str(" fill=\"none\"");
            Ok(())
        }
    }
}

fn write_font_attrs(buf: &mut String, font: &FontContext) -> std::fmt::Result {
    // Intentionally ignores font.lineheight — no direct SVG text mapping.
    if !font.family.is_empty() {
        write!(buf, " font-family=\"{}\"", xml_escape(&font.family))?;
    }
    write!(buf, " font-size=\"{}\"", font.size)?;
    match font.face {
        2 => buf.push_str(" font-weight=\"bold\""),
        3 => buf.push_str(" font-style=\"italic\""),
        4 => buf.push_str(" font-weight=\"bold\" font-style=\"italic\""),
        _ => {}
    }
    Ok(())
}

fn write_points(buf: &mut String, x: &[f64], y: &[f64]) -> std::fmt::Result {
    for (i, (px, py)) in x.iter().zip(y.iter()).enumerate() {
        if i > 0 {
            buf.push(' ');
        }
        write!(buf, "{px},{py}")?;
    }
    Ok(())
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::message::DeviceInfo;

    fn device(w: f64, h: f64) -> DeviceInfo {
        DeviceInfo {
            width: w,
            height: h,
            dpi: None,
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
    fn empty_plot_produces_valid_svg() {
        let plot = Plot {
            session_id: None,
            ops: vec![],
            device: device(800.0, 600.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.starts_with("<svg xmlns="));
        assert!(svg.contains("width=\"800\""));
        assert!(svg.contains("height=\"600\""));
        assert!(svg.ends_with("</svg>\n"));
    }

    #[test]
    fn background_rect() {
        let plot = Plot {
            session_id: None,
            ops: vec![],
            device: DeviceInfo {
                width: 100.0,
                height: 100.0,
                dpi: None,
                bg: Some("white".into()),
            },
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("fill=\"white\""));
    }

    #[test]
    fn line_op() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 100.0,
                gc: simple_gc(),
            }],
            device: device(200.0, 200.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("<line x1=\"0\" y1=\"0\" x2=\"100\" y2=\"100\""));
        assert!(svg.contains("stroke=\"rgba(0,0,0,1)\""));
        assert!(svg.contains("stroke-width=\"2\""));
    }

    #[test]
    fn rect_op_normalizes_coords() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Rect {
                x0: 50.0,
                y0: 80.0,
                x1: 10.0,
                y1: 20.0,
                gc: simple_gc(),
            }],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("x=\"10\""));
        assert!(svg.contains("y=\"20\""));
        assert!(svg.contains("width=\"40\""));
        assert!(svg.contains("height=\"60\""));
    }

    #[test]
    fn text_with_rotation() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Text {
                x: 50.0,
                y: 50.0,
                r#str: "Hello".into(),
                rot: 90.0,
                hadj: 0.5,
                gc: simple_gc(),
            }],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("text-anchor=\"middle\""));
        assert!(svg.contains("transform=\"rotate(-90,50,50)\""));
        assert!(svg.contains(">Hello</text>"));
    }

    #[test]
    fn text_xml_escaping() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Text {
                x: 0.0,
                y: 0.0,
                r#str: "<b>&\"test\"</b>".into(),
                rot: 0.0,
                hadj: 0.0,
                gc: simple_gc(),
            }],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("&lt;b&gt;&amp;&quot;test&quot;&lt;/b&gt;"));
    }

    #[test]
    fn clip_creates_defs_and_group() {
        let plot = Plot {
            session_id: None,
            ops: vec![
                DrawingOp::Clip {
                    x0: 10.0,
                    y0: 10.0,
                    x1: 90.0,
                    y1: 90.0,
                },
                DrawingOp::Line {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 100.0,
                    y2: 100.0,
                    gc: simple_gc(),
                },
            ],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("<defs>"));
        assert!(svg.contains("<clipPath id=\"clip-0\">"));
        assert!(svg.contains("clip-path=\"url(#clip-0)\""));
    }

    #[test]
    fn circle_and_polygon() {
        let gc = simple_gc();
        let plot = Plot {
            session_id: None,
            ops: vec![
                DrawingOp::Circle {
                    x: 50.0,
                    y: 50.0,
                    r: 25.0,
                    gc: gc.clone(),
                },
                DrawingOp::Polygon {
                    x: vec![0.0, 50.0, 100.0],
                    y: vec![100.0, 0.0, 100.0],
                    gc,
                },
            ],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("<circle cx=\"50\" cy=\"50\" r=\"25\""));
        assert!(svg.contains("<polygon points=\"0,100 50,0 100,100\""));
    }

    #[test]
    fn path_with_fill_rule() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Path {
                winding: FillRule::Evenodd,
                subpaths: vec![vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]],
                gc: simple_gc(),
            }],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("fill-rule=\"evenodd\""));
        assert!(svg.contains("d=\"M0 0 L100 0 L100 100 Z\""));
    }

    #[test]
    fn polyline_op() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Polyline {
                x: vec![10.0, 20.0, 30.0],
                y: vec![40.0, 50.0, 60.0],
                gc: simple_gc(),
            }],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("<polyline points=\"10,40 20,50 30,60\""));
        assert!(svg.contains("fill=\"none\""));
    }

    #[test]
    fn raster_op() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Raster {
                x: 10.0,
                y: 20.0,
                w: 64.0,
                h: 48.0,
                rot: 45.0,
                interpolate: false,
                pw: 64,
                ph: 48,
                data: "AAAA".into(),
            }],
            device: device(200.0, 200.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("x=\"10\" y=\"20\" width=\"64\" height=\"48\""));
        assert!(svg.contains("transform=\"rotate(-45,10,20)\""));
        assert!(svg.contains("image-rendering=\"optimizeSpeed\""));
        assert!(svg.contains("href=\"data:image/png;base64,AAAA\""));
    }

    #[test]
    fn stroke_dasharray() {
        let gc = GraphicsContext {
            col: Some("rgba(0,0,0,1)".into()),
            lty: vec![4.0, 2.0],
            ..Default::default()
        };
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 0.0,
                gc,
            }],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("stroke-dasharray=\"4,2\""));
    }

    #[test]
    fn clip_and_group_interleave() {
        // BeginGroup → Clip → EndGroup must produce valid nesting.
        let plot = Plot {
            session_id: None,
            ops: vec![
                DrawingOp::BeginGroup { ext: None },
                DrawingOp::Clip {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 50.0,
                    y1: 50.0,
                },
                DrawingOp::Line {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 50.0,
                    y2: 50.0,
                    gc: simple_gc(),
                },
                DrawingOp::EndGroup,
            ],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        // Must contain clip infrastructure.
        assert!(svg.contains("<clipPath id=\"clip-0\">"));
        // The SVG must be well-formed: count <g> and </g> must match.
        let opens = svg.matches("<g").count();
        let closes = svg.matches("</g>").count();
        assert_eq!(opens, closes, "mismatched <g>/</g>: {svg}");
    }

    #[test]
    fn group_nesting() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::BeginGroup { ext: None }, DrawingOp::EndGroup],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer.render(&plot).unwrap();
        assert!(svg.contains("<g>"));
        assert!(svg.contains("</g>"));
    }
}
