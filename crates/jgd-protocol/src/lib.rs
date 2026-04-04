//! Protocol types, serde definitions, and JSONL codec for jgd.

pub mod addr;
pub mod codec;
pub mod gc;
pub mod message;
pub mod ops;

pub use addr::SocketAddr;
pub use codec::JsonLinesCodec;
pub use gc::{FontContext, GraphicsContext, LineCap, LineJoin};
pub use message::{
    DeviceInfo, FrameMessage, Message, MetricsKind, MetricsRequest, MetricsResponse, Plot,
    ResizeMessage, ServerInfoMessage, Transport,
};
pub use ops::{DrawingOp, FillRule};
