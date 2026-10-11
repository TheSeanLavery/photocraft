//! Platform-independent room protocol. Time and cryptographic identities come from the shell.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
pub type ApiResult<T> = Result<T, (u16, &'static str)>;
pub const MEMBER_TTL: u64 = 120_000;
const MAX_PEERS: usize = 64;
const MAX_QUEUE: usize = 128;
const MAX_STATE: usize = 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct Auth {
    pub code: String,
    pub peer: String,
    pub token: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Signal {
    pub from: String,
    pub to: String,
    pub sdp: String,
    pub offer: bool,
}
#[derive(Serialize, Deserialize)]
pub struct Member {
    token: String,
    seen: u64,
    signals: VecDeque<Signal>,
    #[serde(default)]
    pub ice: Option<(u64, Value)>,
}
#[derive(Serialize, Deserialize)]
pub struct Room {
    pub code: String,
    pub host: String,
    password: Option<String>,
    pub members: BTreeMap<String, Member>,
}
fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
fn password_hash(code: &str, password: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{code}:{password}").as_bytes()))
}
pub fn code(value: &str) -> ApiResult<String> {
    let value = value.trim().to_ascii_uppercase();
    if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err((400, "Invalid room code"));
    }
    Ok(value)
}
impl Room {
    pub fn create(code: String, password: Option<&str>, peer: String, token: String, now: u64) -> ApiResult<Self> {
        if password.is_some_and(|p| p.len() > 256) {
            return Err((400, "Password too long"));
        }
        Ok(Self {
            password: password.filter(|p| !p.is_empty()).map(|p| password_hash(&code, p)),
            code,
            host: peer.clone(),
            members: BTreeMap::from([(peer, Member { token, seen: now, signals: VecDeque::new(), ice: None })]),
        })
    }
    pub fn active(&self, now: u64) -> bool {
        self.members.get(&self.host).is_some_and(|m| now.saturating_sub(m.seen) < MEMBER_TTL)
    }
    pub fn admission(&self, peer: &str) -> ApiResult<Value> {
        let m = self.members.get(peer).ok_or((401, "Member unavailable"))?;
        Ok(json!({"code":self.code,"host":self.host,"peer":peer,"token":m.token}))
    }
    pub fn join(&mut self, password: Option<&str>, peer: String, token: String, now: u64) -> ApiResult<Value> {
        if !self.active(now) {
            return Err((404, "Host unavailable"));
        }
        let p = password.unwrap_or("");
        if p.len() > 256 || self.password.as_ref().is_some_and(|hash| !constant_eq(hash, &password_hash(&self.code, p))) {
            return Err((401, "Invalid room password"));
        }
        self.members.retain(|_, m| now.saturating_sub(m.seen) < MEMBER_TTL);
        if self.members.len() >= MAX_PEERS {
            return Err((409, "Room full"));
        }
        self.members.insert(peer.clone(), Member { token, seen: now, signals: VecDeque::new(), ice: None });
        self.admission(&peer)
    }
    pub fn authorize(&mut self, auth: &Auth, now: u64) -> ApiResult<()> {
        if auth.code != self.code || !self.active(now) {
            return Err((404, "Room unavailable"));
        }
        let m = self.members.get_mut(&auth.peer).ok_or((401, "Member unavailable"))?;
        if !constant_eq(&m.token, &auth.token) || now.saturating_sub(m.seen) >= MEMBER_TTL {
            return Err((401, "Invalid or expired member token"));
        }
        // Heartbeats need persistence at most every 30s, not every 150ms poll.
        if now.saturating_sub(m.seen) >= 30_000 {
            m.seen = now;
        }
        Ok(())
    }
    pub fn send(&mut self, auth: &Auth, signal: Signal, now: u64) -> ApiResult<()> {
        self.authorize(auth, now)?;
        if signal.sdp.len() > 64 * 1024 {
            return Err((413, "SDP too large"));
        }
        if signal.from != auth.peer || (auth.peer != self.host && signal.to != self.host) {
            return Err((403, "Signals must connect to host"));
        }
        let m = self.members.get_mut(&signal.to).ok_or((404, "Recipient unavailable"))?;
        if now.saturating_sub(m.seen) >= MEMBER_TTL {
            return Err((404, "Recipient unavailable"));
        }
        if m.signals.len() >= MAX_QUEUE {
            return Err((429, "Signal queue full"));
        }
        m.signals.push_back(signal);
        if self.encoded()?.len() > MAX_STATE {
            return Err((429, "Room signaling capacity reached"));
        }
        Ok(())
    }
    pub fn poll(&mut self, auth: &Auth, now: u64) -> ApiResult<Value> {
        self.authorize(auth, now)?;
        let m = self.members.get_mut(&auth.peer).ok_or((401, "Member unavailable"))?;
        // Match the browser's bounded 256KiB response reader, retaining later signals.
        let mut out = Vec::new();
        let mut size = 2usize;
        while let Some(s) = m.signals.front() {
            let n = serde_json::to_vec(s).map_err(|_| (500, "Signal encoding failed"))?.len().saturating_add(1);
            if size.saturating_add(n) > 240 * 1024 {
                break;
            }
            size = size.saturating_add(n);
            if let Some(s) = m.signals.pop_front() {
                out.push(s);
            }
        }
        serde_json::to_value(out).map_err(|_| (500, "Signal encoding failed"))
    }
    pub fn kick(&mut self, auth: &Auth, peer: &str, now: u64) -> ApiResult<()> {
        self.authorize(auth, now)?;
        if auth.peer != self.host || peer == self.host {
            return Err((403, "Only host can remove other members"));
        }
        self.members.remove(peer);
        Ok(())
    }
    pub fn leave(&mut self, auth: &Auth, now: u64) -> ApiResult<bool> {
        self.authorize(auth, now)?;
        if auth.peer == self.host {
            return Ok(true);
        }
        self.members.remove(&auth.peer);
        if let Some(m) = self.members.get_mut(&self.host)
            && m.signals.len() < MAX_QUEUE
        {
            m.signals.push_back(Signal { from: auth.peer.clone(), to: self.host.clone(), sdp: String::new(), offer: false });
        }
        Ok(false)
    }
    pub fn encoded(&self) -> ApiResult<String> {
        serde_json::to_string(self).map_err(|_| (500, "Room encoding failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn room() -> Room {
        Room::create("1234ABCD".into(), Some("pass"), "host".into(), "secret".into(), 0).unwrap()
    }
    fn auth(peer: &str, token: &str) -> Auth {
        Auth { code: "1234ABCD".into(), peer: peer.into(), token: token.into() }
    }
    #[test]
    fn password_tokens_expiry_and_kicks_are_enforced() {
        let mut r = room();
        assert!(r.join(Some("wrong"), "g".into(), "t".into(), 1).is_err());
        r.join(Some("pass"), "g".into(), "t".into(), 1).unwrap();
        assert!(r.authorize(&auth("g", "forged"), 1).is_err());
        assert!(r.kick(&auth("g", "t"), "host", 1).is_err());
        r.kick(&auth("host", "secret"), "g", 1).unwrap();
        assert!(r.authorize(&auth("g", "t"), 1).is_err());
        assert!(!r.active(MEMBER_TTL));
        assert!(r.authorize(&auth("host", "secret"), MEMBER_TTL).is_err());
    }
    #[test]
    fn storage_round_trip_preserves_queued_signals_and_auth() {
        let mut r = room();
        r.join(Some("pass"), "g".into(), "t".into(), 1).unwrap();
        let signal = Signal { from: "g".into(), to: "host".into(), sdp: "offer".into(), offer: true };
        r.send(&auth("g", "t"), signal.clone(), 1).unwrap();
        let mut restored: Room = serde_json::from_str(&r.encoded().unwrap()).unwrap();
        let mut forged = signal;
        forged.from = "host".into();
        assert!(restored.send(&auth("g", "t"), forged, 1).is_err());
        assert_eq!(restored.poll(&auth("host", "secret"), 1).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(restored.poll(&auth("host", "secret"), 1).unwrap(), json!([]));
    }
    #[test]
    fn leaving_notifies_host_and_host_leave_closes_room() {
        let mut r = room();
        r.join(Some("pass"), "g".into(), "t".into(), 1).unwrap();
        assert!(!r.leave(&auth("g", "t"), 1).unwrap());
        assert!(r.authorize(&auth("g", "t"), 1).is_err());
        assert_eq!(r.poll(&auth("host", "secret"), 1).unwrap()[0]["sdp"], "");
        assert!(r.leave(&auth("host", "secret"), 1).unwrap());
    }
    #[test]
    fn poll_is_bounded_without_dropping_remaining_messages() {
        let mut r = room();
        r.join(Some("pass"), "g".into(), "t".into(), 1).unwrap();
        for _ in 0..5 {
            r.send(&auth("g", "t"), Signal { from: "g".into(), to: "host".into(), sdp: "x".repeat(64 * 1024), offer: true }, 1).unwrap();
        }
        assert_eq!(r.poll(&auth("host", "secret"), 1).unwrap().as_array().unwrap().len(), 3);
        assert_eq!(r.poll(&auth("host", "secret"), 1).unwrap().as_array().unwrap().len(), 2);
        assert!(code("../../x").is_err());
        assert_eq!(code(" abcdef12 ").unwrap(), "ABCDEF12");
    }
}
