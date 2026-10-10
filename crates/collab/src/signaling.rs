//! Admission and SDP routing only. Canvas operations flow directly over WebRTC.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::post,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
const MAX_ROOMS: usize = 256;
const MAX_PEERS: usize = 64;
const MAX_SIGNALS: usize = 128;
const MAX_SDP: usize = 64 * 1024;
const TTL: Duration = Duration::from_secs(3600);
const MEMBER_TTL: Duration = Duration::from_secs(120);
type ApiError = (StatusCode, &'static str);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Admission {
    pub code: String,
    pub peer: String,
    pub token: String,
    pub host: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    pub from: String,
    pub to: String,
    pub sdp: String,
    pub offer: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomRequest {
    pub code: Option<String>,
    pub password: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Auth {
    pub code: String,
    pub peer: String,
    pub token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kick {
    pub auth: Auth,
    pub peer: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendSignal {
    pub auth: Auth,
    pub signal: Signal,
}
struct Member {
    token: String,
    signals: VecDeque<Signal>,
    last_seen: Instant,
}
struct Room {
    password: Option<[u8; 32]>,
    host: String,
    members: HashMap<String, Member>,
    touched: Instant,
}
#[derive(Clone, Default)]
pub struct RoomService {
    rooms: Arc<Mutex<HashMap<String, Room>>>,
}
fn random_token() -> String {
    let mut rng = rand::rng();
    (0..32).map(|_| format!("{:02x}", rng.random::<u8>())).collect()
}
fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.as_bytes().iter().zip(b.as_bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
fn password_hash(code: &str, password: &str) -> [u8; 32] {
    Sha256::digest(format!("{code}:{password}").as_bytes()).into()
}
impl RoomService {
    /// Validate active room membership without draining the signaling queue.
    pub async fn authorize(&self, auth: &Auth) -> Result<(), ApiError> {
        let mut rooms = self.rooms.lock().await;
        authenticated(&mut rooms, auth)?;
        Ok(())
    }

    pub async fn create(&self, request: RoomRequest) -> Result<Admission, ApiError> {
        if request.password.as_ref().is_some_and(|p| p.len() > 256) {
            return Err((StatusCode::BAD_REQUEST, "password too long"));
        }
        let mut rooms = self.rooms.lock().await;
        rooms.retain(|_, room| room.touched.elapsed() < TTL && room.members.get(&room.host).is_some_and(|host| host.last_seen.elapsed() < MEMBER_TTL));
        if rooms.len() >= MAX_ROOMS {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "room capacity reached"));
        }
        let code = loop {
            let code = format!("{:08X}", rand::rng().random::<u32>());
            if !rooms.contains_key(&code) {
                break code;
            }
        };
        let peer = random_token();
        let token = random_token();
        let admission = Admission { code: code.clone(), peer: peer.clone(), token: token.clone(), host: peer.clone() };
        rooms.insert(
            code.clone(),
            Room {
                password: request.password.filter(|p| !p.is_empty()).map(|p| password_hash(&code, &p)),
                host: peer.clone(),
                members: HashMap::from([(peer, Member { token, signals: VecDeque::new(), last_seen: Instant::now() })]),
                touched: Instant::now(),
            },
        );
        Ok(admission)
    }
    pub async fn join(&self, request: RoomRequest) -> Result<Admission, ApiError> {
        let code = request.code.ok_or((StatusCode::BAD_REQUEST, "room code required"))?.trim().to_uppercase();
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(&code).filter(|r| r.touched.elapsed() < TTL).ok_or((StatusCode::NOT_FOUND, "room unavailable"))?;
        if room.members.get(&room.host).is_none_or(|host| host.last_seen.elapsed() >= MEMBER_TTL) {
            return Err((StatusCode::NOT_FOUND, "host unavailable"));
        }
        let provided = request.password.as_deref().unwrap_or("");
        if provided.len() > 256
            || room.password.is_some_and(|expected| {
                let actual = password_hash(&code, provided);
                expected.iter().zip(actual).fold(0u8, |diff, (a, b)| diff | (a ^ b)) != 0
            })
        {
            return Err((StatusCode::UNAUTHORIZED, "invalid room password"));
        }
        room.members.retain(|peer, member| peer == &room.host || member.last_seen.elapsed() < MEMBER_TTL);
        if room.members.len() >= MAX_PEERS {
            return Err((StatusCode::CONFLICT, "room full"));
        }
        let peer = random_token();
        let token = random_token();
        room.members.insert(peer.clone(), Member { token: token.clone(), signals: VecDeque::new(), last_seen: Instant::now() });
        room.touched = Instant::now();
        Ok(Admission { code, peer, token, host: room.host.clone() })
    }
    pub async fn send(&self, request: SendSignal) -> Result<(), ApiError> {
        if request.signal.sdp.len() > MAX_SDP {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, "SDP too large"));
        }
        let mut rooms = self.rooms.lock().await;
        let room = authenticated(&mut rooms, &request.auth)?;
        if request.signal.from != request.auth.peer || (request.auth.peer != room.host && request.signal.to != room.host) {
            return Err((StatusCode::FORBIDDEN, "signals must connect to host"));
        }
        let recipient = room.members.get_mut(&request.signal.to).ok_or((StatusCode::NOT_FOUND, "recipient unavailable"))?;
        if recipient.signals.len() >= MAX_SIGNALS {
            return Err((StatusCode::TOO_MANY_REQUESTS, "signal queue full"));
        }
        recipient.signals.push_back(request.signal);
        Ok(())
    }
    pub async fn poll(&self, auth: Auth) -> Result<Vec<Signal>, ApiError> {
        let mut rooms = self.rooms.lock().await;
        let room = authenticated(&mut rooms, &auth)?;
        Ok(room.members.get_mut(&auth.peer).ok_or((StatusCode::UNAUTHORIZED, "member unavailable"))?.signals.drain(..).collect())
    }
    pub async fn kick(&self, request: Kick) -> Result<(), ApiError> {
        let mut rooms = self.rooms.lock().await;
        let room = authenticated(&mut rooms, &request.auth)?;
        if request.auth.peer != room.host || request.peer == room.host {
            return Err((StatusCode::FORBIDDEN, "only host can remove other members"));
        }
        room.members.remove(&request.peer);
        Ok(())
    }
    pub async fn leave(&self, auth: Auth) -> Result<(), ApiError> {
        let mut rooms = self.rooms.lock().await;
        let room = authenticated(&mut rooms, &auth)?;
        if auth.peer == room.host {
            rooms.remove(&auth.code);
        } else {
            room.members.remove(&auth.peer);
            if let Some(host) = room.members.get_mut(&room.host)
                && host.signals.len() < MAX_SIGNALS
            {
                host.signals.push_back(Signal { from: auth.peer.clone(), to: room.host.clone(), sdp: String::new(), offer: false });
            }
        }
        Ok(())
    }
}
fn authenticated<'a>(rooms: &'a mut HashMap<String, Room>, auth: &Auth) -> Result<&'a mut Room, ApiError> {
    let room = rooms.get_mut(&auth.code).filter(|r| r.touched.elapsed() < TTL).ok_or((StatusCode::NOT_FOUND, "room unavailable"))?;
    if !room.members.get(&auth.peer).is_some_and(|m| constant_eq(&m.token, &auth.token)) {
        return Err((StatusCode::UNAUTHORIZED, "invalid member token"));
    }
    if room.members.get(&room.host).is_none_or(|host| host.last_seen.elapsed() >= MEMBER_TTL) {
        return Err((StatusCode::NOT_FOUND, "host unavailable"));
    }
    if let Some(member) = room.members.get_mut(&auth.peer) {
        member.last_seen = Instant::now();
    }
    room.touched = Instant::now();
    Ok(room)
}
pub fn router() -> Router {
    router_with(RoomService::default())
}
pub fn router_with(service: RoomService) -> Router {
    Router::new()
        .route("/create", post(create))
        .route("/join", post(join))
        .route("/signal", post(signal))
        .route("/poll", post(poll))
        .route("/leave", post(leave))
        .route("/kick", post(kick))
        .layer(DefaultBodyLimit::max(96 * 1024))
        .with_state(service)
}
async fn create(State(s): State<RoomService>, Json(r): Json<RoomRequest>) -> Result<Json<Admission>, ApiError> {
    s.create(r).await.map(Json)
}
async fn join(State(s): State<RoomService>, Json(r): Json<RoomRequest>) -> Result<Json<Admission>, ApiError> {
    s.join(r).await.map(Json)
}
async fn signal(State(s): State<RoomService>, Json(r): Json<SendSignal>) -> Result<StatusCode, ApiError> {
    s.send(r).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn poll(State(s): State<RoomService>, Json(r): Json<Auth>) -> Result<Json<Vec<Signal>>, ApiError> {
    s.poll(r).await.map(Json)
}
async fn leave(State(s): State<RoomService>, Json(r): Json<Auth>) -> Result<StatusCode, ApiError> {
    s.leave(r).await?;
    Ok(StatusCode::NO_CONTENT)
}
impl From<&Admission> for Auth {
    fn from(a: &Admission) -> Self {
        Self { code: a.code.clone(), peer: a.peer.clone(), token: a.token.clone() }
    }
}

async fn kick(State(s): State<RoomService>, Json(r): Json<Kick>) -> Result<StatusCode, ApiError> {
    s.kick(r).await?;
    Ok(StatusCode::NO_CONTENT)
}
