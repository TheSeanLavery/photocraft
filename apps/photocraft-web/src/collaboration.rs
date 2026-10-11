//! Browser WebRTC bridge using the same bounded wire protocol and host authority as native.
use photocraft_collab::{
    Operation, Presence, Profile, RoomState,
    wire::{self, Decoder, Wire},
};
use photocraft_engine::collab::{Collaboration, NetworkIntent};
use photocraft_ui_egui::PhotocraftApp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet, VecDeque},
    rc::Rc,
    sync::Arc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{RtcConfiguration, RtcDataChannel, RtcDataChannelInit, RtcDataChannelState, RtcPeerConnection, RtcSdpType, RtcSessionDescriptionInit};
const MAX_EVENTS: usize = 1024;
const MAX_QUEUE_BYTES: usize = wire::MAX_TRANSFER;
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelKind {
    Edits,
    Presence,
    Voice,
}
#[derive(Clone, Deserialize, Serialize)]
struct Admission {
    code: String,
    peer: String,
    token: String,
    host: String,
}
impl Admission {
    fn auth(&self) -> Value {
        json!({"code":self.code,"peer":self.peer,"token":self.token})
    }
}
#[derive(Deserialize)]
struct Signal {
    from: String,
    to: String,
    sdp: String,
    offer: bool,
}
type Handlers = Rc<RefCell<Vec<Closure<dyn FnMut(web_sys::Event)>>>>;
struct Link {
    connection: RtcPeerConnection,
    channels: Rc<RefCell<BTreeMap<String, RtcDataChannel>>>,
    handlers: Handlers,
}
impl Drop for Link {
    fn drop(&mut self) {
        self.connection.set_ondatachannel(None);
        for channel in self.channels.borrow().values() {
            channel.set_onopen(None);
            channel.set_onclose(None);
            channel.set_onmessage(None);
            channel.close();
        }
        self.connection.close();
    }
}
enum Event {
    Joined(Admission),
    Packet(String, ChannelKind, Vec<u8>),
    Connected(String),
    Disconnected(String),
    Error(String),
    PeerError(String, String),
}
struct Transport {
    generation: u64,
    links: BTreeMap<String, Link>,
    events: VecDeque<Event>,
}
type Shared = Rc<RefCell<Transport>>;
struct Job {
    peer: String,
    id: u64,
    bytes: Arc<Vec<u8>>,
    index: usize,
}
struct Bridge {
    shared: Shared,
    admission: Option<Admission>,
    connected: HashSet<String>,
    decoder: Decoder,
    outgoing: VecDeque<Job>,
    transfer_id: u64,
    queued_bytes: usize,
    pending_bootstraps: Vec<String>,
    bootstrapped: bool,
}
fn clock_ms() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map_or_else(js_sys::Date::now, |p| p.now())
}
fn js_error(e: JsValue) -> String {
    format!("Browser connection: {e:?}")
}
async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}
struct FetchTimeout {
    window: web_sys::Window,
    id: i32,
    _callback: Closure<dyn FnMut()>,
}
impl Drop for FetchTimeout {
    fn drop(&mut self) {
        self.window.clear_timeout_with_handle(self.id);
    }
}
async fn post(server: &str, path: &str, body: &Value) -> Result<Value, String> {
    let controller = web_sys::AbortController::new().map_err(js_error)?;
    let abort = controller.clone();
    let callback = Closure::<dyn FnMut()>::new(move || abort.abort());
    let window = web_sys::window().ok_or("No browser window")?;
    let id = window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), 20_000).map_err(js_error)?;
    let _timeout = FetchTimeout { window: window.clone(), id, _callback: callback };
    let init = web_sys::RequestInit::new();
    init.set_signal(Some(&controller.signal()));
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body.to_string()));
    let request = web_sys::Request::new_with_str_and_init(&format!("{}{path}", server.trim_end_matches('/')), &init).map_err(js_error)?;
    request.headers().set("Content-Type", "application/json").map_err(js_error)?;
    let window = web_sys::window().ok_or("No browser window")?;
    let response = JsFuture::from(window.fetch_with_request(&request)).await.map_err(js_error)?.dyn_into::<web_sys::Response>().map_err(js_error)?;
    let limit = if path == "/poll" { 8 * 1024 * 1024 } else { 96 * 1024 };
    if response.headers().get("Content-Length").ok().flatten().and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| n > limit) {
        return Err("Room service response too large".into());
    }
    let mut bytes = Vec::new();
    if let Some(body) = response.body() {
        let reader = body.get_reader().dyn_into::<web_sys::ReadableStreamDefaultReader>().map_err(|e| js_error(e.into()))?;
        loop {
            let chunk = JsFuture::from(reader.read()).await.map_err(js_error)?;
            if js_sys::Reflect::get(&chunk, &JsValue::from_str("done")).map_err(js_error)?.as_bool() == Some(true) {
                break;
            }
            let data = js_sys::Uint8Array::new(&js_sys::Reflect::get(&chunk, &JsValue::from_str("value")).map_err(js_error)?);
            if data.length() as usize > limit.saturating_sub(bytes.len()) {
                let _ = reader.cancel();
                reader.release_lock();
                return Err("Room service response too large".into());
            }
            bytes.extend_from_slice(&data.to_vec());
        }
        reader.release_lock();
    }
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!("Room service {}: {}", response.status(), text.chars().take(256).collect::<String>()));
    }
    if text.len() > limit {
        return Err("Room service response too large".into());
    }
    if text.is_empty() { Ok(Value::Null) } else { serde_json::from_str(&text).map_err(|e| e.to_string()) }
}
fn event(shared: &Shared, generation: u64, e: Event, ctx: &egui::Context) {
    let mut state = shared.borrow_mut();
    if state.generation != generation {
        return;
    }
    if state.events.len() >= MAX_EVENTS {
        state.events.clear();
        state.events.push_back(Event::Error("Connection receive queue exceeded; rejoin to resynchronize".into()));
    } else {
        state.events.push_back(e);
    }
    ctx.request_repaint();
}
fn attach_channel(
    channel: RtcDataChannel,
    peer: String,
    shared: Shared,
    generation: u64,
    ctx: egui::Context,
    channels: Rc<RefCell<BTreeMap<String, RtcDataChannel>>>,
    handlers: Handlers,
) {
    let kind = match channel.label().as_str() {
        "edits" => ChannelKind::Edits,
        "presence" => ChannelKind::Presence,
        "voice" => ChannelKind::Voice,
        _ => {
            channel.close();
            return;
        }
    };
    if channels.borrow().contains_key(&channel.label()) {
        channel.close();
        return;
    }
    channel.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);
    channels.borrow_mut().insert(channel.label(), channel.clone());
    let sh = Rc::downgrade(&shared);
    let pe = peer.clone();
    let cx = ctx.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if kind == ChannelKind::Edits
            && let Some(sh) = sh.upgrade()
        {
            event(&sh, generation, Event::Connected(pe.clone()), &cx);
        }
    });
    channel.set_onopen(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
    let sh = Rc::downgrade(&shared);
    let pe = peer.clone();
    let cx = ctx.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        let Some(sh) = sh.upgrade() else {
            return;
        };
        let Ok(e) = e.dyn_into::<web_sys::MessageEvent>() else {
            return;
        };
        let data = e.data();
        if !data.is_instance_of::<js_sys::ArrayBuffer>() {
            event(&sh, generation, Event::PeerError(pe.clone(), "Expected binary collaboration packet".into()), &cx);
            return;
        }
        let array = js_sys::Uint8Array::new(&data);
        if array.length() as usize > wire::MAX_PACKET {
            event(&sh, generation, Event::PeerError(pe.clone(), "Collaboration packet too large".into()), &cx);
            return;
        }
        let bytes = array.to_vec();
        event(&sh, generation, Event::Packet(pe.clone(), kind, bytes), &cx);
    });
    channel.set_onmessage(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
    let weak = Rc::downgrade(&shared);
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if kind == ChannelKind::Edits
            && let Some(shared) = weak.upgrade()
        {
            event(&shared, generation, Event::Disconnected(peer.clone()), &ctx);
        }
    });
    channel.set_onclose(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
}
fn new_link(peer: &str, ice: &Value, shared: &Shared, generation: u64, ctx: &egui::Context) -> Result<RtcPeerConnection, String> {
    if shared.borrow().generation != generation {
        return Err("Room changed during negotiation".into());
    }
    let config = RtcConfiguration::new();
    let parsed = js_sys::JSON::parse(&ice.to_string()).map_err(js_error)?;
    config.set_ice_servers(&parsed);
    let pc = RtcPeerConnection::new_with_configuration(&config).map_err(js_error)?;
    if shared.borrow().links.len() >= 64 {
        return Err("Too many room peers".into());
    }
    let channels = Rc::new(RefCell::new(BTreeMap::new()));
    let handlers: Handlers = Rc::default();
    let sh = Rc::downgrade(shared);
    let pe = peer.to_string();
    let cx = ctx.clone();
    let ch = channels.clone();
    let weak_handlers = Rc::downgrade(&handlers);
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        if let (Some(sh), Some(handlers), Ok(e)) = (sh.upgrade(), weak_handlers.upgrade(), e.dyn_into::<web_sys::RtcDataChannelEvent>()) {
            attach_channel(e.channel(), pe.clone(), sh, generation, cx.clone(), ch.clone(), handlers);
        }
    });
    pc.set_ondatachannel(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
    shared.borrow_mut().links.insert(peer.into(), Link { connection: pc.clone(), channels, handlers });
    Ok(pc)
}
async fn description(pc: &RtcPeerConnection, offer: bool) -> Result<String, String> {
    let value = JsFuture::from(if offer { pc.create_offer() } else { pc.create_answer() }).await.map_err(js_error)?;
    // WebIDL dictionaries have no JavaScript constructor for instanceof/dyn_into.
    let sdp = js_sys::Reflect::get(&value, &JsValue::from_str("sdp")).map_err(js_error)?.as_string().ok_or("Missing generated SDP")?;
    let init = RtcSessionDescriptionInit::new(if offer { RtcSdpType::Offer } else { RtcSdpType::Answer });
    init.set_sdp(&sdp);
    JsFuture::from(pc.set_local_description(&init)).await.map_err(js_error)?;
    let start = clock_ms();
    let mut relay_seen = None;
    loop {
        let now = clock_ms();
        let local = pc.local_description();
        if local.as_ref().is_some_and(|desc| super::collaboration_protocol::candidate_kind(&desc.sdp(), &["relay"])) {
            relay_seen.get_or_insert(now);
        }
        if pc.ice_gathering_state() == web_sys::RtcIceGatheringState::Complete || relay_seen.is_some_and(|seen| now - seen >= 1000.0) {
            break;
        }
        if now - start >= 15000.0 {
            if local.as_ref().is_some_and(|desc| super::collaboration_protocol::candidate_kind(&desc.sdp(), &["relay", "srflx", "prflx"])) {
                break;
            }
            return Err("No Internet route gathered. Check network access and retry Join.".into());
        }
        sleep(50).await;
    }
    let desc = pc.local_description().ok_or("Missing local SDP")?;
    Ok(json!({"type":if offer {"offer"} else {"answer"},"sdp":desc.sdp()}).to_string())
}
async fn remote(pc: &RtcPeerConnection, sdp: &str, offer: bool) -> Result<(), String> {
    let value: Value = serde_json::from_str(sdp).map_err(|e| e.to_string())?;
    let sdp = value.get("sdp").and_then(Value::as_str).ok_or("Missing remote SDP")?;
    let init = RtcSessionDescriptionInit::new(if offer { RtcSdpType::Offer } else { RtcSdpType::Answer });
    init.set_sdp(sdp);
    JsFuture::from(pc.set_remote_description(&init)).await.map_err(js_error)?;
    Ok(())
}
async fn open_room(shared: Shared, generation: u64, ctx: egui::Context, server: String, code: Option<String>, password: Option<String>) -> Result<(), String> {
    let admission: Admission =
        serde_json::from_value(post(&server, if code.is_some() { "/join" } else { "/create" }, &json!({"code":code,"password":password})).await?)
            .map_err(|e| e.to_string())?;
    let result = async {
        if shared.borrow().generation != generation {
            let _ = post(&server, "/leave", &admission.auth()).await;
            return Ok(());
        }
        let ice = match post(&server, "/ice", &admission.auth()).await {
            Ok(v) => v.get("iceServers").cloned().ok_or("Missing ICE configuration")?,
            Err(e) if server.starts_with("http://127.0.0.1:") && e.contains("404") => json!([]),
            Err(e) => {
                let _ = post(&server, "/leave", &admission.auth()).await;
                return Err(e);
            }
        };
        if shared.borrow().generation != generation {
            return Ok(());
        }
        if ice.as_array().is_none_or(|v| v.len() > 16) {
            return Err("Invalid ICE configuration".into());
        }
        event(&shared, generation, Event::Joined(admission.clone()), &ctx);
        if admission.peer != admission.host {
            let pc = new_link(&admission.host, &ice, &shared, generation, &ctx)?;
            for label in ["edits", "presence", "voice"] {
                let opts = RtcDataChannelInit::new();
                opts.set_ordered(label == "edits");
                if label != "edits" {
                    opts.set_max_retransmits(0);
                }
                let channel = pc.create_data_channel_with_data_channel_dict(label, &opts);
                let (channels, handlers) =
                    shared.borrow().links.get(&admission.host).map(|l| (l.channels.clone(), l.handlers.clone())).ok_or("Missing host link")?;
                attach_channel(channel, admission.host.clone(), shared.clone(), generation, ctx.clone(), channels, handlers);
            }
            let sdp = description(&pc, true).await?;
            post(&server, "/signal", &json!({"auth":admission.auth(),"signal":{"from":admission.peer,"to":admission.host,"sdp":sdp,"offer":true}})).await?;
        }
        while shared.borrow().generation == generation {
            let signals: Vec<Signal> = serde_json::from_value(post(&server, "/poll", &admission.auth()).await?).map_err(|e| e.to_string())?;
            if shared.borrow().generation != generation {
                break;
            }
            for signal in signals {
                if shared.borrow().generation != generation {
                    break;
                }
                if signal.from.is_empty() || signal.from.len() > 128 || signal.sdp.len() > 64 * 1024 {
                    return Err("Invalid signaling message".into());
                }
                if signal.to != admission.peer {
                    continue;
                }
                if signal.sdp.is_empty() {
                    shared.borrow_mut().links.remove(&signal.from);
                    event(&shared, generation, Event::Disconnected(signal.from), &ctx);
                    continue;
                }
                if signal.offer && admission.peer == admission.host {
                    let pc = new_link(&signal.from, &ice, &shared, generation, &ctx)?;
                    remote(&pc, &signal.sdp, true).await?;
                    let sdp = description(&pc, false).await?;
                    post(&server, "/signal", &json!({"auth":admission.auth(),"signal":{"from":admission.peer,"to":signal.from,"sdp":sdp,"offer":false}}))
                        .await?;
                } else if !signal.offer && signal.from == admission.host {
                    let pc = shared.borrow().links.get(&signal.from).map(|l| l.connection.clone()).ok_or("Unknown host answer")?;
                    remote(&pc, &signal.sdp, false).await?;
                }
            }
            sleep(250).await;
        }
        Ok(())
    }
    .await;
    let _ = post(&server, "/leave", &admission.auth()).await;
    result
}
pub fn attach(mut app: PhotocraftApp, ctx: egui::Context) -> PhotocraftApp {
    let shared = Rc::new(RefCell::new(Transport { generation: 0, links: BTreeMap::new(), events: VecDeque::new() }));
    let mut bridge = Bridge {
        shared,
        admission: None,
        connected: HashSet::new(),
        decoder: Decoder::default(),
        outgoing: VecDeque::new(),
        transfer_id: 0,
        queued_bytes: 0,
        pending_bootstraps: Vec::new(),
        bootstrapped: false,
    };
    app.ui.collaboration.voice_enabled = false;
    app.with_collaboration_frame_hook(Box::new(move |app| {
        if let Err(error) = bridge.tick(app, &ctx) {
            app.ui.collaboration.error = error;
        }
    }))
}
impl Bridge {
    fn host(&self) -> bool {
        self.admission.as_ref().is_some_and(|a| a.peer == a.host)
    }
    fn send(&mut self, peer: Option<String>, channel: ChannelKind, message: &Wire) -> Result<(), String> {
        let peers: Vec<String> = match peer {
            Some(p) => vec![p],
            None => self.connected.iter().cloned().collect(),
        };
        self.send_to(peers, channel, message)
    }
    fn send_to(&mut self, peers: Vec<String>, channel: ChannelKind, message: &Wire) -> Result<(), String> {
        if peers.is_empty() {
            return Ok(());
        }
        if channel != ChannelKind::Edits {
            let bytes = wire::encode_unreliable(message).map_err(|e| e.to_string())?;
            for peer in peers {
                let _ = self.packet(&peer, channel, &bytes);
            }
            return Ok(());
        }
        if self.outgoing.len().saturating_add(peers.len()) > 4096 {
            return Err("Collaboration send queue full".into());
        }
        let bytes = Arc::new(wire::encode_message(message).map_err(|e| e.to_string())?);
        if self.queued_bytes.saturating_add(bytes.len().saturating_mul(peers.len())) > MAX_QUEUE_BYTES {
            return Err("Collaboration send queue full".into());
        }
        self.transfer_id = self.transfer_id.checked_add(1).ok_or("Transfer id exhausted")?;
        for peer in peers {
            self.queued_bytes = self.queued_bytes.saturating_add(bytes.len());
            self.outgoing.push_back(Job { peer, id: self.transfer_id, bytes: bytes.clone(), index: 0 });
        }
        Ok(())
    }
    fn broadcast(&mut self, except: Option<&str>, channel: ChannelKind, message: &Wire) -> Result<(), String> {
        let peers: Vec<_> = self.connected.iter().filter(|p| except != Some(p.as_str())).cloned().collect();
        self.send_to(peers, channel, message)
    }
    fn packet(&self, peer: &str, kind: ChannelKind, bytes: &[u8]) -> Result<bool, String> {
        let shared = self.shared.borrow();
        let Some(link) = shared.links.get(peer) else { return Ok(false) };
        let channels = link.channels.borrow();
        let label = match kind {
            ChannelKind::Edits => "edits",
            ChannelKind::Presence => "presence",
            ChannelKind::Voice => "voice",
        };
        let Some(channel) = channels.get(label) else { return Ok(false) };
        if channel.ready_state() != RtcDataChannelState::Open || channel.buffered_amount() > 512 * 1024 {
            return Ok(false);
        }
        channel.send_with_u8_array(bytes).map_err(js_error)?;
        Ok(true)
    }
    fn flush(&mut self) -> Result<(), String> {
        let start = clock_ms();
        for _ in 0..256 {
            if clock_ms() - start > 4.0 {
                break;
            }
            let candidates = super::collaboration_protocol::first_per_peer(self.outgoing.iter().map(|job| job.peer.as_str()));
            if candidates.is_empty() {
                break;
            }
            let mut progressed = false;
            // Descending indices permit removing completed jobs without invalidating earlier indices.
            for index in candidates.into_iter().rev() {
                let Some(job) = self.outgoing.get(index) else {
                    continue;
                };
                let packet = wire::fragment_message(job.id, &job.bytes, job.index).map_err(|e| e.to_string())?;
                if !self.packet(&job.peer, ChannelKind::Edits, &packet)? {
                    continue;
                }
                progressed = true;
                if let Some(job) = self.outgoing.get_mut(index) {
                    job.index += 1;
                    if job.index >= job.bytes.len().div_ceil(wire::FRAGMENT_BYTES) {
                        self.queued_bytes = self.queued_bytes.saturating_sub(job.bytes.len());
                        self.outgoing.remove(index);
                    }
                }
                if clock_ms() - start > 4.0 {
                    break;
                }
            }
            if !progressed {
                break;
            }
        }
        Ok(())
    }
    fn tick(&mut self, app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
        for intent in std::mem::take(&mut app.session.collaboration.network_intents) {
            let generation = self.shared.borrow().generation.saturating_add(1);
            self.shared.borrow_mut().generation = generation;
            self.shared.borrow_mut().links.clear();
            self.shared.borrow_mut().events.clear();
            self.connected.clear();
            self.outgoing.clear();
            self.queued_bytes = 0;
            self.decoder = Decoder::default();
            self.pending_bootstraps.clear();
            self.bootstrapped = false;
            self.admission = None;
            let generation = self.shared.borrow().generation;
            let (server, code, password) = match intent {
                NetworkIntent::Create { server, password, .. } => (server, None, password),
                NetworkIntent::Join { server, code, password, .. } => (server, Some(code), password),
                NetworkIntent::Leave => {
                    app.ui.collaboration.transport_pending = false;
                    app.ui.collaboration.transport_status = "Disconnected".into();
                    app.session.collaboration = Default::default();
                    continue;
                }
            };
            app.ui.collaboration.signaling_url = server.clone();
            app.ui.collaboration.transport_pending = true;
            app.ui.collaboration.transport_status = "Connecting…".into();
            let shared = self.shared.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                if let Err(e) = open_room(shared.clone(), generation, ctx.clone(), server, code, password).await {
                    event(&shared, generation, Event::Error(e), &ctx);
                }
            });
        }
        let start = clock_ms();
        for count in 0..256 {
            if count > 0 && clock_ms() - start > 4.0 {
                break;
            }
            let event = self.shared.borrow_mut().events.pop_front();
            let Some(event) = event else { break };
            match event {
                Event::Joined(a) => {
                    app.session.collaboration = Default::default();
                    app.session.collaboration.document_id = app.session.active().map(|s| s.doc.id);
                    app.session.collaboration.room = Some(RoomState::new(a.code.clone(), a.host.clone(), a.peer.clone()));
                    app.ui.collaboration.code = a.code.clone();
                    app.ui.collaboration.password.clear();
                    self.admission = Some(a);
                    if self.host() {
                        self.bootstrapped = true;
                        app.ui.collaboration.transport_pending = false;
                        app.ui.collaboration.transport_status = "Hosting — WebRTC ready".into();
                        publish_profile(app)?;
                    }
                }
                Event::Connected(peer) => {
                    if self.host() {
                        if !self.connected.contains(&peer) && !self.pending_bootstraps.contains(&peer) {
                            self.pending_bootstraps.push(peer);
                        }
                    } else {
                        self.connected.insert(peer);
                    }
                }
                Event::Packet(peer, channel, bytes) => {
                    let result = (|| {
                        let message = if channel == ChannelKind::Edits {
                            self.decoder.push(&peer, &bytes).map_err(|e| e.to_string())?
                        } else {
                            Some(wire::decode_unreliable(&bytes).map_err(|e| e.to_string())?)
                        };
                        if let Some(message) = message {
                            self.receive(app, &peer, channel, message)?;
                        }
                        Ok::<(), String>(())
                    })();
                    if let Err(error) = result {
                        self.peer_error(app, peer, error)?;
                    }
                }
                Event::PeerError(peer, error) => self.peer_error(app, peer, error)?,
                Event::Disconnected(peer) => {
                    self.pending_bootstraps.retain(|p| p != &peer);
                    self.connected.remove(&peer);
                    self.outgoing.retain(|j| j.peer != peer);
                    self.queued_bytes = self.outgoing.iter().map(|j| j.bytes.len()).sum();
                    self.decoder.remove(&peer);
                    if let Some(room) = app.session.collaboration.room.as_mut() {
                        room.members.remove(&peer);
                    }
                    if self.host() {
                        app.session.collaboration.sequencer.forget_peer(&peer);
                        self.broadcast(None, ChannelKind::Edits, &Wire::Left(peer))?;
                    } else {
                        app.ui.collaboration.transport_status = "Host disconnected — drawing preserved locally".into();
                        app.ui.collaboration.transport_pending = false;
                        app.session.collaboration = Default::default();
                    }
                }
                Event::Error(error) => {
                    let generation = self.shared.borrow().generation.saturating_add(1);
                    self.shared.borrow_mut().generation = generation;
                    self.shared.borrow_mut().links.clear();
                    self.shared.borrow_mut().events.clear();
                    self.admission = None;
                    self.connected.clear();
                    self.outgoing.clear();
                    self.pending_bootstraps.clear();
                    self.bootstrapped = false;
                    self.queued_bytes = 0;
                    app.session.collaboration = Default::default();
                    app.ui.collaboration.error = if error.contains("401") { "This room requires a password. Enter it and Join again.".into() } else { error };
                    app.ui.collaboration.open = true;
                    app.ui.collaboration.transport_pending = false;
                    app.ui.collaboration.transport_status = "Connection failed — retry Join".into();
                }
            }
        }
        if self.host() && !self.pending_bootstraps.is_empty() {
            let checkpoint = app.session.collaboration_checkpoint().map_err(|e| e.to_string())?;
            let room = app.session.collaboration.room.clone().ok_or("Missing room")?;
            let mut pending = std::mem::take(&mut self.pending_bootstraps);
            let mut connected = std::mem::take(&mut self.connected);
            let result = super::collaboration_protocol::admit_bootstraps(&mut pending, &mut connected, |peers| {
                self.send_to(peers, ChannelKind::Edits, &Wire::Checkpoint { document: vec![], room, checkpoint })
            });
            self.pending_bootstraps = pending;
            self.connected = connected;
            match result {
                Err(error) if error.starts_with("Collaboration send queue full") => {}
                result => result?,
            }
        }
        self.flush()?;
        let host = self.host();
        let mut accepted = 0;
        let mut send_error = None;
        for index in 0..256 {
            let message = if host {
                app.session.collaboration.canonical_outbox.get(index).cloned().map(Wire::Host)
            } else if self.bootstrapped && !app.ui.collaboration.transport_pending && !self.connected.is_empty() {
                app.session.collaboration.outbox.get(index).cloned().map(Wire::Client)
            } else {
                None
            };
            let Some(message) = message else { break };
            match self.send(None, ChannelKind::Edits, &message) {
                Ok(()) => accepted += 1,
                Err(error) if error.starts_with("Collaboration send queue full") => break,
                Err(error) => {
                    send_error = Some(error);
                    break;
                }
            }
            if clock_ms() - start > 8.0 {
                break;
            }
        }
        if host {
            drop(app.session.collaboration.canonical_outbox.drain(..accepted));
        } else {
            drop(app.session.collaboration.outbox.drain(..accepted));
        }
        if let Some(error) = send_error {
            return Err(error);
        }
        if let Some(presence) = app.session.collaboration.queued_presence.take()
            && let Some(a) = &self.admission
        {
            self.broadcast(None, ChannelKind::Presence, &Wire::Presence { peer: a.peer.clone(), presence })?;
        }
        app.ui.collaboration.voice_enabled = false;
        app.ui.collaboration.push_to_talk = false;
        app.ui.collaboration.voice_status = "Voice off".into();
        app.ui.collaboration.transport_metrics = json!({"browser":true,"incomingEventsQueued":self.shared.borrow().events.len(),"outgoingJobs":self.outgoing.len(),"bufferedBytes":self.queued_bytes});
        self.flush()?;
        Ok(())
    }
    fn peer_error(&mut self, app: &mut PhotocraftApp, peer: String, error: String) -> Result<(), String> {
        self.shared.borrow_mut().links.remove(&peer);
        self.connected.remove(&peer);
        self.pending_bootstraps.retain(|p| p != &peer);
        self.outgoing.retain(|job| job.peer != peer);
        self.queued_bytes = self.outgoing.iter().map(|job| job.bytes.len()).sum();
        self.decoder.remove(&peer);
        app.ui.collaboration.error = format!("Disconnected a peer after invalid input: {error}");
        if let Some(room) = app.session.collaboration.room.as_mut() {
            room.members.remove(&peer);
        }
        if self.host() {
            app.session.collaboration.sequencer.forget_peer(&peer);
            self.broadcast(None, ChannelKind::Edits, &Wire::Left(peer))?;
        } else {
            let next = self.shared.borrow().generation.saturating_add(1);
            self.shared.borrow_mut().generation = next;
            self.admission = None;
            self.bootstrapped = false;
            app.session.collaboration = Default::default();
            app.ui.collaboration.transport_pending = false;
            app.ui.collaboration.transport_status = "Disconnected — drawing preserved locally".into();
        }
        Ok(())
    }
    fn receive(&mut self, app: &mut PhotocraftApp, peer: &str, channel: ChannelKind, wire: Wire) -> Result<(), String> {
        let admission = self.admission.as_ref().ok_or("Packet received outside room")?;
        let host_id = admission.host.clone();
        if !self.host() && peer != host_id {
            return Err("Message came from a non-host peer".into());
        }
        match wire {
            Wire::Checkpoint { document, mut room, checkpoint } if !self.host() && channel == ChannelKind::Edits => {
                if self.bootstrapped {
                    return Err("Unexpected duplicate checkpoint".into());
                }
                let limits =
                    photocraft_format::LoadOptions { max_manifest_bytes: 8 << 20, max_blob_bytes: 64 << 20, max_total_bytes: 512 << 20, preserve_ids: true };
                let anchor = if document.is_empty() { &checkpoint.baseline } else { &document };
                let mut doc = photocraft_format::load_from_bytes_with(anchor, &limits).map_err(|e| e.to_string())?;
                doc.selection = None;
                doc.quick_mask = None;
                room.local_peer = admission.peer.clone();
                room.host = host_id;
                room.code = admission.code.clone();
                app.session.collaboration = Collaboration::default();
                app.session.add_document(doc, None);
                app.session.collaboration.document_id = app.session.active().map(|s| s.doc.id);
                app.session.collaboration.room = Some(room);
                app.session.collaboration_install_checkpoint(checkpoint).map_err(|e| e.to_string())?;
                if let Some(document) = app.session.active_mut() {
                    Arc::make_mut(&mut document.doc).selection = None;
                    Arc::make_mut(&mut document.doc).quick_mask = None;
                }
                publish_profile(app)?;
                self.bootstrapped = true;
                app.ui.collaboration.transport_pending = false;
                app.ui.collaboration.transport_status = "Connected — WebRTC drawing".into();
            }
            Wire::Bootstrap { document, events, mut room } if !self.host() && channel == ChannelKind::Edits => {
                if self.bootstrapped {
                    return Err("Unexpected duplicate bootstrap".into());
                }
                let limits =
                    photocraft_format::LoadOptions { max_manifest_bytes: 8 << 20, max_blob_bytes: 64 << 20, max_total_bytes: 512 << 20, preserve_ids: true };
                let mut doc = photocraft_format::load_from_bytes_with(&document, &limits).map_err(|e| e.to_string())?;
                doc.selection = None;
                doc.quick_mask = None;
                room.chat.clear();
                room.notes.clear();
                room.local_peer = admission.peer.clone();
                room.host = host_id;
                room.code = admission.code.clone();
                app.session.collaboration = Collaboration::default();
                app.session.add_document(doc, None);
                app.session.collaboration.document_id = app.session.active().map(|s| s.doc.id);
                app.session.collaboration.room = Some(room);
                for event in events {
                    app.session.collaboration_receive(&event).map_err(|e| e.to_string())?;
                }
                publish_profile(app)?;
                self.bootstrapped = true;
                app.ui.collaboration.transport_pending = false;
                app.ui.collaboration.transport_status = "Connected — WebRTC drawing".into();
            }
            Wire::Client(message) if self.host() && channel == ChannelKind::Edits => {
                if let Err(error) = app.session.collaboration_accept(peer, message) {
                    self.send(Some(peer.into()), ChannelKind::Edits, &Wire::Reject(error.to_string()))?;
                }
            }
            Wire::Host(event) if !self.host() && self.bootstrapped && channel == ChannelKind::Edits => {
                app.session.collaboration_receive(&event).map_err(|e| e.to_string())?;
            }
            Wire::Presence { peer: author, presence } if channel == ChannelKind::Presence => {
                if self.host() && author != peer {
                    return Err("Forged cursor identity".into());
                }
                photocraft_collab::validate(&Operation::Presence { presence: presence.clone() }).map_err(|e| e.to_string())?;
                if let Some(room) = app.session.collaboration.room.as_mut() {
                    room.members.insert(author.clone(), presence.clone());
                }
                if self.host() {
                    self.broadcast(Some(peer), channel, &Wire::Presence { peer: author, presence })?;
                }
            }
            Wire::Voice { peer: author, samples } if channel == ChannelKind::Voice => {
                if self.host() && author != peer {
                    return Err("Forged voice identity".into());
                }
                // Browser microphone is off; voice packets are validated by the wire decoder.
                if self.host() {
                    self.broadcast(Some(peer), channel, &Wire::Voice { peer: author, samples })?;
                }
            }
            Wire::Left(author) if !self.host() && channel == ChannelKind::Edits => {
                if let Some(room) = app.session.collaboration.room.as_mut() {
                    room.members.remove(&author);
                }
            }
            Wire::Reject(error) if !self.host() => {
                app.ui.collaboration.error = format!("Host rejected input: {error}. Leave and rejoin to resynchronize before drawing again.");
                app.ui.collaboration.transport_pending = true;
                app.session.collaboration.outbox.clear();
            }
            _ => return Err("Invalid room message or channel".into()),
        }
        Ok(())
    }
}
fn publish_profile(app: &mut PhotocraftApp) -> Result<(), String> {
    let state = &app.ui.collaboration;
    let profile = Profile {
        name: state.name.clone(),
        color: [f32::from(state.color[0]) / 255.0, f32::from(state.color[1]) / 255.0, f32::from(state.color[2]) / 255.0, 1.0],
        icon: (!state.icon.is_empty()).then(|| state.icon.clone()),
        show_name: state.name_visible,
        visible: state.cursor_visible,
    };
    let layer = app.session.active().and_then(|s| s.active_layer);
    app.session
        .collaboration_submit(Operation::Presence { presence: Presence { profile, position: None, layer, tool: "brush".into(), speaking: false } })
        .map_err(|e| e.to_string())
}
