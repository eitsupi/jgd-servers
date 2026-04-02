//! Transport layer, Hub, session management, discovery, REST API, and WebSocket relay for jgd.

pub mod api;
pub mod discovery;
pub mod hub;
pub mod listener;
pub mod serve;
pub mod session;
pub mod web_assets;
pub mod ws;

#[cfg(test)]
mod testing;
