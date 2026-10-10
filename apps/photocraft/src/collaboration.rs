//! Native platform bridge: room admission, encrypted peer transport, document bootstrap and audio.
use photocraft_collab::transport::{ChannelKind, Event, RTCIceServer, Request, Worker};
use photocraft_collab::wire::{self, Decoder, Wire};
use photocraft_collab::{Operation, Presence, Profile, RoomState};
use photocraft_engine::collab::{Collaboration, NetworkIntent};
use photocraft_ui_egui::PhotocraftApp;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;
const MAX_PENDING: usize = 16_384;
const MAX_OUTGOING_JOBS: usize = 4096;
const MAX_OUTGOING_BYTES: usize = wire::MAX_TRANSFER;
struct Outgoing {
    peer: Option<String>,
    id: u64,
    bytes: Arc<Vec<u8>>,
    index: usize,
}
impl Outgoing {
    fn packet(&self) -> Result<Vec<u8>, String> {
        wire::fragment_message(self.id, &self.bytes, self.index).map_err(|e| e.to_string())
    }
    fn done(&self) -> bool {
        self.index >= self.bytes.len().div_ceil(wire::FRAGMENT_BYTES)
    }
}

struct Bridge {
    worker: Worker,
    connected: HashSet<String>,
    pending_bootstraps: VecDeque<String>,
    admission: Option<photocraft_collab::signaling::Admission>,
    pending: VecDeque<Request>,
    outgoing: BTreeMap<String, VecDeque<Outgoing>>,
    outgoing_next: Option<String>,
    decoder: Decoder,
    transfer_id: u64,
    voice: Option<crate::collaboration_voice::Voice>,
    voice_failed: bool,
}

pub fn attach(mut app: PhotocraftApp) -> PhotocraftApp {
    match Worker::start() {
        Ok(worker) => {
            let mut bridge = Bridge {
                worker,
                connected: HashSet::new(),
                pending_bootstraps: VecDeque::new(),
                admission: None,
                pending: VecDeque::new(),
                outgoing: BTreeMap::new(),
                outgoing_next: None,
                decoder: Decoder::default(),
                transfer_id: 0,
                voice: None,
                voice_failed: false,
            };
            app = app.with_collaboration_frame_hook(Box::new(move |app| {
                if let Err(error) = bridge.tick(app) {
                    app.ui.collaboration.error = error;
                }
            }));
        }
        Err(error) => app.ui.collaboration.error = format!("WebRTC worker: {error}"),
    }
    app
}

impl Bridge {
    fn host(&self) -> bool {
        self.admission.as_ref().is_some_and(|a| a.peer == a.host)
    }
    fn send(&mut self, peer: Option<String>, channel: ChannelKind, message: &Wire) -> Result<(), String> {
        if channel != ChannelKind::Edits {
            let payload = wire::encode_unreliable(message).map_err(|e| e.to_string())?;
            if self.pending.len() < MAX_PENDING {
                self.pending.push_back(Request::Send { peer, channel, payload });
            }
            return Ok(());
        }
        let peers = vec![peer];
        self.queue_reliable(peers, message)
    }
    fn queue_reliable(&mut self, peers: Vec<Option<String>>, message: &Wire) -> Result<(), String> {
        if peers.is_empty() {
            return Ok(());
        }
        let jobs = self.outgoing.values().map(VecDeque::len).sum::<usize>();
        if jobs.saturating_add(peers.len()) > MAX_OUTGOING_JOBS {
            return Err("Collaboration send queue full; waiting for peers to drain".into());
        }
        let bytes = Arc::new(wire::encode_message(message).map_err(|e| e.to_string())?);
        let mut ids = HashSet::new();
        let buffered =
            self.outgoing.values().flat_map(|lane| lane.iter()).filter(|job| ids.insert(job.id)).map(|job| job.bytes.len()).fold(0usize, usize::saturating_add);
        if buffered.saturating_add(bytes.len()) > MAX_OUTGOING_BYTES {
            return Err("Collaboration send queue full; waiting for peers to drain".into());
        }
        self.transfer_id = self.transfer_id.checked_add(1).ok_or("Transfer id exhausted")?;
        for peer in peers {
            self.outgoing.entry(peer.clone().unwrap_or_default()).or_default().push_back(Outgoing {
                peer,
                id: self.transfer_id,
                bytes: bytes.clone(),
                index: 0,
            });
        }
        Ok(())
    }
    fn broadcast(&mut self, except: Option<&str>, channel: ChannelKind, message: &Wire) -> Result<(), String> {
        let peers: Vec<_> = self.connected.iter().filter(|peer| except != Some(peer.as_str())).cloned().collect();
        if channel == ChannelKind::Edits {
            return self.queue_reliable(peers.into_iter().map(Some).collect(), message);
        }
        for peer in peers {
            self.send(Some(peer), channel, message)?;
        }
        Ok(())
    }
    fn flush_pending(&mut self) -> Result<(), String> {
        for _ in 0..64 {
            let Some(request) = self.pending.pop_front() else {
                break;
            };
            match self.worker.requests.try_send(request) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(request)) => {
                    self.pending.push_front(request);
                    return Ok(());
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        // Round-robin peers while preserving complete-message order within each peer.
        for _ in 0..256 {
            let key = self
                .outgoing
                .keys()
                .find(|key| self.outgoing_next.as_ref().is_none_or(|last| *key > last))
                .cloned()
                .or_else(|| self.outgoing.keys().next().cloned());
            let Some(key) = key else {
                break;
            };
            let Some(job) = self.outgoing.get(&key).and_then(VecDeque::front) else {
                self.outgoing.remove(&key);
                continue;
            };
            let request = Request::Send { peer: job.peer.clone(), channel: ChannelKind::Edits, payload: job.packet()? };
            match self.worker.requests.try_send(request) {
                Ok(()) => {
                    if let Some(lane) = self.outgoing.get_mut(&key) {
                        if let Some(job) = lane.front_mut() {
                            job.index = job.index.saturating_add(1);
                            if job.done() {
                                lane.pop_front();
                            }
                        }
                        if lane.is_empty() {
                            self.outgoing.remove(&key);
                        }
                    }
                    self.outgoing_next = Some(key);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => break,
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }
    fn decode(&mut self, peer: &str, channel: ChannelKind, bytes: &[u8]) -> Result<Option<Wire>, String> {
        if channel == ChannelKind::Edits {
            self.decoder.push(peer, bytes).map_err(|e| e.to_string())
        } else {
            wire::decode_unreliable(bytes).map(Some).map_err(|e| e.to_string())
        }
    }
    fn tick(&mut self, app: &mut PhotocraftApp) -> Result<(), String> {
        for intent in std::mem::take(&mut app.session.collaboration.network_intents) {
            self.connected.clear();
            self.pending_bootstraps.clear();
            self.decoder = Decoder::default();
            self.pending.clear();
            self.outgoing.clear();
            self.outgoing_next = None;
            self.admission = None;
            self.voice = None;
            app.ui.collaboration.transport_pending = !matches!(&intent, NetworkIntent::Leave);
            // Operators configure STUN/TURN credentials via a private environment value.
            let ice_servers: Vec<RTCIceServer> = std::env::var("PHOTOCRAFT_ICE_SERVERS")
                .ok()
                .map(|v| serde_json::from_str(&v))
                .transpose()
                .map_err(|e| format!("Invalid PHOTOCRAFT_ICE_SERVERS: {e}"))?
                .unwrap_or_default();
            let request = match intent {
                NetworkIntent::Create { password, server, .. } => {
                    app.session.active().ok_or("Open a document before creating a room")?;
                    app.ui.collaboration.transport_status = "Creating room…".into();
                    Request::Create { server, password, ice_servers }
                }
                NetworkIntent::Join { code, password, server, .. } => {
                    app.ui.collaboration.transport_status = "Joining room…".into();
                    Request::Join { server, code, password, ice_servers }
                }
                NetworkIntent::Leave => Request::Close,
            };
            self.worker.requests.try_send(request).map_err(|e| e.to_string())?;
        }
        self.flush_pending()?;
        for _ in 0..256 {
            let Ok(event) = self.worker.events.try_recv() else {
                break;
            };
            match event {
                Event::Joined { admission } => {
                    app.session.collaboration = Collaboration::default();
                    app.session.collaboration.document_id = app.session.active().map(|s| s.doc.id);
                    app.session.collaboration.room = Some(RoomState::new(admission.code.clone(), admission.host.clone(), admission.peer.clone()));
                    app.ui.collaboration.code = admission.code.clone();
                    app.ui.collaboration.password.clear();
                    app.ui.collaboration.transport_status =
                        if admission.peer == admission.host { "Hosting — WebRTC ready".into() } else { "Connecting to host…".into() };
                    self.admission = Some(admission);
                    if self.host() {
                        app.ui.collaboration.transport_pending = false;
                        publish_profile(app)?;
                    }
                }
                Event::PeerConnected { peer } => {
                    if self.host() {
                        if !self.connected.contains(&peer) && !self.pending_bootstraps.contains(&peer) {
                            self.pending_bootstraps.push_back(peer);
                        }
                    } else {
                        self.connected.insert(peer);
                    }
                }
                Event::PeerDisconnected { peer } => {
                    self.connected.remove(&peer);
                    self.outgoing.remove(&peer);
                    self.pending_bootstraps.retain(|p| p != &peer);
                    self.decoder.remove(&peer);
                    if let Some(room) = app.session.collaboration.room.as_mut() {
                        room.members.remove(&peer);
                    }
                    if self.host() {
                        app.session.collaboration.sequencer.forget_peer(&peer);
                        self.broadcast(None, ChannelKind::Edits, &Wire::Left(peer))?;
                    } else if self.admission.as_ref().is_some_and(|a| a.host == peer) {
                        app.ui.collaboration.transport_status = "Host disconnected — shared session stopped".into();
                        app.ui.collaboration.error = "The host left. The received drawing is preserved locally; rejoin a new room to collaborate.".into();
                        app.session.collaboration = Default::default();
                        app.ui.collaboration.transport_pending = false;
                        self.admission = None;
                        self.pending.clear();
                        self.outgoing.clear();
                        self.outgoing_next = None;
                        self.voice = None;
                    }
                }
                Event::Packet { peer, channel, payload } => {
                    if let Some(wire) = self.decode(&peer, channel, &payload)? {
                        self.receive(app, &peer, channel, wire)?;
                    }
                }
                Event::Error(error) => {
                    app.ui.collaboration.error = error;
                    if self.admission.is_none() {
                        app.ui.collaboration.transport_pending = false;
                        app.session.collaboration = Default::default();
                        app.ui.collaboration.transport_status = "Disconnected — room admission failed".into();
                    }
                }
                Event::Closed => {
                    app.ui.collaboration.transport_pending = false;
                    app.session.collaboration = Default::default();
                    app.ui.collaboration.transport_status = "Disconnected".into();
                }
            }
        }
        if self.host() && !self.pending_bootstraps.is_empty() {
            let peers: Vec<_> = self.pending_bootstraps.iter().cloned().collect();
            let checkpoint = app.session.collaboration_checkpoint().map_err(|e| e.to_string())?;
            let document = Vec::new();
            let room = app.session.collaboration.room.clone().ok_or("No room for checkpoint")?;
            let result = self.queue_reliable(peers.iter().cloned().map(Some).collect(), &Wire::Checkpoint { document, room, checkpoint });
            match result {
                Ok(()) => {
                    self.pending_bootstraps.clear();
                    self.connected.extend(peers);
                }
                Err(error) if error.starts_with("Collaboration send queue full") => {}
                Err(error) => return Err(error),
            }
        }
        // Do not drain the operation queue until there is a negotiated host channel.
        if self.host() {
            for _ in 0..256 {
                let Some(event) = app.session.collaboration.canonical_outbox.first().cloned() else {
                    break;
                };
                self.broadcast(None, ChannelKind::Edits, &Wire::Host(event))?;
                app.session.collaboration.canonical_outbox.remove(0);
            }
        } else if self.admission.as_ref().is_some_and(|a| self.connected.contains(&a.host)) {
            for _ in 0..256 {
                let Some(message) = app.session.collaboration.outbox.first().cloned() else {
                    break;
                };
                self.send(None, ChannelKind::Edits, &Wire::Client(message))?;
                app.session.collaboration.outbox.remove(0);
            }
        }
        if let Some(presence) = app.session.collaboration.queued_presence.take()
            && let Some(admission) = self.admission.as_ref()
        {
            let wire = Wire::Presence { peer: admission.peer.clone(), presence };
            self.broadcast(None, ChannelKind::Presence, &wire)?;
        }
        self.tick_voice(app)?;
        self.flush_pending()?;
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
                if app.session.collaboration.applied_revision != 0 {
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
                app.ui.collaboration.transport_pending = false;
                app.ui.collaboration.transport_status = "Connected — WebRTC drawing".into();
            }
            Wire::Bootstrap { document, events, mut room } if !self.host() && channel == ChannelKind::Edits => {
                if app.session.collaboration.applied_revision != 0 {
                    return Err("Unexpected duplicate bootstrap".into());
                }
                let limits =
                    photocraft_format::LoadOptions { max_manifest_bytes: 8 << 20, max_blob_bytes: 64 << 20, max_total_bytes: 512 << 20, preserve_ids: true };
                let mut doc = photocraft_format::load_from_bytes_with(&document, &limits).map_err(|e| e.to_string())?;
                doc.selection = None;
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
                app.ui.collaboration.transport_pending = false;
                app.ui.collaboration.transport_status = "Connected — WebRTC drawing".into();
            }
            Wire::Client(message) if self.host() && channel == ChannelKind::Edits => {
                if let Err(error) = app.session.collaboration_accept(peer, message) {
                    self.send(Some(peer.into()), ChannelKind::Edits, &Wire::Reject(error.to_string()))?;
                }
            }
            Wire::Host(event) if !self.host() && channel == ChannelKind::Edits => {
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
                if let Some(voice) = &self.voice {
                    voice.receive(&author, &samples)?;
                }
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
    fn tick_voice(&mut self, app: &mut PhotocraftApp) -> Result<(), String> {
        let state = &mut app.ui.collaboration;
        if !state.voice_enabled || self.admission.is_none() {
            self.voice = None;
            self.voice_failed = false;
            state.voice_status = "Voice off".into();
            return Ok(());
        }
        if self.voice.is_none() && !self.voice_failed {
            match crate::collaboration_voice::Voice::open() {
                Ok(voice) => {
                    self.voice = Some(voice);
                }
                Err(error) => {
                    state.voice_status = error;
                    self.voice_failed = true;
                }
            }
        }
        if let Some(voice) = &self.voice {
            voice.set_talking(state.push_to_talk);
            state.voice_status = if state.push_to_talk { "Talking" } else { "Voice ready — hold to talk" }.into();
            if let Some(error) = voice.take_error() {
                state.voice_status = error;
            }
            let peer = self.admission.as_ref().map(|a| a.peer.clone()).unwrap_or_default();
            let mut packets = Vec::new();
            while let Some(packet) = voice.take_packet() {
                packets.push(packet);
            }
            for samples in packets {
                self.broadcast(None, ChannelKind::Voice, &Wire::Voice { peer: peer.clone(), samples })?;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_collab::wire::Fragment;
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn bridge() -> Bridge {
        Bridge {
            worker: Worker::start().unwrap(),
            connected: Default::default(),
            pending_bootstraps: VecDeque::new(),
            admission: None,
            pending: Default::default(),
            outgoing: BTreeMap::new(),
            outgoing_next: None,
            decoder: Decoder::default(),
            transfer_id: 0,
            voice: None,
            voice_failed: false,
        }
    }
    #[test]
    fn reliable_fanout_shares_bytes_and_preserves_per_peer_order() {
        let mut bridge = bridge();
        bridge.connected.extend(["alice".into(), "bob".into()]);
        bridge.broadcast(None, ChannelKind::Edits, &Wire::Reject("test".into())).unwrap();
        let a = bridge.outgoing.get("alice").unwrap().front().unwrap();
        let b = bridge.outgoing.get("bob").unwrap().front().unwrap();
        assert!(Arc::ptr_eq(&a.bytes, &b.bytes));
        bridge.broadcast(None, ChannelKind::Edits, &Wire::Reject("second".into())).unwrap();
        assert_eq!(bridge.outgoing.get("alice").unwrap().len(), 2);
        let before = bridge.outgoing.len();
        let mut oversized = Vec::new();
        oversized.resize(MAX_OUTGOING_JOBS + 1, Some("bob".into()));
        assert!(bridge.queue_reliable(oversized, &Wire::Reject("no".into())).is_err());
        assert_eq!(bridge.outgoing.len(), before);
    }

    #[test]
    fn fragments_reassemble_large_operations_and_reject_disorder() {
        let mut sender = bridge();
        let mut receiver = bridge();
        let wire = Wire::Reject("large payload ".repeat(4000));
        sender.send(Some("host".into()), ChannelKind::Edits, &wire).unwrap();
        let job = sender.outgoing.get("host").unwrap().front().unwrap();
        let total = job.bytes.len().div_ceil(wire::FRAGMENT_BYTES);
        assert!(total > 1);
        let mut completed = None;
        for index in 0..total {
            let payload = wire::fragment_message(job.id, &job.bytes, index).unwrap();
            let result = receiver.decode("host", ChannelKind::Edits, &payload).unwrap();
            if result.is_some() {
                assert!(completed.is_none());
                completed = result;
            }
        }
        assert!(matches!(completed, Some(Wire::Reject(message)) if message.len() == "large payload ".len() * 4000));
        let bad = serde_json::to_vec(&Fragment { id: 1, index: 1, total: 2, bytes: vec![1] }).unwrap();
        assert!(receiver.decode("host", ChannelKind::Edits, &bad).is_err());
    }

    fn pump(host: &mut Bridge, a: &mut PhotocraftApp, guest: &mut Bridge, b: &mut PhotocraftApp, until: impl Fn(&PhotocraftApp, &PhotocraftApp) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            host.tick(a).unwrap();
            guest.tick(b).unwrap();
            if until(a, b) {
                break;
            }
            assert!(Instant::now() < deadline, "native bridge timed out: host={} guest={}", a.ui.collaboration.error, b.ui.collaboration.error);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn pixels(app: &PhotocraftApp) -> Vec<f32> {
        photocraft_compose::render(&app.session.active().unwrap().doc, photocraft_geom::Rect::new(0, 0, 64, 64)).px.into_iter().flatten().collect()
    }

    #[test]
    fn actual_native_bridge_bootstrap_draw_chat_and_personal_undo() {
        bridge_acceptance(None);
    }
    #[test]
    #[ignore = "requires live PHOTOCRAFT_PLAYTEST_URL; run with PHOTOCRAFT_RTC_RELAY_ONLY=1"]
    fn public_cloudflare_bridge_draw_chat_late_join_and_undo() {
        let url = std::env::var("PHOTOCRAFT_PLAYTEST_URL").expect("public playtest URL");
        bridge_acceptance(Some(url));
    }
    fn bridge_acceptance(public_server: Option<String>) {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let (server, stop) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let server = format!("http://{}", listener.local_addr().unwrap());
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = axum::serve(listener, photocraft_collab::signaling::router())
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await;
            });
            (server, stop)
        });
        let server = public_server.unwrap_or(server);
        let mut a = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let mut b = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        a.ui.collaboration.name = "Host".into();
        b.ui.collaboration.name = "Guest".into();
        for app in [&mut a, &mut b] {
            app.run("file.new", json!({"width":64,"height":64,"fill":"transparent"})).unwrap();
        }
        a.run("collab.room.create", json!({"signalingUrl":server,"password":"test-password"})).unwrap();
        let (mut host, mut guest) = (bridge(), bridge());
        pump(&mut host, &mut a, &mut guest, &mut b, |a, _| a.ui.collaboration.transport_status.starts_with("Hosting"));
        let code = a.ui.collaboration.code.clone();
        b.run("collab.room.join", json!({"signalingUrl":server,"code":code,"password":"test-password"})).unwrap();
        pump(&mut host, &mut a, &mut guest, &mut b, |_, b| b.ui.collaboration.transport_status.starts_with("Connected"));
        assert_eq!(b.session.documents().len(), 2, "joining preserves the guest's original document");
        assert_eq!(pixels(&a), pixels(&b));
        for (app, id, x, color) in [(&mut a, "host-stroke", 12.0, [1.0, 0.0, 0.0, 1.0]), (&mut b, "guest-stroke", 40.0, [0.0, 0.0, 1.0, 1.0])] {
            app.run("collab.stroke.begin", json!({"id":id,"params":{"points":[[x,20.0,0.5]],"brush":{"size":8},"color":color,"seed":17}})).unwrap();
            app.run("collab.submit", json!({"type":"strokeChunk","id":id,"sequence":0,"points":[{"x":x,"y":20.0,"pressure":0.5}]})).unwrap();
            app.run("collab.submit", json!({"type":"strokeEnd","id":id,"sequence":1})).unwrap();
        }
        pump(&mut host, &mut a, &mut guest, &mut b, |a, b| {
            a.session.collaboration.applied_revision >= 6 && a.session.collaboration.applied_revision == b.session.collaboration.applied_revision
        });
        assert_eq!(pixels(&a), pixels(&b));
        let drawn = pixels(&a);
        assert!(drawn.iter().any(|p| *p != 0.0));
        b.run("collab.chat", json!({"text":"Hello host"})).unwrap();
        pump(&mut host, &mut a, &mut guest, &mut b, |a, b| {
            a.session.collaboration.room.as_ref().unwrap().chat.len() == 1 && b.session.collaboration.room.as_ref().unwrap().chat.len() == 1
        });
        let mut late_app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        late_app.ui.collaboration.name = "Late guest".into();
        late_app.run("file.new", json!({"width":64,"height":64,"fill":"transparent"})).unwrap();
        late_app.run("collab.room.join", json!({"signalingUrl":server,"code":a.ui.collaboration.code,"password":"test-password"})).unwrap();
        let mut late = bridge();
        pump(&mut host, &mut a, &mut late, &mut late_app, |_, late| late.ui.collaboration.transport_status.starts_with("Connected"));
        assert_eq!(pixels(&a), pixels(&late_app), "late checkpoint preserves both authors' existing strokes");
        assert_eq!(late_app.session.collaboration.room.as_ref().unwrap().chat.len(), 1, "late checkpoint preserves chat");
        a.run("edit.undo", json!({})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            host.tick(&mut a).unwrap();
            guest.tick(&mut b).unwrap();
            late.tick(&mut late_app).unwrap();
            if a.session.collaboration.applied_revision == b.session.collaboration.applied_revision
                && a.session.collaboration.applied_revision == late_app.session.collaboration.applied_revision
            {
                break;
            }
            assert!(Instant::now() < deadline, "late guest failed to converge after author undo");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pixels(&a), pixels(&late_app));
        late.worker.requests.try_send(Request::Close).unwrap();
        assert_eq!(pixels(&a), pixels(&b));
        assert_ne!(pixels(&a), drawn);
        let guest_pixel = a.session.active().unwrap().doc.layer(a.session.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().pixel(40, 20);
        assert!(guest_pixel.get(2).is_some_and(|b| *b > 0.5), "host undo preserves guest's blue stroke");
        host.worker.requests.try_send(Request::Close).unwrap();
        guest.worker.requests.try_send(Request::Close).unwrap();
        let _ = stop.send(());
    }
}
