//! JSONL (JSON Lines) codec for tokio-util.

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::message::Message;

/// Error type for codec operations.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("line too long ({len} bytes, max {max})")]
    LineTooLong { len: usize, max: usize },
}

/// JSONL codec: each line is one JSON message terminated by `\n`.
#[derive(Debug)]
pub struct JsonLinesCodec {
    max_line_length: usize,
}

impl JsonLinesCodec {
    /// Default maximum line length (8 MiB) — raster images can be large.
    const DEFAULT_MAX_LINE_LENGTH: usize = 8 * 1024 * 1024;

    pub fn new() -> Self {
        Self {
            max_line_length: Self::DEFAULT_MAX_LINE_LENGTH,
        }
    }

    /// Create a codec with a custom max line length.
    pub fn with_max_line_length(max_line_length: usize) -> Self {
        Self { max_line_length }
    }
}

impl Default for JsonLinesCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for JsonLinesCodec {
    type Item = Message;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let newline_pos = src.iter().position(|&b| b == b'\n');

        match newline_pos {
            Some(pos) => {
                if pos > self.max_line_length {
                    // Discard the oversized line.
                    src.advance(pos + 1);
                    return Err(CodecError::LineTooLong {
                        len: pos,
                        max: self.max_line_length,
                    });
                }

                let line = &src[..pos];
                let msg = serde_json::from_slice(line)?;
                src.advance(pos + 1);
                Ok(Some(msg))
            }
            None => {
                if src.len() > self.max_line_length {
                    let len = src.len();
                    src.clear();
                    return Err(CodecError::LineTooLong {
                        len,
                        max: self.max_line_length,
                    });
                }
                Ok(None)
            }
        }
    }
}

impl Encoder<Message> for JsonLinesCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let json = serde_json::to_string(&item)?;
        dst.reserve(json.len() + 1);
        dst.put_slice(json.as_bytes());
        dst.put_u8(b'\n');
        Ok(())
    }
}
