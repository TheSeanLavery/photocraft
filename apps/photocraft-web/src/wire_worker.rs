#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#[cfg(target_arch = "wasm32")]
mod worker_codec;
#[cfg(target_arch = "wasm32")]
fn main() {
    if let Err(error) = worker_codec::run() {
        log::error!("PhotoCraft codec worker: {error}");
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
