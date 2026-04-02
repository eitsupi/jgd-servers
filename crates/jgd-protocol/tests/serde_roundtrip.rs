use jgd_protocol::*;

/// Round-trip: serialize then deserialize, check equality.
fn roundtrip(msg: &Message) {
    let json = serde_json::to_string(msg).expect("serialize");
    let decoded: Message = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(*msg, decoded, "round-trip failed for:\n{json}");
}

#[test]
fn ping_pong() {
    roundtrip(&Message::Ping);
    roundtrip(&Message::Pong);
}

#[test]
fn close() {
    roundtrip(&Message::Close);
}

#[test]
fn server_info() {
    let msg = Message::ServerInfo(ServerInfoMessage {
        server_name: "jgd-rust-server".into(),
        protocol_version: 1,
        transport: Transport::Unix,
        server_info: None,
    });
    roundtrip(&msg);
}

#[test]
fn metrics_request_str_width() {
    let msg = Message::MetricsRequest(MetricsRequest {
        id: 42,
        kind: MetricsKind::StrWidth,
        str: Some("Hello".into()),
        c: None,
        gc: GraphicsContext {
            col: Some("rgba(0,0,0,1)".into()),
            fill: None,
            lwd: 1.0,
            lty: vec![],
            lend: LineCap::Round,
            ljoin: LineJoin::Round,
            lmitre: 10.0,
            font: FontContext {
                family: "sans".into(),
                face: 1,
                size: 12.0,
                lineheight: 1.0,
            },
            ext: None,
        },
    });
    roundtrip(&msg);
}

#[test]
fn metrics_response() {
    let msg = Message::MetricsResponse(MetricsResponse {
        id: 42,
        width: 48.5,
        ascent: 10.2,
        descent: 2.8,
    });
    roundtrip(&msg);
}

#[test]
fn resize() {
    let msg = Message::Resize(ResizeMessage {
        width: 800.0,
        height: 600.0,
        plot_index: Some(3),
        session_id: Some("r-1234-1".into()),
    });
    roundtrip(&msg);
}

#[test]
fn frame_with_ops() {
    let msg = Message::Frame(FrameMessage {
        plot: Plot {
            session_id: Some("r-1234-1".into()),
            device: DeviceInfo {
                width: 720.0,
                height: 576.0,
                dpi: Some(72.0),
                bg: Some("rgba(255,255,255,1)".into()),
            },
            ops: vec![
                DrawingOp::Clip {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 720.0,
                    y1: 576.0,
                },
                DrawingOp::Rect {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 720.0,
                    y1: 576.0,
                    gc: GraphicsContext {
                        col: None,
                        fill: Some("rgba(255,255,255,1)".into()),
                        lwd: 1.0,
                        lty: vec![],
                        lend: LineCap::Round,
                        ljoin: LineJoin::Round,
                        lmitre: 10.0,
                        font: FontContext::default(),
                        ext: None,
                    },
                },
                DrawingOp::Line {
                    x1: 10.0,
                    y1: 20.0,
                    x2: 100.0,
                    y2: 200.0,
                    gc: GraphicsContext {
                        col: Some("rgba(0,0,0,1)".into()),
                        fill: None,
                        lwd: 2.0,
                        lty: vec![4.0, 2.0],
                        lend: LineCap::Butt,
                        ljoin: LineJoin::Miter,
                        lmitre: 10.0,
                        font: FontContext::default(),
                        ext: None,
                    },
                },
                DrawingOp::Circle {
                    x: 50.0,
                    y: 50.0,
                    r: 25.0,
                    gc: GraphicsContext {
                        col: Some("rgba(255,0,0,1)".into()),
                        fill: Some("rgba(0,0,255,0.5)".into()),
                        lwd: 1.0,
                        lty: vec![],
                        lend: LineCap::Round,
                        ljoin: LineJoin::Round,
                        lmitre: 10.0,
                        font: FontContext::default(),
                        ext: None,
                    },
                },
                DrawingOp::Text {
                    x: 100.0,
                    y: 300.0,
                    str: "Hello, world!".into(),
                    rot: 45.0,
                    hadj: 0.0,
                    gc: GraphicsContext {
                        col: Some("rgba(0,0,0,1)".into()),
                        fill: None,
                        lwd: 1.0,
                        lty: vec![],
                        lend: LineCap::Round,
                        ljoin: LineJoin::Round,
                        lmitre: 10.0,
                        font: FontContext {
                            family: "serif".into(),
                            face: 2,
                            size: 14.0,
                            lineheight: 1.2,
                        },
                        ext: None,
                    },
                },
                DrawingOp::BeginGroup {
                    ext: Some(serde_json::json!({"opacity": 0.5})),
                },
                DrawingOp::EndGroup,
            ],
        },
        incremental: false,
        new_page: Some(true),
        resize_replay: None,
        plot_index: None,
        plot_number: None,
        ext: None,
    });
    roundtrip(&msg);
}

#[test]
fn deserialize_from_wire_format() {
    // Simulate JSON as it would appear on the wire from the R client.
    let json = r#"{"type":"ping"}"#;
    let msg: Message = serde_json::from_str(json).unwrap();
    assert_eq!(msg, Message::Ping);

    let json = r#"{"type":"metrics_response","id":1,"width":48.5,"ascent":10.2,"descent":2.8}"#;
    let msg: Message = serde_json::from_str(json).unwrap();
    assert!(matches!(
        msg,
        Message::MetricsResponse(MetricsResponse { id: 1, .. })
    ));
}

#[test]
fn drawing_op_tag() {
    // Verify the op tag is lowercase camelCase.
    let op = DrawingOp::BeginGroup { ext: None };
    let json = serde_json::to_string(&op).unwrap();
    assert!(json.contains(r#""op":"beginGroup""#), "got: {json}");

    let op = DrawingOp::EndGroup;
    let json = serde_json::to_string(&op).unwrap();
    assert!(json.contains(r#""op":"endGroup""#), "got: {json}");
}
