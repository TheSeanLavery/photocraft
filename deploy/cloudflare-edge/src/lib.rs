//! Rust-only Cloudflare hosting: R2 editor assets and one SQLite Durable Object per room.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#[cfg(any(test, target_arch = "wasm32"))]
mod room;
#[cfg(target_arch = "wasm32")]
mod runtime;
#[cfg(target_arch = "wasm32")]
pub use runtime::PhotoCraftRoom;
#[cfg(any(test, target_arch = "wasm32"))]
fn room_route(path: &str) -> bool {
    matches!(path, "/create" | "/join" | "/signal" | "/poll" | "/leave" | "/kick" | "/ice")
}
#[cfg(any(test, target_arch = "wasm32"))]
fn asset_key(path: &str) -> Option<String> {
    if path == "/" {
        return Some("index.html".into());
    }
    let (path, prefix) = if let Some(path) = path.strip_prefix("/app/") { (path, "room-panel/") } else { (path.strip_prefix('/')?, "") };
    if path.is_empty() {
        return Some(format!("{prefix}index.html"));
    }
    let key = path;
    if key.is_empty()
        || key.contains('/')
        || key.contains('%')
        || key.contains("..")
        || !key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    let ext = key.rsplit_once('.')?.1;
    matches!(ext, "html" | "wasm" | "js" | "css" | "png" | "svg" | "ico" | "json").then(|| format!("{prefix}{key}"))
}
#[cfg(any(test, target_arch = "wasm32"))]
fn join_destination(path: &str) -> Option<String> {
    let code = path.strip_prefix("/join/")?;
    if !(4..=12).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!("/app/?room={}", code.to_ascii_uppercase()))
}
#[cfg(any(test, target_arch = "wasm32"))]
fn normalize_ice(mut v: serde_json::Value) -> Result<serde_json::Value, &'static str> {
    let servers = v.get_mut("iceServers").and_then(serde_json::Value::as_array_mut).ok_or("Invalid TURN response")?;
    if servers.is_empty() || servers.len() > 16 {
        return Err("Invalid TURN response");
    }
    for server in servers {
        if !server.is_object() {
            return Err("Invalid TURN server");
        }
        let urls = server.get("urls").and_then(serde_json::Value::as_array).ok_or("Invalid TURN URLs")?;
        if urls.is_empty()
            || urls.len() > 16
            || urls.iter().any(|v| v.as_str().is_none_or(|s| s.len() > 256 || !(s.starts_with("stun:") || s.starts_with("turn:") || s.starts_with("turns:"))))
        {
            return Err("Invalid TURN URLs");
        }
        let turn = urls.iter().any(|v| v.as_str().is_some_and(|s| !s.starts_with("stun:")));
        for k in ["username", "credential"] {
            if server.get(k).is_none() {
                server[k] = serde_json::json!("");
            }
            if server.get(k).and_then(serde_json::Value::as_str).is_none_or(|s| s.len() > 1024 || (turn && s.is_empty())) {
                return Err("Invalid TURN credentials");
            }
        }
    }
    Ok(v)
}
#[cfg(target_arch = "wasm32")]
#[worker::event(fetch)]
async fn main(req: worker::Request, env: worker::Env, _ctx: worker::Context) -> worker::Result<worker::Response> {
    runtime::fetch(req, env).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assets_cannot_expose_private_or_traversed_paths() {
        assert_eq!(asset_key("/"), Some("index.html".into()));
        assert!(asset_key("/app-hash_bg.wasm").is_some());
        for path in ["/../secret.json", "//private.json", "/private/token.json", "/secret.txt", "/%2e%2e.json"] {
            assert!(asset_key(path).is_none());
        }
        assert!(!room_route("/engine.execute"));
    }
    #[test]
    fn room_links_and_prefixed_assets_are_bounded() {
        assert_eq!(join_destination("/join/deadBEEF").as_deref(), Some("/app/?room=DEADBEEF"));
        assert_eq!(asset_key("/app/"), Some("room-panel/index.html".into()));
        assert_eq!(asset_key("/app/editor-hash_bg.wasm"), Some("room-panel/editor-hash_bg.wasm".into()));
        for bad in ["/join/", "/join/../../private", "/join/DEADBEEF/extra", "/join/DEAD%0A1", "/join/日本語", "/join/TOOLONGROOMCODE"] {
            assert!(join_destination(bad).is_none());
        }
        for bad in ["/app/../private.json", "/app//secret.json", "/app/a/b.json"] {
            assert!(asset_key(bad).is_none());
        }
    }
    #[test]
    fn stun_defaults_are_compatible_and_turn_requires_credentials() {
        let v = normalize_ice(serde_json::json!({"iceServers":[{"urls":["stun:stun.cloudflare.com:3478"]}]})).unwrap();
        assert_eq!(v["iceServers"][0]["username"], "");
        assert!(normalize_ice(serde_json::json!({"iceServers":[{"urls":["turn:turn.cloudflare.com:3478"]}]})).is_err());
    }
}
