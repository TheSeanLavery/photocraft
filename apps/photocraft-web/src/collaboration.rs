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
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet, VecDeque},
    rc::{Rc, Weak},
    sync::Arc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{RtcConfiguration, RtcDataChannel, RtcDataChannelInit, RtcDataChannelState, RtcPeerConnection, RtcSdpType, RtcSessionDescriptionInit};
#[path = "peer_policy.rs"]
mod peer_policy;
const MAX_EVENTS: usize = 1024;
// Encoded queues retain the protocol cap plus conservative structured-clone metadata credit.
const MAX_QUEUE_BYTES: usize = super::worker_codec::policy::MAX_SINGLE;
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
    identity: Rc<u64>,
    grace: peer_policy::DisconnectGrace,
    connection: RtcPeerConnection,
    channels: Rc<RefCell<BTreeMap<String, RtcDataChannel>>>,
    handlers: Handlers,
}
impl Drop for Link {
    fn drop(&mut self) {
        self.connection.set_ondatachannel(None);
        self.connection.set_onconnectionstatechange(None);
        self.connection.set_oniceconnectionstatechange(None);
        for channel in self.channels.borrow().values() {
            channel.set_onopen(None);
            channel.set_onclose(None);
            channel.set_onmessage(None);
            channel.set_onbufferedamountlow(None);
            channel.close();
        }
        self.connection.close();
    }
}
type EncodedSlot = Rc<RefCell<Option<Arc<Vec<u8>>>>>;
enum WorkerPending {
    Encode(EncodedSlot, usize),
    Decode(String, Rc<u64>, ChannelKind),
    Remove,
}
enum Event {
    Worker(Box<super::worker_codec::Reply>),
    Joined(Admission),
    Packet(String, Rc<u64>, ChannelKind, js_sys::ArrayBuffer),
    Connected(String, Rc<u64>),
    LinkHealth(String, Rc<u64>, String),
    Replaced(String, Rc<u64>, Rc<u64>),
    Error(String),
    PeerError(String, Rc<u64>, String),
}
struct Transport {
    generation: u64,
    next_instance: u64,
    links: BTreeMap<String, Link>,
    events: VecDeque<Event>,
    worker_events: VecDeque<Event>,
    pump: Option<Rc<BackgroundPump>>,
    codec: Option<Weak<super::worker_codec::Codec>>,
}
type Shared = Rc<RefCell<Transport>>;
struct Job {
    peer: String,
    id: u64,
    bytes: EncodedSlot,
    charge: usize,
    index: usize,
    bulk: bool,
    queued_at: f64,
}
struct Bridge {
    shared: Shared,
    admission: Option<Admission>,
    connected: HashSet<String>,
    decoder: Decoder,
    codec: Option<Rc<super::worker_codec::Codec>>,
    worker_pending: BTreeMap<u64, WorkerPending>,
    worker_completed: u64,
    worker_ms: f64,
    bootstrap_wait: Option<f64>,
    bootstrap_attempt: Option<f64>,
    outgoing: BTreeMap<String, VecDeque<Job>>,
    outgoing_next: Option<String>,
    outgoing_jobs: usize,
    input_priority: super::collaboration_protocol::InputPriority,
    cached_checkpoint: Option<(u64, Arc<Vec<u8>>)>,
    worst_tick_ms: f64,
    max_apply_ms: f64,
    last_snapshot_ms: f64,
    diagnostics: Option<Diagnostics>,
    frame_apply_ms: f64,
    progress: u64,
    background_ticks: u64,
    transfer_id: u64,
    queued_bytes: usize,
    pending_bootstraps: Vec<String>,
    pending_decoder_removals: HashSet<String>,
    bootstrapped: bool,
}
type SharedApp = Rc<RefCell<PhotocraftApp>>;
fn hidden() -> bool {
    web_sys::window().and_then(|window| window.document()).is_some_and(|doc| doc.hidden())
}
struct BackgroundPump {
    channel: web_sys::MessageChannel,
    pending: Cell<bool>,
    app: Weak<RefCell<PhotocraftApp>>,
    bridge: Weak<RefCell<Bridge>>,
    ctx: egui::Context,
    _callback: Closure<dyn FnMut(web_sys::MessageEvent)>,
    document: web_sys::Document,
    visibility_callback: Closure<dyn FnMut(web_sys::Event)>,
}
impl BackgroundPump {
    fn new(app: &SharedApp, bridge: &Rc<RefCell<Bridge>>, ctx: egui::Context) -> Result<Rc<Self>, String> {
        let channel = web_sys::MessageChannel::new().map_err(js_error)?;
        let document = web_sys::window().and_then(|w| w.document()).ok_or("No browser document")?;
        let app = Rc::downgrade(app);
        let bridge = Rc::downgrade(bridge);
        let pump = Rc::new_cyclic(|weak: &Weak<Self>| {
            let visibility_weak = weak.clone();
            let visibility_callback = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                if let Some(pump) = visibility_weak.upgrade() {
                    pump.wake();
                }
            });
            let weak = weak.clone();
            let callback = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
                if let Some(pump) = weak.upgrade() {
                    pump.run();
                }
            });
            channel.port1().set_onmessage(Some(callback.as_ref().unchecked_ref()));
            channel.port1().start();
            Self { channel, pending: Cell::new(false), app, bridge, ctx, _callback: callback, document: document.clone(), visibility_callback }
        });
        document.add_event_listener_with_callback("visibilitychange", pump.visibility_callback.as_ref().unchecked_ref()).map_err(js_error)?;
        Ok(pump)
    }
    fn wake(&self) {
        if !hidden() || self.pending.replace(true) {
            return;
        }
        if let Err(error) = self.channel.port2().post_message(&JsValue::NULL) {
            self.pending.set(false);
            log::warn!("Background collaboration wake: {error:?}");
            self.ctx.request_repaint();
        }
    }
    fn run(&self) {
        self.pending.set(false);
        if !hidden() {
            return;
        }
        let (Some(app), Some(bridge)) = (self.app.upgrade(), self.bridge.upgrade()) else {
            return;
        };
        let (Ok(mut app), Ok(mut bridge)) = (app.try_borrow_mut(), bridge.try_borrow_mut()) else {
            self.ctx.request_repaint();
            return;
        };
        let before = bridge.progress;
        bridge.background_ticks = bridge.background_ticks.saturating_add(1);
        let result = bridge.tick(&mut app, &self.ctx, true);
        let progressed = bridge.progress != before;
        let queued = !bridge.shared.borrow().events.is_empty()
            || !bridge.shared.borrow().worker_events.is_empty()
            || !app.session.collaboration.outbox.is_empty()
            || !app.session.collaboration.canonical_outbox.is_empty()
            || bridge.outgoing_jobs > 0
            || !bridge.pending_bootstraps.is_empty();
        let healthy = result.is_ok();
        if let Err(error) = result {
            app.ui.collaboration.error = error;
        }
        drop(bridge);
        drop(app);
        self.ctx.request_repaint();
        if super::collaboration_protocol::continue_background(hidden(), queued, progressed, healthy) {
            self.wake();
        }
    }
}
impl Drop for BackgroundPump {
    fn drop(&mut self) {
        let _ = self.document.remove_event_listener_with_callback("visibilitychange", self.visibility_callback.as_ref().unchecked_ref());
        self.channel.port1().set_onmessage(None);
        self.channel.port1().close();
        self.channel.port2().close();
    }
}
struct Diagnostics {
    element: web_sys::HtmlElement,
    ticks: super::collaboration_protocol::Samples,
    applies: super::collaboration_protocol::Samples,
    frames: super::collaboration_protocol::Samples,
    previous_frame: Option<f64>,
    last_publish: f64,
}
impl Diagnostics {
    fn open() -> Option<Self> {
        let window = web_sys::window()?;
        if !window.location().search().ok()?.trim_start_matches('?').split('&').any(|pair| pair == "collab_perf" || pair == "collab_perf=1") {
            return None;
        }
        let document = window.document()?;
        let element = document.create_element("pre").ok()?.dyn_into::<web_sys::HtmlElement>().ok()?;
        element.set_id("photocraft_collab_perf");
        element.set_attribute("aria-label", "PhotoCraft collaboration performance diagnostics").ok()?;
        element.set_attribute("style","position:fixed;right:8px;bottom:8px;z-index:9999;max-height:48vh;max-width:420px;overflow:auto;pointer-events:none;background:#171717e8;color:#eeeeee;padding:10px;font:11px monospace;white-space:pre-wrap;border-radius:5px").ok()?;
        document.body()?.append_child(&element).ok()?;
        Some(Self { element, ticks: Default::default(), applies: Default::default(), frames: Default::default(), previous_frame: None, last_publish: 0.0 })
    }
    fn publish(&mut self, now: f64, tick_ms: f64, apply_ms: f64, metrics: &Value, frame_tick: bool) {
        self.ticks.observe(tick_ms);
        self.applies.observe(apply_ms);
        if frame_tick && let Some(previous) = self.previous_frame.replace(now) {
            self.frames.observe(now - previous);
        }
        if now - self.last_publish < 250.0 {
            return;
        }
        self.last_publish = now;
        let summary = json!({"diagnostic":"PhotoCraft collaboration","scope":"last 240 collaboration ticks; logic-frame cadence sampled separately; not pointer-to-photon latency","tickMs":self.ticks.summary(),"applyMs":self.applies.summary(),"logicFrameIntervalMs":self.frames.summary(),"queues":metrics});
        if let Ok(text) = serde_json::to_string_pretty(&summary) {
            self.element.set_text_content(Some(&text));
        }
    }
}
impl Drop for Diagnostics {
    fn drop(&mut self) {
        self.element.remove();
    }
}
fn decoder_key(_peer: &str, identity: &Rc<u64>) -> String {
    format!("instance:{:x}", **identity)
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
    if state.events.len().saturating_add(state.worker_events.len()) >= MAX_EVENTS {
        state.events.clear();
        let codec = state.codec.as_ref().and_then(Weak::upgrade);
        if let (Some(codec), Event::Worker(reply)) = (&codec, &e) {
            codec.acknowledge(reply.id);
        }
        for reply in state.worker_events.drain(..) {
            if let (Some(codec), Event::Worker(reply)) = (&codec, reply) {
                codec.acknowledge(reply.id);
            }
        }
        state.events.push_back(Event::Error("Connection receive queue exceeded; rejoin to resynchronize".into()));
    } else if matches!(e, Event::Worker(_)) {
        state.worker_events.push_back(e);
    } else {
        state.events.push_back(e);
    }
    let pump = state.pump.clone();
    drop(state);
    ctx.request_repaint();
    if let Some(pump) = pump {
        pump.wake();
    }
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
    let identity = shared.borrow().links.get(&peer).map(|link| link.identity.clone());
    let Some(identity) = identity else {
        channel.close();
        return;
    };
    channel.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);
    channels.borrow_mut().insert(channel.label(), channel.clone());
    let sh = Rc::downgrade(&shared);
    let pe = peer.clone();
    let cx = ctx.clone();
    let token = identity.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if kind == ChannelKind::Edits
            && let Some(sh) = sh.upgrade()
        {
            event(&sh, generation, Event::Connected(pe.clone(), token.clone()), &cx);
        }
    });
    channel.set_onopen(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
    let sh = Rc::downgrade(&shared);
    let pe = peer.clone();
    let cx = ctx.clone();
    let token = identity.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        let Some(sh) = sh.upgrade() else {
            return;
        };
        let Ok(e) = e.dyn_into::<web_sys::MessageEvent>() else {
            return;
        };
        let data = e.data();
        if !data.is_instance_of::<js_sys::ArrayBuffer>() {
            event(&sh, generation, Event::PeerError(pe.clone(), token.clone(), "Expected binary collaboration packet".into()), &cx);
            return;
        }
        let array = js_sys::Uint8Array::new(&data);
        if array.length() as usize > wire::MAX_PACKET {
            event(&sh, generation, Event::PeerError(pe.clone(), token.clone(), "Collaboration packet too large".into()), &cx);
            return;
        }
        let Ok(buffer) = data.dyn_into::<js_sys::ArrayBuffer>() else {
            return;
        };
        event(&sh, generation, Event::Packet(pe.clone(), token.clone(), kind, buffer), &cx);
    });
    channel.set_onmessage(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
    let weak = Rc::downgrade(&shared);
    let close_channel = channel.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if kind == ChannelKind::Edits
            && let Some(shared) = weak.upgrade()
        {
            let token = shared.try_borrow().ok().and_then(|state| {
                state.links.get(&peer).filter(|link| link.channels.borrow().get("edits") == Some(&close_channel)).map(|link| link.identity.clone())
            });
            if let Some(token) = token {
                event(&shared, generation, Event::LinkHealth(peer.clone(), token, "closed".into()), &ctx);
            }
        }
    });
    channel.set_onclose(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
    channel.set_buffered_amount_low_threshold(32 * 1024);
    let weak = Rc::downgrade(&shared);
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if let Some(shared) = weak.upgrade()
            && let Ok(state) = shared.try_borrow()
            && state.generation == generation
            && let Some(pump) = state.pump.as_ref()
        {
            pump.wake();
        }
    });
    channel.set_onbufferedamountlow(Some(cb.as_ref().unchecked_ref()));
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
    if shared.borrow().links.len() >= 64 && !shared.borrow().links.contains_key(peer) {
        return Err("Too many room peers".into());
    }
    let instance = {
        let mut shared = shared.borrow_mut();
        shared.next_instance = shared.next_instance.checked_add(1).ok_or("Peer instance id exhausted")?;
        shared.next_instance
    };
    let identity = Rc::new(instance);
    let channels = Rc::new(RefCell::new(BTreeMap::new()));
    let handlers: Handlers = Rc::default();
    let sh = Rc::downgrade(shared);
    let pe = peer.to_string();
    let cx = ctx.clone();
    let ch = channels.clone();
    let weak_handlers = Rc::downgrade(&handlers);
    let token = Rc::downgrade(&identity);
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        if let (Some(sh), Some(handlers), Some(token), Ok(e)) =
            (sh.upgrade(), weak_handlers.upgrade(), token.upgrade(), e.dyn_into::<web_sys::RtcDataChannelEvent>())
        {
            let current = sh.try_borrow().ok().is_some_and(|state| {
                state.generation == generation && state.links.get(&pe).is_some_and(|link| peer_policy::same_instance(&link.identity, &token))
            });
            if current {
                attach_channel(e.channel(), pe.clone(), sh, generation, cx.clone(), ch.clone(), handlers);
            } else {
                e.channel().close();
            }
        }
    });
    pc.set_ondatachannel(Some(cb.as_ref().unchecked_ref()));
    handlers.borrow_mut().push(cb);
    for property in ["connectionState", "iceConnectionState"] {
        let weak = Rc::downgrade(shared);
        let peer = peer.to_owned();
        let token = Rc::downgrade(&identity);
        let callback_ctx = ctx.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let (Some(shared), Some(token)) = (weak.upgrade(), token.upgrade()) else { return };
            let state = {
                let Ok(transport) = shared.try_borrow() else { return };
                if transport.generation != generation {
                    return;
                }
                let Some(link) = transport.links.get(&peer).filter(|link| peer_policy::same_instance(&link.identity, &token)) else { return };
                js_sys::Reflect::get(link.connection.as_ref(), &JsValue::from_str(property)).ok().and_then(|value| value.as_string())
            };
            if let Some(state) = state {
                event(&shared, generation, Event::LinkHealth(peer.clone(), token, state), &callback_ctx);
            }
        });
        if property == "connectionState" {
            pc.set_onconnectionstatechange(Some(cb.as_ref().unchecked_ref()));
        } else {
            pc.set_oniceconnectionstatechange(Some(cb.as_ref().unchecked_ref()));
        }
        handlers.borrow_mut().push(cb);
    }
    let previous = shared
        .borrow_mut()
        .links
        .insert(peer.into(), Link { identity: identity.clone(), grace: Default::default(), connection: pc.clone(), channels, handlers });
    if let Some(previous) = previous {
        event(shared, generation, Event::Replaced(peer.to_owned(), identity, previous.identity.clone()), ctx);
    }
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
fn current_connection(shared: &Shared, generation: u64, peer: &str, pc: &RtcPeerConnection) -> bool {
    shared.try_borrow().ok().is_some_and(|state| state.generation == generation && state.links.get(peer).is_some_and(|link| link.connection == *pc))
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
            let result = description(&pc, true).await;
            if !current_connection(&shared, generation, &admission.host, &pc) {
                return Ok(());
            }
            let sdp = result?;
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
                    let token = shared.borrow().links.get(&signal.from).map(|link| link.identity.clone());
                    if let Some(token) = token {
                        event(&shared, generation, Event::LinkHealth(signal.from, token, "closed".into()), &ctx);
                    }
                    continue;
                }
                if signal.offer && admission.peer == admission.host {
                    let pc = new_link(&signal.from, &ice, &shared, generation, &ctx)?;
                    let result = remote(&pc, &signal.sdp, true).await;
                    if !current_connection(&shared, generation, &signal.from, &pc) {
                        continue;
                    }
                    result?;
                    let result = description(&pc, false).await;
                    if !current_connection(&shared, generation, &signal.from, &pc) {
                        continue;
                    }
                    let sdp = result?;
                    post(&server, "/signal", &json!({"auth":admission.auth(),"signal":{"from":admission.peer,"to":signal.from,"sdp":sdp,"offer":false}}))
                        .await?;
                } else if !signal.offer && signal.from == admission.host {
                    let Some(pc) = shared.borrow().links.get(&signal.from).map(|l| l.connection.clone()) else { continue };
                    let result = remote(&pc, &signal.sdp, false).await;
                    if !current_connection(&shared, generation, &signal.from, &pc) {
                        continue;
                    }
                    result?;
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
pub fn attach(mut app: PhotocraftApp, ctx: egui::Context) -> SharedApp {
    let shared = Rc::new(RefCell::new(Transport {
        generation: 0,
        next_instance: 0,
        links: BTreeMap::new(),
        events: VecDeque::new(),
        worker_events: VecDeque::new(),
        pump: None,
        codec: None,
    }));
    let bridge = Rc::new(RefCell::new(Bridge {
        shared: shared.clone(),
        admission: None,
        connected: HashSet::new(),
        decoder: Decoder::default(),
        codec: None,
        worker_pending: BTreeMap::new(),
        worker_completed: 0,
        worker_ms: 0.0,
        bootstrap_wait: None,
        bootstrap_attempt: None,
        outgoing: BTreeMap::new(),
        outgoing_next: None,
        outgoing_jobs: 0,
        input_priority: Default::default(),
        cached_checkpoint: None,
        worst_tick_ms: 0.0,
        max_apply_ms: 0.0,
        last_snapshot_ms: 0.0,
        diagnostics: Diagnostics::open(),
        frame_apply_ms: 0.0,
        progress: 0,
        background_ticks: 0,
        transfer_id: 0,
        queued_bytes: 0,
        pending_bootstraps: Vec::new(),
        pending_decoder_removals: HashSet::new(),
        bootstrapped: false,
    }));
    let disabled =
        web_sys::window().and_then(|w| w.location().search().ok()).is_some_and(|q| q.trim_start_matches('?').split('&').any(|p| p == "collab_worker=0"));
    if !disabled {
        let weak = Rc::downgrade(&shared);
        let callback_ctx = ctx.clone();
        match super::worker_codec::Codec::new(move |reply| {
            if let Some(shared) = weak.upgrade() {
                let current = shared.borrow().generation;
                if reply.id != 0 && reply.generation != current {
                    if let Some(codec) = shared.borrow().codec.as_ref().and_then(Weak::upgrade) {
                        codec.acknowledge(reply.id);
                    }
                    return;
                }
                let generation = if reply.id == 0 { current } else { reply.generation };
                event(&shared, generation, Event::Worker(Box::new(reply)), &callback_ctx);
            }
        }) {
            Ok(codec) => {
                shared.borrow_mut().codec = Some(Rc::downgrade(&codec));
                bridge.borrow_mut().codec = Some(codec);
            }
            Err(error) => log::warn!("PhotoCraft codec worker unavailable; using Window codec: {error}"),
        }
    }
    app.ui.collaboration.voice_enabled = false;
    let hook_bridge = bridge.clone();
    let hook_ctx = ctx.clone();
    app = app.with_collaboration_frame_hook(Box::new(move |app| {
        if let Ok(mut bridge) = hook_bridge.try_borrow_mut() {
            if let Err(error) = bridge.tick(app, &hook_ctx, false) {
                app.ui.collaboration.error = error;
            }
            let pump = bridge.shared.borrow().pump.clone();
            if let Some(pump) = pump {
                pump.wake();
            }
        }
    }));
    let app = Rc::new(RefCell::new(app));
    match BackgroundPump::new(&app, &bridge, ctx) {
        Ok(pump) => shared.borrow_mut().pump = Some(pump),
        Err(error) => {
            log::warn!("Background collaboration pump unavailable: {error}");
        }
    }
    app
}

fn sync_room_address(code: Option<&str>) {
    let result = (|| -> Result<(), String> {
        let window = web_sys::window().ok_or("No browser window")?;
        let location = window.location();
        let path = location.pathname().map_err(|e| format!("Cannot read page path: {e:?}"))?;
        let query = location.search().map_err(|e| format!("Cannot read page query: {e:?}"))?;
        let address = super::collaboration_protocol::room_page_address(&path, &query, code)?;
        if address != format!("{path}{query}") {
            window
                .history()
                .map_err(|e| format!("Cannot access page history: {e:?}"))?
                .replace_state_with_url(&JsValue::NULL, "", Some(&address))
                .map_err(|e| format!("Cannot update invitation address: {e:?}"))?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        log::warn!("{error}");
    }
}
impl Bridge {
    fn clear_worker_replies(&mut self) {
        let replies = std::mem::take(&mut self.shared.borrow_mut().worker_events);
        if let Some(codec) = &self.codec {
            for reply in replies {
                if let Event::Worker(reply) = reply {
                    codec.acknowledge(reply.id);
                }
            }
        }
    }
    fn current_link(&self, peer: &str, identity: &Rc<u64>) -> bool {
        self.shared.borrow().links.get(peer).is_some_and(|link| peer_policy::same_instance(&link.identity, identity))
    }
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
        if self.outgoing_jobs.saturating_add(peers.len()) > 4096 {
            return Err("Collaboration send queue full".into());
        }
        let bulk = matches!(message, Wire::Checkpoint { .. } | Wire::Bootstrap { .. });
        if let Some(codec) = &self.codec {
            // Reserve each FIFO before the asynchronous encode. Later canonical edits cannot
            // overtake a checkpoint, including when its captured revision is already stale.
            let generation = self.shared.borrow().generation;
            let prepared = codec.prepare_encode(generation, message).map_err(|error| {
                if error.starts_with("Collaboration send queue full") {
                    error
                } else {
                    format!("Codec worker encode failed — drawing preserved; retry without worker: {error}")
                }
            })?;
            let charge = prepared.charge;
            if !super::collaboration_protocol::fits_queue(self.outgoing_jobs, self.queued_bytes, peers.len(), charge, MAX_QUEUE_BYTES) {
                return Err("Collaboration send queue full".into());
            }
            let slot = Rc::new(RefCell::new(None));
            let (id, _) = codec.post(prepared).map_err(|error| format!("Codec worker post failed — rejoin: {error}"))?;
            if let Err(error) = self.queue_slot(peers, slot.clone(), charge, bulk) {
                return Err(format!("Worker queue reservation failed — rejoin: {error}"));
            }
            self.worker_pending.insert(id, WorkerPending::Encode(slot, charge));
            Ok(())
        } else {
            let bytes = Arc::new(wire::encode_message(message).map_err(|e| e.to_string())?);
            self.queue_encoded(peers, bytes, bulk)
        }
    }
    fn queue_encoded(&mut self, peers: Vec<String>, bytes: Arc<Vec<u8>>, bulk: bool) -> Result<(), String> {
        let charge = bytes.len();
        self.queue_slot(peers, Rc::new(RefCell::new(Some(bytes))), charge, bulk)
    }
    fn queue_slot(&mut self, peers: Vec<String>, bytes: EncodedSlot, charge: usize, bulk: bool) -> Result<(), String> {
        if !super::collaboration_protocol::fits_queue(self.outgoing_jobs, self.queued_bytes, peers.len(), charge, MAX_QUEUE_BYTES) {
            return Err("Collaboration send queue full".into());
        }
        self.transfer_id = self.transfer_id.checked_add(1).ok_or("Transfer id exhausted")?;
        let queued_at = clock_ms();
        for peer in peers {
            self.queued_bytes = self.queued_bytes.saturating_add(charge);
            self.outgoing_jobs += 1;
            self.progress = self.progress.saturating_add(1);
            self.outgoing.entry(peer.clone()).or_default().push_back(Job {
                peer,
                id: self.transfer_id,
                bytes: bytes.clone(),
                charge,
                index: 0,
                bulk,
                queued_at,
            });
        }
        Ok(())
    }
    fn remove_lane(&mut self, peer: &str) {
        if let Some(lane) = self.outgoing.remove(peer) {
            let charge = lane.iter().fold(0_usize, |total, job| total.saturating_add(job.charge));
            (self.outgoing_jobs, self.queued_bytes) = peer_policy::release_credit(self.outgoing_jobs, self.queued_bytes, lane.len(), charge);
        }
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
        if channel.ready_state() != RtcDataChannelState::Open || (channel.buffered_amount() as usize).saturating_add(bytes.len()) > 128 * 1024 {
            return Ok(false);
        }
        channel.send_with_u8_array(bytes).map_err(js_error)?;
        Ok(true)
    }
    fn flush(&mut self, budget: &mut super::collaboration_protocol::FrameBudget) -> Result<(), String> {
        let mut blocked = HashSet::new();
        let mut sent_live = false;
        let mut sent_bulk = false;
        loop {
            let key = super::collaboration_protocol::next_peer(
                self.outgoing
                    .iter()
                    .filter(|(peer, _)| !blocked.contains(peer.as_str()))
                    .filter_map(|(peer, lane)| lane.front().map(|job| (peer.as_str(), job.bulk))),
                self.outgoing_next.as_deref(),
                !budget.busy
                    || self.outgoing.values().any(|lane| {
                        lane.front().is_some_and(|job| job.bulk && super::worker_codec::policy::bulk_due(budget.busy, sent_bulk, clock_ms() - job.queued_at))
                    }),
                sent_live && !sent_bulk,
            );
            let Some(key) = key else { break };
            let Some(job) = self.outgoing.get(&key).and_then(VecDeque::front) else { break };
            let Some(bytes) = job.bytes.borrow().clone() else {
                blocked.insert(key);
                continue;
            };
            let size = wire::FRAGMENT_BYTES.min(bytes.len().saturating_sub(job.index.saturating_mul(wire::FRAGMENT_BYTES))).saturating_add(32);
            let ready = self.shared.borrow().links.get(&job.peer).is_some_and(|link| {
                link.channels.borrow().get("edits").is_some_and(|channel| {
                    peer_policy::packet_ready(channel.ready_state() == RtcDataChannelState::Open, channel.buffered_amount() as usize, size)
                })
            });
            if !ready {
                blocked.insert(key);
                continue;
            }
            if !budget.packet(clock_ms()) {
                break;
            }
            let packet = wire::fragment_message(job.id, &bytes, job.index).map_err(|e| e.to_string())?;
            if !self.packet(&job.peer, ChannelKind::Edits, &packet)? {
                blocked.insert(key);
                continue;
            }
            if job.bulk {
                sent_bulk = true;
            } else {
                sent_live = true;
            }
            self.progress = self.progress.saturating_add(1);
            self.outgoing_next = Some(key.clone());
            if let Some(lane) = self.outgoing.get_mut(&key) {
                if let Some(job) = lane.front_mut() {
                    job.index += 1;
                    if job.index >= bytes.len().div_ceil(wire::FRAGMENT_BYTES) {
                        self.queued_bytes = self.queued_bytes.saturating_sub(job.charge);
                        self.outgoing_jobs = self.outgoing_jobs.saturating_sub(1);
                        lane.pop_front();
                    }
                }
                if lane.is_empty() {
                    self.outgoing.remove(&key);
                }
            }
        }
        Ok(())
    }
    fn tick(&mut self, app: &mut PhotocraftApp, ctx: &egui::Context, from_pump: bool) -> Result<(), String> {
        let started = clock_ms();
        self.frame_apply_ms = 0.0;
        let busy = !hidden() && self.input_priority.busy(started, ctx.input(|i| i.pointer.any_down()));
        let mut budget = super::collaboration_protocol::FrameBudget::new(started, busy);
        let result = self.tick_inner(app, ctx, &mut budget);
        if let Err(error) = &result
            && (error.starts_with("Codec worker") || error.starts_with("Worker queue reservation"))
        {
            self.clear_worker_replies();
            self.shared.borrow_mut().events.clear();
            self.codec = None;
            let generation = self.shared.borrow().generation;
            event(&self.shared, generation, Event::Error(error.clone()), ctx);
        }
        let elapsed = clock_ms() - started;
        self.worst_tick_ms = self.worst_tick_ms.max(elapsed);
        let oldest = self.outgoing.values().filter_map(|lane| lane.front().map(|job| job.queued_at)).reduce(f64::min);
        let sctp = self
            .shared
            .borrow()
            .links
            .values()
            .map(|link| link.channels.borrow().values().map(|channel| u64::from(channel.buffered_amount())).sum::<u64>())
            .sum::<u64>();
        app.ui.collaboration.transport_metrics = json!({"browser":true,"documentHidden":hidden(),"backgroundPumpTicks":self.background_ticks,"tickMs":elapsed,"worstTickMs":self.worst_tick_ms,"maxApplyMs":self.max_apply_ms,"snapshotMs":self.last_snapshot_ms,"drawingPriority":busy,"incomingEventsQueued":self.shared.borrow().events.len(),"codecRepliesQueued":self.shared.borrow().worker_events.len(),"canonicalOpsQueued":app.session.collaboration.canonical_outbox.len(),"clientOpsQueued":app.session.collaboration.outbox.len(),"outgoingJobs":self.outgoing_jobs,"bufferedBytes":self.queued_bytes,"sctpBufferedBytes":sctp,"oldestQueuedMs":oldest.map(|time|(clock_ms()-time).max(0.0)),"pendingBootstraps":self.pending_bootstraps.len(),"codecWorkerActive":self.codec.as_ref().is_some_and(|c|c.ready.get()),"codecWorkerCompleted":self.worker_completed,"codecWorkerMs":self.worker_ms,"codecWorkerQueued":self.codec.as_ref().map_or(0,|c|c.queued()),"codecWorkerBytes":self.codec.as_ref().map_or(0,|c|c.bytes()),"codecWorkerCompletion":self.codec.as_ref().map(|c|c.completion_metrics())});
        if let Some(diagnostics) = self.diagnostics.as_mut() {
            diagnostics.publish(started, elapsed, self.frame_apply_ms, &app.ui.collaboration.transport_metrics, !from_pump);
        }
        result
    }
    fn tick_inner(&mut self, app: &mut PhotocraftApp, ctx: &egui::Context, budget: &mut super::collaboration_protocol::FrameBudget) -> Result<(), String> {
        if let Some(codec) = &self.codec
            && !codec.ready.get()
        {
            if js_sys::Date::now() - codec.started < 2000.0 {
                ctx.request_repaint_after(std::time::Duration::from_millis(25));
                return Ok(());
            }
            log::warn!("PhotoCraft codec worker ready timeout; using Window codec");
            self.codec = None;
        }
        if self.codec.as_ref().is_some_and(|codec| codec.queued() > 0) {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if self.codec.as_ref().is_some_and(|codec| codec.stalled()) {
            return Err("Codec worker completion timed out — drawing preserved; rejoin".into());
        }
        for intent in std::mem::take(&mut app.session.collaboration.network_intents) {
            let generation = self.shared.borrow().generation.saturating_add(1);
            self.shared.borrow_mut().generation = generation;
            self.shared.borrow_mut().links.clear();
            self.shared.borrow_mut().events.clear();
            self.clear_worker_replies();
            self.connected.clear();
            self.outgoing.clear();
            self.outgoing_next = None;
            self.outgoing_jobs = 0;
            self.cached_checkpoint = None;
            self.queued_bytes = 0;
            self.decoder = Decoder::default();
            self.worker_pending.clear();
            self.bootstrap_wait = None;
            self.bootstrap_attempt = None;
            self.pending_bootstraps.clear();
            self.pending_decoder_removals.clear();
            self.bootstrapped = false;
            self.admission = None;
            let generation = self.shared.borrow().generation;
            let (server, code, password) = match intent {
                NetworkIntent::Create { server, password, .. } => (server, None, password),
                NetworkIntent::Join { server, code, password, .. } => (server, Some(code), password),
                NetworkIntent::Leave => {
                    sync_room_address(None);
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
        if let Some(codec) = &self.codec {
            for key in self.pending_decoder_removals.clone() {
                if !budget.available(clock_ms()) {
                    break;
                }
                match codec.submit(self.shared.borrow().generation, super::worker_codec::Task::Remove(key.clone())) {
                    Ok((id, _)) => {
                        self.worker_pending.insert(id, WorkerPending::Remove);
                        self.pending_decoder_removals.remove(&key);
                    }
                    Err(error) if error.starts_with("Collaboration send queue full") => break,
                    Err(error) => return Err(format!("Codec worker cleanup failed — rejoin: {error}")),
                }
            }
            if !self.pending_decoder_removals.is_empty() {
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            }
        } else {
            self.pending_decoder_removals.clear();
        }
        while budget.event(clock_ms()) {
            let incoming = {
                let mut shared = self.shared.borrow_mut();
                shared.worker_events.pop_front().or_else(|| shared.events.pop_front())
            };
            let Some(incoming) = incoming else { break };
            self.progress = self.progress.saturating_add(1);
            match incoming {
                Event::Joined(a) => {
                    sync_room_address(Some(&a.code));
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
                Event::Connected(peer, identity) => {
                    if !self.current_link(&peer, &identity) {
                        continue;
                    }
                    if self.host() {
                        if !self.connected.contains(&peer) && !self.pending_bootstraps.contains(&peer) {
                            self.bootstrap_wait.get_or_insert(clock_ms());
                            self.pending_bootstraps.push(peer);
                        }
                    } else {
                        self.connected.insert(peer);
                    }
                }
                Event::Packet(peer, identity, channel, bytes) => {
                    if !self.current_link(&peer, &identity) {
                        continue;
                    }
                    if let Some(codec) = &self.codec {
                        if (self.bootstrap_wait.is_some_and(|since| clock_ms() - since >= 1500.0) && codec.queued() > 0)
                            || codec.queued() >= 256
                            || codec.bytes().saturating_add(bytes.byte_length() as usize).saturating_add(2048) > super::worker_codec::policy::LIMIT
                        {
                            self.shared.borrow_mut().events.push_front(Event::Packet(peer, identity.clone(), channel, bytes));
                            self.progress = self.progress.saturating_sub(1);
                            break;
                        }
                        match codec.submit_packet(self.shared.borrow().generation, decoder_key(&peer, &identity), channel == ChannelKind::Edits, &bytes) {
                            Ok((id, _)) => {
                                self.worker_pending.insert(id, WorkerPending::Decode(peer, identity, channel));
                            }
                            Err(error) => return Err(format!("Codec worker overloaded — rejoin: {error}")),
                        }
                        continue;
                    }
                    let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
                    let result = (|| {
                        let message = if channel == ChannelKind::Edits {
                            self.decoder.push(&decoder_key(&peer, &identity), &bytes).map_err(|e| e.to_string())?
                        } else {
                            Some(wire::decode_unreliable(&bytes).map_err(|e| e.to_string())?)
                        };
                        if let Some(message) = message {
                            let apply_started = clock_ms();
                            let result = self.receive(app, &peer, channel, message);
                            let apply_ms = clock_ms() - apply_started;
                            self.max_apply_ms = self.max_apply_ms.max(apply_ms);
                            self.frame_apply_ms = self.frame_apply_ms.max(apply_ms);
                            result?;
                        }
                        Ok::<(), String>(())
                    })();
                    if let Err(error) = result {
                        self.peer_error(app, peer, error)?;
                    }
                }
                Event::Worker(reply) => {
                    if let Some(codec) = &self.codec {
                        codec.acknowledge(reply.id);
                    }
                    use super::worker_codec::ResultData;
                    if matches!(reply.result, ResultData::Ready { .. }) {
                        continue;
                    }
                    if reply.id == 0 {
                        if self.admission.is_none() {
                            self.codec = None;
                            continue;
                        }
                        let generation = self.shared.borrow().generation;
                        event(&self.shared, generation, Event::Error("Codec worker stopped — drawing preserved; rejoin".into()), ctx);
                        continue;
                    }
                    let Some(pending) = self.worker_pending.remove(&reply.id) else {
                        continue;
                    };
                    self.worker_completed = self.worker_completed.saturating_add(1);
                    self.worker_ms = self.worker_ms.max(reply.elapsed_ms);
                    match (pending, reply.result) {
                        (WorkerPending::Encode(slot, charge), ResultData::Encoded(bytes)) if bytes.len() <= charge => {
                            *slot.borrow_mut() = Some(Arc::new(bytes));
                        }
                        (WorkerPending::Decode(peer, identity, channel), ResultData::Decoded(Some(message))) => {
                            if !self.current_link(&peer, &identity) {
                                continue;
                            }
                            let started = clock_ms();
                            let result = self.receive(app, &peer, channel, *message);
                            let elapsed = clock_ms() - started;
                            self.max_apply_ms = self.max_apply_ms.max(elapsed);
                            self.frame_apply_ms = self.frame_apply_ms.max(elapsed);
                            if let Err(error) = result {
                                self.peer_error(app, peer, error)?;
                            }
                        }
                        (WorkerPending::Decode(_, _, _), ResultData::Decoded(None)) | (WorkerPending::Remove, ResultData::Removed) => {}
                        (WorkerPending::Decode(peer, identity, _), ResultData::Failed(error)) => {
                            if self.current_link(&peer, &identity) {
                                self.peer_error(app, peer, error)?;
                            }
                        }
                        (_, result) => {
                            let error = match result {
                                ResultData::Failed(error) => error,
                                _ => "Invalid codec result".into(),
                            };
                            let generation = self.shared.borrow().generation;
                            event(&self.shared, generation, Event::Error(format!("Codec worker failed — rejoin: {error}")), ctx);
                        }
                    }
                }
                Event::PeerError(peer, identity, error) => {
                    if self.current_link(&peer, &identity) {
                        self.peer_error(app, peer, error)?;
                    }
                }
                Event::Replaced(peer, identity, previous) => {
                    self.remove_decoder(decoder_key(&peer, &previous));
                    if self.current_link(&peer, &identity) {
                        self.connected.remove(&peer);
                        self.pending_bootstraps.retain(|p| p != &peer);
                        self.remove_lane(&peer);
                    }
                }
                Event::LinkHealth(peer, identity, health) => {
                    let now = clock_ms();
                    let action = {
                        let mut shared = self.shared.borrow_mut();
                        shared
                            .links
                            .get_mut(&peer)
                            .filter(|link| peer_policy::same_instance(&link.identity, &identity))
                            .map(|link| link.grace.update(&health, now))
                    };
                    match action {
                        Some(peer_policy::HealthAction::Close) => self.disconnect(app, &peer)?,
                        Some(peer_policy::HealthAction::Schedule) => {
                            let weak = Rc::downgrade(&self.shared);
                            let token = Rc::downgrade(&identity);
                            let generation = self.shared.borrow().generation;
                            let ctx = ctx.clone();
                            spawn_local(async move {
                                sleep(30_000).await;
                                if let (Some(shared), Some(token)) = (weak.upgrade(), token.upgrade()) {
                                    event(&shared, generation, Event::LinkHealth(peer, token, "timeout".into()), &ctx);
                                }
                            });
                        }
                        _ => {}
                    }
                }
                Event::Error(error) => {
                    self.worker_pending.clear();
                    if error.starts_with("Codec worker") || error.starts_with("Worker queue reservation") {
                        self.codec = None;
                    }
                    let generation = self.shared.borrow().generation.saturating_add(1);
                    self.shared.borrow_mut().generation = generation;
                    self.shared.borrow_mut().links.clear();
                    self.shared.borrow_mut().events.clear();
                    self.clear_worker_replies();
                    self.admission = None;
                    self.connected.clear();
                    self.outgoing.clear();
                    self.outgoing_next = None;
                    self.outgoing_jobs = 0;
                    self.cached_checkpoint = None;
                    self.pending_bootstraps.clear();
                    self.pending_decoder_removals.clear();
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
        if self.host()
            && !self.pending_bootstraps.is_empty()
            && super::worker_codec::policy::snapshot_due(budget.busy, budget.snapshot(clock_ms()), self.bootstrap_wait.map_or(0.0, |since| clock_ms() - since))
            && self.bootstrap_attempt.is_none_or(|last| clock_ms() - last >= 500.0)
            && self.codec.as_ref().is_none_or(|codec| codec.queued() < 256 && codec.bytes() < super::worker_codec::policy::LIMIT.saturating_sub(128 * 1024))
        {
            self.bootstrap_attempt = Some(clock_ms());
            if self.codec.is_some() {
                let began = clock_ms();
                let checkpoint = app.session.collaboration_checkpoint().map_err(|e| e.to_string())?;
                let room = app.session.collaboration.room.clone().ok_or("Missing room")?;
                let peers = self.pending_bootstraps.clone();
                let result = self.send_to(peers.clone(), ChannelKind::Edits, &Wire::Checkpoint { document: vec![], room, checkpoint });
                self.last_snapshot_ms = clock_ms() - began;
                match result {
                    Ok(()) => {
                        self.connected.extend(peers);
                        self.pending_bootstraps.clear();
                        self.bootstrap_wait = None;
                    }
                    Err(error) if error.starts_with("Collaboration send queue full") => {}
                    Err(error) => return Err(error),
                }
            } else {
                let revision = app.session.collaboration.applied_revision;
                // Retain a stale encoded checkpoint while backpressured. Its known size avoids
                // repeatedly encoding a newer one that still cannot fit the same fanout.
                let payload = self.cached_checkpoint.as_ref().map_or(1, |(_, bytes)| bytes.len());
                if super::collaboration_protocol::fits_queue(self.outgoing_jobs, self.queued_bytes, self.pending_bootstraps.len(), payload, MAX_QUEUE_BYTES) {
                    if self.cached_checkpoint.as_ref().is_some_and(|(cached, _)| !super::collaboration_protocol::snapshot_current(*cached, revision)) {
                        self.cached_checkpoint = None;
                    }
                    if self.cached_checkpoint.is_none() {
                        let began = clock_ms();
                        let checkpoint = app.session.collaboration_checkpoint().map_err(|e| e.to_string())?;
                        let room = app.session.collaboration.room.clone().ok_or("Missing room")?;
                        let bytes = Arc::new(wire::encode_message(&Wire::Checkpoint { document: vec![], room, checkpoint }).map_err(|e| e.to_string())?);
                        self.last_snapshot_ms = clock_ms() - began;
                        self.cached_checkpoint = Some((revision, bytes));
                    }
                    if let Some((cached, bytes)) = self.cached_checkpoint.clone()
                        && super::collaboration_protocol::snapshot_current(cached, app.session.collaboration.applied_revision)
                    {
                        let mut pending = std::mem::take(&mut self.pending_bootstraps);
                        let mut connected = std::mem::take(&mut self.connected);
                        let result =
                            super::collaboration_protocol::admit_bootstraps(&mut pending, &mut connected, |peers| self.queue_encoded(peers, bytes, true));
                        self.pending_bootstraps = pending;
                        self.connected = connected;
                        match result {
                            Ok(()) => self.cached_checkpoint = None,
                            Err(error) if error.starts_with("Collaboration send queue full") => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
        }
        if self.pending_bootstraps.is_empty() {
            self.bootstrap_wait = None;
        }
        let host = self.host();
        let mut accepted = 0;
        let mut send_error = None;
        let mut index = 0;
        while budget.operation(clock_ms()) {
            if !self.pending_bootstraps.is_empty()
                && self.bootstrap_wait.is_some_and(|since| clock_ms() - since >= 1500.0)
                && self.codec.as_ref().is_some_and(|codec| codec.queued() > 0)
            {
                break;
            }
            let message = if host {
                app.session.collaboration.canonical_outbox.get(index).cloned().map(Wire::Host)
            } else if self.bootstrapped && !app.ui.collaboration.transport_pending && !self.connected.is_empty() {
                app.session.collaboration.outbox.get(index).cloned().map(Wire::Client)
            } else {
                None
            };
            let Some(message) = message else { break };
            match self.send(None, ChannelKind::Edits, &message) {
                Ok(()) => {
                    accepted += 1;
                    index += 1;
                }
                Err(error) if error.starts_with("Collaboration send queue full") => break,
                Err(error) => {
                    send_error = Some(error);
                    break;
                }
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
        self.flush(budget)?;
        if budget.available(clock_ms())
            && let Some(presence) = app.session.collaboration.queued_presence.take()
            && let Some(a) = &self.admission
        {
            self.broadcast(None, ChannelKind::Presence, &Wire::Presence { peer: a.peer.clone(), presence })?;
        }
        app.ui.collaboration.voice_enabled = false;
        app.ui.collaboration.push_to_talk = false;
        app.ui.collaboration.voice_status = "Voice off".into();
        Ok(())
    }
    fn remove_decoder(&mut self, key: String) {
        self.decoder.remove(&key);
        if self.codec.is_some() {
            self.pending_decoder_removals.insert(key);
        }
    }
    fn disconnect(&mut self, app: &mut PhotocraftApp, peer: &str) -> Result<(), String> {
        let link = self.shared.borrow_mut().links.remove(peer);
        let known = link.is_some() || self.connected.contains(peer) || self.pending_bootstraps.iter().any(|p| p == peer);
        if let Some(link) = link {
            self.remove_decoder(decoder_key(peer, &link.identity));
        }
        if !known {
            return Ok(());
        }
        self.pending_bootstraps.retain(|p| p != peer);
        self.connected.remove(peer);
        self.remove_lane(peer);
        if let Some(room) = app.session.collaboration.room.as_mut() {
            room.members.remove(peer);
        }
        if self.host() {
            app.session.collaboration.sequencer.forget_peer(peer);
            self.broadcast(None, ChannelKind::Edits, &Wire::Left(peer.to_owned()))?;
        } else {
            app.ui.collaboration.transport_status = "Host disconnected — drawing preserved locally".into();
            app.ui.collaboration.transport_pending = false;
            app.session.collaboration = Default::default();
        }
        Ok(())
    }
    fn peer_error(&mut self, app: &mut PhotocraftApp, peer: String, error: String) -> Result<(), String> {
        let link = self.shared.borrow_mut().links.remove(&peer);
        if let Some(link) = link {
            self.remove_decoder(decoder_key(&peer, &link.identity));
        }
        self.connected.remove(&peer);
        self.pending_bootstraps.retain(|p| p != &peer);
        self.remove_lane(&peer);
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
