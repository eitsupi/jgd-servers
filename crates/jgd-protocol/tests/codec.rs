use bytes::BytesMut;
use jgd_protocol::*;
use jgd_protocol::codec::JsonLinesCodec;
use tokio_util::codec::{Decoder, Encoder};

#[test]
fn decode_single_line() {
    let mut codec = JsonLinesCodec::new();
    let mut buf = BytesMut::from("{\"type\":\"ping\"}\n");
    let msg = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(msg, Message::Ping);
    assert!(buf.is_empty());
}

#[test]
fn decode_partial_then_complete() {
    let mut codec = JsonLinesCodec::new();
    let mut buf = BytesMut::from("{\"type\":");

    // Incomplete line — should return None.
    assert!(codec.decode(&mut buf).unwrap().is_none());

    // Append the rest.
    buf.extend_from_slice(b"\"pong\"}\n");
    let msg = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(msg, Message::Pong);
}

#[test]
fn decode_two_messages() {
    let mut codec = JsonLinesCodec::new();
    let mut buf = BytesMut::from("{\"type\":\"ping\"}\n{\"type\":\"close\"}\n");

    let msg1 = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(msg1, Message::Ping);

    let msg2 = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(msg2, Message::Close);

    assert!(codec.decode(&mut buf).unwrap().is_none());
}

#[test]
fn encode_appends_newline() {
    let mut codec = JsonLinesCodec::new();
    let mut buf = BytesMut::new();
    codec.encode(Message::Ping, &mut buf).unwrap();

    let s = std::str::from_utf8(&buf).unwrap();
    assert!(s.ends_with('\n'));
    assert!(s.starts_with('{'));

    // The encoded line should be decodable.
    let msg = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(msg, Message::Ping);
}

#[test]
fn line_too_long_error() {
    let mut codec = JsonLinesCodec::with_max_line_length(16);
    let mut buf = BytesMut::from("{\"type\":\"ping\",\"extra\":\"padding\"}\n");

    let err = codec.decode(&mut buf).unwrap_err();
    assert!(
        err.to_string().contains("line too long"),
        "expected LineTooLong error, got: {err}"
    );
}

#[test]
fn encode_decode_roundtrip() {
    let mut codec = JsonLinesCodec::new();
    let original = Message::MetricsResponse(MetricsResponse {
        id: 99,
        width: 42.5,
        ascent: 11.0,
        descent: 3.0,
    });

    let mut buf = BytesMut::new();
    codec.encode(original.clone(), &mut buf).unwrap();
    let decoded = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(original, decoded);
}
