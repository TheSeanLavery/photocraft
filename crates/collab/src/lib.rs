//! Typed, bounded host-authoritative collaborative drawing protocol.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
pub mod delta;
pub mod model;
pub mod wire;
pub use model::*;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod signaling;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod transport;
