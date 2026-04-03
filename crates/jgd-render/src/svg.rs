//! SVG rendering backend using quick-xml.

use quick_xml::Writer;
use quick_xml::events::{BytesStart, BytesText, Event};

use jgd_protocol::{
    DrawingOp, FillRule, GraphicsContext, LineCap, LineJoin, Plot, gc::FontContext,
};

use crate::Renderer;

/// Renders a [`Plot`] as an SVG document string.
///
/// When `output_width` / `output_height` are set the SVG element uses those
/// values for its `width`/`height` attributes while a `viewBox` maps the
/// original device coordinate system, allowing the browser to scale the
/// drawing to any size.
#[derive(Debug, Clone, Default)]
pub struct SvgRenderer {
    /// Desired output width (SVG `width` attribute).  Falls back to device width.
    pub output_width: Option<f64>,
    /// Desired output height (SVG `height` attribute).  Falls back to device height.
    pub output_height: Option<f64>,
}

/// Error type for SVG rendering.
#[derive(Debug, thiserror::Error)]
pub enum SvgError {
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Renderer for SvgRenderer {
    type Output = String;
    type Error = SvgError;

    fn render(&self, plot: &Plot) -> Result<String, SvgError> {
        let w = plot.device.width;
        let h = plot.device.height;

        let mut defs_writer = Writer::new(Vec::new());
        let mut body_writer = Writer::new(Vec::new());
        let mut clip_id: usize = 0;
        let mut nesting: Vec<NestingKind> = Vec::new();

        // Background.
        if let Some(bg) = &plot.device.bg {
            let elem = BytesStart::new("rect")
                .with_attributes(vec![
                    ("width", ftoa(w).as_str()),
                    ("height", ftoa(h).as_str()),
                    ("fill", bg.as_str()),
                ]);
            body_writer.write_event(Event::Empty(elem))?;
        }

        for op in &plot.ops {
            match op {
                DrawingOp::Clip { x0, y0, x1, y1 } => {
                    close_innermost_clip(&mut body_writer, &mut nesting)?;

                    let cid = clip_id;
                    clip_id += 1;
                    let cx = x0.min(*x1);
                    let cy = y0.min(*y1);
                    let cw = (x1 - x0).abs();
                    let ch = (y1 - y0).abs();

                    // Write clipPath def.
                    let clip_start =
                        BytesStart::new("clipPath").with_attributes(vec![(
                            "id",
                            format!("clip-{cid}").as_str(),
                        )]);
                    defs_writer.write_event(Event::Start(clip_start.borrow()))?;
                    let rect = BytesStart::new("rect").with_attributes(vec![
                        ("x", ftoa(cx).as_str()),
                        ("y", ftoa(cy).as_str()),
                        ("width", ftoa(cw).as_str()),
                        ("height", ftoa(ch).as_str()),
                    ]);
                    defs_writer.write_event(Event::Empty(rect))?;
                    defs_writer.write_event(Event::End(clip_start.to_end()))?;

                    // Open clip group in body.
                    let g = BytesStart::new("g").with_attributes(vec![(
                        "clip-path",
                        format!("url(#clip-{cid})").as_str(),
                    )]);
                    body_writer.write_event(Event::Start(g))?;
                    nesting.push(NestingKind::Clip);
                }

                DrawingOp::Line { x1, y1, x2, y2, gc } => {
                    let mut elem = BytesStart::new("line");
                    push_attr(&mut elem, "x1", &ftoa(*x1));
                    push_attr(&mut elem, "y1", &ftoa(*y1));
                    push_attr(&mut elem, "x2", &ftoa(*x2));
                    push_attr(&mut elem, "y2", &ftoa(*y2));
                    push_stroke_attrs(&mut elem, gc);
                    body_writer.write_event(Event::Empty(elem))?;
                }

                DrawingOp::Polyline { x, y, gc } => {
                    let mut elem = BytesStart::new("polyline");
                    push_attr(&mut elem, "points", &format_points(x, y));
                    push_stroke_attrs(&mut elem, gc);
                    push_attr(&mut elem, "fill", "none");
                    body_writer.write_event(Event::Empty(elem))?;
                }

                DrawingOp::Polygon { x, y, gc } => {
                    let mut elem = BytesStart::new("polygon");
                    push_attr(&mut elem, "points", &format_points(x, y));
                    push_stroke_attrs(&mut elem, gc);
                    push_fill_attr(&mut elem, gc);
                    body_writer.write_event(Event::Empty(elem))?;
                }

                DrawingOp::Rect { x0, y0, x1, y1, gc } => {
                    let rx = x0.min(*x1);
                    let ry = y0.min(*y1);
                    let rw = (x1 - x0).abs();
                    let rh = (y1 - y0).abs();
                    let mut elem = BytesStart::new("rect");
                    push_attr(&mut elem, "x", &ftoa(rx));
                    push_attr(&mut elem, "y", &ftoa(ry));
                    push_attr(&mut elem, "width", &ftoa(rw));
                    push_attr(&mut elem, "height", &ftoa(rh));
                    push_stroke_attrs(&mut elem, gc);
                    push_fill_attr(&mut elem, gc);
                    body_writer.write_event(Event::Empty(elem))?;
                }

                DrawingOp::Circle { x, y, r, gc } => {
                    let mut elem = BytesStart::new("circle");
                    push_attr(&mut elem, "cx", &ftoa(*x));
                    push_attr(&mut elem, "cy", &ftoa(*y));
                    push_attr(&mut elem, "r", &ftoa(*r));
                    push_stroke_attrs(&mut elem, gc);
                    push_fill_attr(&mut elem, gc);
                    body_writer.write_event(Event::Empty(elem))?;
                }

                DrawingOp::Text {
                    x,
                    y,
                    r#str: text,
                    rot,
                    hadj,
                    gc,
                } => {
                    let mut elem = BytesStart::new("text");
                    push_attr(&mut elem, "x", &ftoa(*x));
                    push_attr(&mut elem, "y", &ftoa(*y));

                    let anchor = if *hadj >= 0.9 {
                        "end"
                    } else if *hadj >= 0.4 {
                        "middle"
                    } else {
                        "start"
                    };
                    if anchor != "start" {
                        push_attr(&mut elem, "text-anchor", anchor);
                    }

                    if rot.abs() > 1e-6 {
                        push_attr(
                            &mut elem,
                            "transform",
                            &format!("rotate({},{},{})", -rot, x, y),
                        );
                    }

                    match &gc.col {
                        Some(c) => push_attr(&mut elem, "fill", c),
                        None => push_attr(&mut elem, "fill", "none"),
                    }

                    push_font_attrs(&mut elem, &gc.font);

                    body_writer.write_event(Event::Start(elem.borrow()))?;
                    body_writer
                        .write_event(Event::Text(BytesText::new(text)))?;
                    body_writer.write_event(Event::End(elem.to_end()))?;
                }

                DrawingOp::Path {
                    winding,
                    subpaths,
                    gc,
                } => {
                    let mut d = String::new();
                    for subpath in subpaths {
                        for (i, pt) in subpath.iter().enumerate() {
                            if i == 0 {
                                d.push_str(&format!("M{} {}", pt[0], pt[1]));
                            } else {
                                d.push_str(&format!(" L{} {}", pt[0], pt[1]));
                            }
                        }
                        d.push_str(" Z");
                    }
                    let rule = match winding {
                        FillRule::Nonzero => "nonzero",
                        FillRule::Evenodd => "evenodd",
                    };

                    let mut elem = BytesStart::new("path");
                    push_attr(&mut elem, "d", &d);
                    push_attr(&mut elem, "fill-rule", rule);
                    push_attr(&mut elem, "clip-rule", rule);
                    push_stroke_attrs(&mut elem, gc);
                    push_fill_attr(&mut elem, gc);
                    body_writer.write_event(Event::Empty(elem))?;
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
                    let mut elem = BytesStart::new("image");
                    push_attr(&mut elem, "x", &ftoa(*x));
                    push_attr(&mut elem, "y", &ftoa(*y));
                    push_attr(&mut elem, "width", &ftoa(*w));
                    push_attr(&mut elem, "height", &ftoa(*h));
                    if rot.abs() > 1e-6 {
                        push_attr(
                            &mut elem,
                            "transform",
                            &format!("rotate({},{},{})", -rot, x, y),
                        );
                    }
                    let rendering = if *interpolate {
                        "optimizeQuality"
                    } else {
                        "optimizeSpeed"
                    };
                    push_attr(&mut elem, "image-rendering", rendering);
                    push_attr(
                        &mut elem,
                        "href",
                        &format!("data:image/png;base64,{data}"),
                    );
                    body_writer.write_event(Event::Empty(elem))?;
                }

                DrawingOp::BeginGroup { .. } => {
                    body_writer.write_event(Event::Start(BytesStart::new("g")))?;
                    nesting.push(NestingKind::Group);
                }
                DrawingOp::EndGroup => {
                    if let Some(NestingKind::Clip) = nesting.last() {
                        close_innermost_clip(&mut body_writer, &mut nesting)?;
                    }
                    if let Some(NestingKind::Group) = nesting.last() {
                        nesting.pop();
                    }
                    body_writer
                        .write_event(Event::End(BytesStart::new("g").to_end()))?;
                }
            }
        }

        // Close any remaining clip group.
        close_innermost_clip(&mut body_writer, &mut nesting)?;

        // Assemble final SVG document.
        let out_w = self.output_width.unwrap_or(w);
        let out_h = self.output_height.unwrap_or(h);

        let mut svg_writer = Writer::new(Vec::new());
        let svg_start = BytesStart::new("svg").with_attributes(vec![
            ("xmlns", "http://www.w3.org/2000/svg"),
            ("width", ftoa(out_w).as_str()),
            ("height", ftoa(out_h).as_str()),
            ("viewBox", format!("0 0 {w} {h}").as_str()),
            ("preserveAspectRatio", "none"),
        ]);
        svg_writer.write_event(Event::Start(svg_start.borrow()))?;

        let defs_bytes = defs_writer.into_inner();
        if !defs_bytes.is_empty() {
            svg_writer
                .write_event(Event::Start(BytesStart::new("defs")))?;
            svg_writer.get_mut().extend_from_slice(&defs_bytes);
            svg_writer
                .write_event(Event::End(BytesStart::new("defs").to_end()))?;
        }

        let body_bytes = body_writer.into_inner();
        svg_writer.get_mut().extend_from_slice(&body_bytes);

        svg_writer.write_event(Event::End(svg_start.to_end()))?;

        let bytes = svg_writer.into_inner();
        Ok(String::from_utf8(bytes).expect("quick-xml produces valid UTF-8"))
    }
}

// ---------------------------------------------------------------------------
// Nesting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestingKind {
    Clip,
    Group,
}

fn close_innermost_clip(
    writer: &mut Writer<Vec<u8>>,
    nesting: &mut Vec<NestingKind>,
) -> Result<(), SvgError> {
    let clip_pos = nesting.iter().rposition(|k| *k == NestingKind::Clip);
    let Some(pos) = clip_pos else {
        return Ok(());
    };

    let groups_above = nesting.len() - pos - 1;
    let g_tag = BytesStart::new("g");
    let end_g = g_tag.to_end();
    for _ in 0..groups_above {
        writer.write_event(Event::End(end_g.borrow()))?;
    }
    writer.write_event(Event::End(end_g.borrow()))?;

    nesting.remove(pos);

    for _ in 0..groups_above {
        writer.write_event(Event::Start(BytesStart::new("g")))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format an f64 for SVG output, stripping unnecessary trailing zeros.
fn ftoa(v: f64) -> String {
    if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn push_attr(elem: &mut BytesStart, key: &str, value: &str) {
    elem.push_attribute((key, value));
}

fn push_stroke_attrs(elem: &mut BytesStart, gc: &GraphicsContext) {
    match &gc.col {
        Some(c) => push_attr(elem, "stroke", c),
        None => push_attr(elem, "stroke", "none"),
    }
    push_attr(elem, "stroke-width", &ftoa(gc.lwd));
    if !gc.lty.is_empty() {
        let dash: String = gc
            .lty
            .iter()
            .map(|v| ftoa(*v))
            .collect::<Vec<_>>()
            .join(",");
        push_attr(elem, "stroke-dasharray", &dash);
    }
    let cap = match gc.lend {
        LineCap::Round => "round",
        LineCap::Butt => "butt",
        LineCap::Square => "square",
    };
    push_attr(elem, "stroke-linecap", cap);
    let join = match gc.ljoin {
        LineJoin::Round => "round",
        LineJoin::Miter => "miter",
        LineJoin::Bevel => "bevel",
    };
    push_attr(elem, "stroke-linejoin", join);
    if gc.ljoin == LineJoin::Miter {
        push_attr(elem, "stroke-miterlimit", &ftoa(gc.lmitre));
    }
}

fn push_fill_attr(elem: &mut BytesStart, gc: &GraphicsContext) {
    match &gc.fill {
        Some(c) => push_attr(elem, "fill", c),
        None => push_attr(elem, "fill", "none"),
    }
}

fn push_font_attrs(elem: &mut BytesStart, font: &FontContext) {
    if !font.family.is_empty() {
        push_attr(elem, "font-family", &font.family);
    }
    push_attr(elem, "font-size", &ftoa(font.size));
    match font.face {
        2 => push_attr(elem, "font-weight", "bold"),
        3 => push_attr(elem, "font-style", "italic"),
        4 => {
            push_attr(elem, "font-weight", "bold");
            push_attr(elem, "font-style", "italic");
        }
        _ => {}
    }
}

fn format_points(x: &[f64], y: &[f64]) -> String {
    x.iter()
        .zip(y.iter())
        .map(|(px, py)| format!("{},{}", ftoa(*px), ftoa(*py)))
        .collect::<Vec<_>>()
        .join(" ")
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
        assert!(svg.starts_with("<svg xmlns="));
        assert!(svg.contains("width=\"800\""));
        assert!(svg.contains("height=\"600\""));
        assert!(svg.ends_with("</svg>"));
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
        // quick-xml does not escape `"` in text nodes (valid per XML spec).
        assert!(svg.contains("&lt;b&gt;&amp;\"test\"&lt;/b&gt;")
            || svg.contains("&lt;b&gt;&amp;&quot;test&quot;&lt;/b&gt;"));
        // Verify the essential escapes are present.
        assert!(svg.contains("&lt;b&gt;"));
        assert!(svg.contains("&amp;"));
        assert!(svg.contains("&lt;/b&gt;"));
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
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
        let svg = SvgRenderer::default().render(&plot).unwrap();
        assert!(svg.contains("stroke-dasharray=\"4,2\""));
    }

    #[test]
    fn clip_and_group_interleave() {
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
                DrawingOp::Line {
                    x1: 60.0,
                    y1: 60.0,
                    x2: 90.0,
                    y2: 90.0,
                    gc: simple_gc(),
                },
            ],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer::default().render(&plot).unwrap();

        let opens = svg.matches("<g").count();
        let closes = svg.matches("</g>").count();
        assert_eq!(opens, closes, "mismatched <g>/</g>: {svg}");

        let group_open = svg.find("<g>").expect("missing group <g>");
        let clip_open = svg.find("<g clip-path=").expect("missing clip <g>");
        assert!(
            group_open < clip_open,
            "group <g> should appear before clip <g>: {svg}"
        );

        let trailing_line = svg.find("x1=\"60\"").expect("missing trailing line");
        let last_close_g = svg.rfind("</g>").expect("missing </g>");
        assert!(
            trailing_line > last_close_g,
            "trailing line should be outside all groups: {svg}"
        );
    }

    #[test]
    fn group_nesting() {
        let plot = Plot {
            session_id: None,
            ops: vec![DrawingOp::BeginGroup { ext: None }, DrawingOp::EndGroup],
            device: device(100.0, 100.0),
        };
        let svg = SvgRenderer::default().render(&plot).unwrap();
        assert!(svg.contains("<g>"));
        assert!(svg.contains("</g>"));
    }

    #[test]
    fn output_dimensions_differ_from_device() {
        let plot = Plot {
            session_id: None,
            ops: vec![],
            device: device(800.0, 600.0),
        };
        let renderer = SvgRenderer {
            output_width: Some(400.0),
            output_height: Some(300.0),
        };
        let svg = renderer.render(&plot).unwrap();
        assert!(svg.contains(r#"width="400""#));
        assert!(svg.contains(r#"height="300""#));
        assert!(svg.contains(r#"viewBox="0 0 800 600""#));
        assert!(svg.contains(r#"preserveAspectRatio="none""#));
    }
}
