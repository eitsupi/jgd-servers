//! Transport layer, Hub, session management, and discovery for jgd.

pub mod discovery;
pub mod hub;
pub mod listener;
pub mod serve;
pub mod session;

#[cfg(test)]
mod testing;
