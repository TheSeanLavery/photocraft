#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind = std::env::var("PHOTOCRAFT_ROOM_BIND").unwrap_or_else(|_| "127.0.0.1:5548".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("PhotoCraft room signaling listening on {bind}");
    axum::serve(listener, photocraft_collab::signaling::router()).await?;
    Ok(())
}
