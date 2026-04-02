//! Transport layer, Hub, session management, discovery, and REST API for jgd.

pub mod api;
pub mod discovery;
pub mod hub;
pub mod listener;
pub mod serve;
pub mod session;

#[cfg(test)]
mod testing;
