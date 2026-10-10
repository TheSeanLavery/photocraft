//! Only authenticated room members can receive temporary TURN credentials.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header},
    routing::{get, post},
};
use photocraft_collab::{
    signaling::{Auth, RoomService},
    transport::RTCIceServer,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};

type Error = (StatusCode, &'static str);
type Cache = Arc<Mutex<HashMap<(String, String), (Instant, IceResponse)>>>;
type IceReply = ([(header::HeaderName, &'static str); 1], Json<IceResponse>);
type Fallible<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const CREDENTIAL_TTL: u64 = 86400;
const CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Deserialize)]
pub struct TurnConfig {
    key_id: String,
    api_token: String,
}
impl TurnConfig {
    pub fn load(path: &str) -> Fallible<Self> {
        let bytes = std::fs::read(path)?;
        if bytes.len() > 4096 {
            return Err("TURN config too large".into());
        }
        let config: Self = serde_json::from_slice(&bytes)?;
        if config.key_id.len() != 32 || !config.key_id.bytes().all(|b| b.is_ascii_hexdigit()) || config.api_token.is_empty() {
            return Err("Invalid TURN configuration".into());
        }
        Ok(config)
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct IceResponse {
    #[serde(rename = "iceServers")]
    ice_servers: Vec<RTCIceServer>,
}
#[derive(Deserialize)]
struct ProviderServer {
    urls: Vec<String>,
    #[serde(default)]
    username: String,
    #[serde(default)]
    credential: String,
}
#[derive(Deserialize)]
struct ProviderResponse {
    #[serde(rename = "iceServers")]
    ice_servers: Vec<ProviderServer>,
}
#[derive(Clone)]
struct TurnService {
    rooms: RoomService,
    config: Arc<TurnConfig>,
    client: reqwest::Client,
    endpoint: String,
    cache: Cache,
    permits: Arc<Semaphore>,
}
pub fn router(rooms: RoomService, config: TurnConfig) -> Fallible<Router> {
    let endpoint = format!("https://rtc.live.cloudflare.com/v1/turn/keys/{}/credentials/generate-ice-servers", config.key_id);
    let turn = TurnService {
        rooms: rooms.clone(),
        config: Arc::new(config),
        client: reqwest::Client::builder().timeout(Duration::from_secs(15)).user_agent("PhotoCraft-dev").build()?,
        endpoint,
        cache: Default::default(),
        permits: Arc::new(Semaphore::new(8)),
    };
    Ok(photocraft_collab::signaling::router_with(rooms).merge(
        Router::new()
            .route("/ice", post(ice))
            .route("/health", get(|| async { "PhotoCraft room service ready" }))
            .layer(DefaultBodyLimit::max(4096))
            .with_state(turn),
    ))
}
fn normalize(response: ProviderResponse) -> Result<IceResponse, Error> {
    if response.ice_servers.is_empty() || response.ice_servers.len() > 16 {
        return Err((StatusCode::BAD_GATEWAY, "Invalid TURN response"));
    }
    let mut servers = Vec::new();
    for s in response.ice_servers {
        if s.urls.is_empty() || s.urls.len() > 16 || s.username.len() > 1024 || s.credential.len() > 1024 {
            return Err((StatusCode::BAD_GATEWAY, "Invalid TURN response"));
        }
        if s.urls.iter().any(|u| u.len() > 256 || !(u.starts_with("stun:") || u.starts_with("turn:") || u.starts_with("turns:"))) {
            return Err((StatusCode::BAD_GATEWAY, "Invalid TURN URLs"));
        }
        if s.urls.iter().any(|u| u.starts_with("turn:") || u.starts_with("turns:")) && (s.username.is_empty() || s.credential.is_empty()) {
            return Err((StatusCode::BAD_GATEWAY, "Missing TURN credentials"));
        }
        servers.push(RTCIceServer { urls: s.urls, username: s.username, credential: s.credential });
    }
    Ok(IceResponse { ice_servers: servers })
}
async fn ice(State(s): State<TurnService>, Json(auth): Json<Auth>) -> Result<IceReply, Error> {
    s.rooms.authorize(&auth).await?;
    let key = (auth.code.clone(), auth.peer.clone());
    let _permit = s.permits.try_acquire().map_err(|_| (StatusCode::TOO_MANY_REQUESTS, "TURN service busy"))?;
    // Serialize generation to make repeated requests idempotent and bound provider usage.
    let mut cache = s.cache.lock().await;
    cache.retain(|_, (created, _)| created.elapsed() < CACHE_TTL);
    if let Some((_, response)) = cache.get(&key) {
        return Ok(([(header::CACHE_CONTROL, "no-store")], Json(response.clone())));
    }
    if cache.len() >= 16384 {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "TURN capacity reached"));
    }
    let mut response = s
        .client
        .post(&s.endpoint)
        .bearer_auth(&s.config.api_token)
        .json(&serde_json::json!({"ttl": CREDENTIAL_TTL}))
        .send()
        .await
        .map_err(|_| (StatusCode::BAD_GATEWAY, "TURN provider unavailable"))?;
    if !response.status().is_success() {
        return Err((StatusCode::BAD_GATEWAY, "TURN provider rejected credentials"));
    }
    if response.content_length().is_some_and(|n| n > 65536) {
        return Err((StatusCode::BAD_GATEWAY, "TURN response too large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| (StatusCode::BAD_GATEWAY, "TURN provider response failed"))? {
        if chunk.len() > 65536usize.saturating_sub(body.len()) {
            return Err((StatusCode::BAD_GATEWAY, "TURN response too large"));
        }
        body.extend_from_slice(&chunk);
    }
    let provider = serde_json::from_slice(&body).map_err(|_| (StatusCode::BAD_GATEWAY, "Invalid TURN response"))?;
    let response = normalize(provider)?;
    s.rooms.authorize(&auth).await?;
    cache.insert(key, (Instant::now(), response.clone()));
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(response)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_collab::signaling::{RoomRequest, SendSignal, Signal};
    #[tokio::test]
    async fn credentials_are_cached_per_member_and_invalid_members_never_reach_provider() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
        let provider = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/generate", post(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    Json(serde_json::json!({"iceServers":[{"urls":["turn:turn.cloudflare.com:3478?transport=udp"],"username":"user","credential":"temporary"}]}))
                }
            }))).await.unwrap();
        });
        let rooms = RoomService::default();
        let host = rooms.create(RoomRequest { code: None, password: None }).await.unwrap();
        let state = TurnService {
            rooms: rooms.clone(),
            config: Arc::new(TurnConfig { key_id: "test".into(), api_token: "test".into() }),
            client: reqwest::Client::new(),
            endpoint,
            cache: Default::default(),
            permits: Arc::new(Semaphore::new(8)),
        };
        let mut forged = Auth::from(&host);
        forged.token = "invalid".into();
        assert!(ice(State(state.clone()), Json(forged)).await.is_err());
        assert_eq!(count.load(Ordering::Relaxed), 0);
        let first = ice(State(state.clone()), Json(Auth::from(&host))).await.unwrap();
        let second = ice(State(state.clone()), Json(Auth::from(&host))).await.unwrap();
        assert_eq!(first.1.0.ice_servers, second.1.0.ice_servers);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        rooms.leave(Auth::from(&host)).await.unwrap();
        assert!(ice(State(state), Json(Auth::from(&host))).await.is_err());
        assert_eq!(count.load(Ordering::Relaxed), 1);
        provider.abort();
    }
    #[test]
    fn cloudflare_stun_without_credentials_is_normalized_for_native_webrtc() {
        let provider = serde_json::from_value(serde_json::json!({"iceServers":[{"urls":["stun:stun.cloudflare.com:3478"]},{"urls":["turn:turn.cloudflare.com:3478?transport=udp"],"username":"user","credential":"short-lived"}]})).unwrap();
        let response = normalize(provider).unwrap();
        assert!(response.ice_servers.first().unwrap().username.is_empty());
        let encoded = serde_json::to_vec(&response).unwrap();
        let parsed: IceResponse = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(parsed.ice_servers.len(), 2);
        let bad = serde_json::from_value(serde_json::json!({"iceServers":[{"urls":["turn:turn.cloudflare.com:3478"]}]})).unwrap();
        assert!(normalize(bad).is_err());
    }
    #[tokio::test]
    async fn authorization_preserves_signals_and_rejects_forged_and_departed_members() {
        let rooms = RoomService::default();
        let host = rooms.create(RoomRequest { code: None, password: Some("test".into()) }).await.unwrap();
        let guest = rooms.join(RoomRequest { code: Some(host.code.clone()), password: Some("test".into()) }).await.unwrap();
        rooms
            .send(SendSignal {
                auth: Auth::from(&guest),
                signal: Signal { from: guest.peer.clone(), to: host.peer.clone(), sdp: "queued".into(), offer: true },
            })
            .await
            .unwrap();
        rooms.authorize(&Auth::from(&host)).await.unwrap();
        assert_eq!(rooms.poll(Auth::from(&host)).await.unwrap().len(), 1);
        let mut forged = Auth::from(&guest);
        forged.token = "wrong".into();
        assert!(rooms.authorize(&forged).await.is_err());
        rooms.leave(Auth::from(&guest)).await.unwrap();
        assert!(rooms.authorize(&Auth::from(&guest)).await.is_err());
    }
}
