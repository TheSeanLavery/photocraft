#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
mod turn;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind = std::env::var("PHOTOCRAFT_ROOM_BIND").unwrap_or_else(|_| "127.0.0.1:5548".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("PhotoCraft room signaling listening on {bind}");
    let service = photocraft_collab::signaling::RoomService::default();
    let mut router = if let Ok(path) = std::env::var("PHOTOCRAFT_TURN_CONFIG") {
        turn::router(service, turn::TurnConfig::load(&path)?)?
    } else {
        photocraft_collab::signaling::router_with(service)
    };
    if let Ok(path) = std::env::var("PHOTOCRAFT_PLAYTEST_DOWNLOADS") {
        router = router.nest_service("/downloads", tower_http::services::ServeDir::new(path));
    }
    router = router.route("/", axum::routing::get(|| async {
        "PhotoCraft multiplayer playtest. Download /downloads/PhotoCraft-Playtest-AppleSilicon.zip, open PhotoCraft, choose Window > Collaboration, then create or join a room. Use Discord for voice. The service is hosted on the operator's Mac through Cloudflare Tunnel."
    }));
    axum::serve(listener, router).await?;
    Ok(())
}
