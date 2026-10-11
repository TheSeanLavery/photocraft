//! Native platform bridge: room admission, encrypted peer transport, document bootstrap and audio.
use photocraft_collab::transport::{ChannelKind, Event, RTCIceServer, Request, Worker};
use photocraft_collab::wire::{self, Decoder, Wire};
use photocraft_collab::{Operation, Presence, Profile, RoomState};
use photocraft_engine::collab::{Collaboration, NetworkIntent};
use photocraft_ui_egui::PhotocraftApp;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
const FRAME_WORK_BUDGET: Duration = Duration::from_millis(4);
const MAX_PENDING: usize = 16_384;
const MAX_OUTGOING_JOBS: usize = 4096;
const MAX_OUTGOING_BYTES: usize = wire::MAX_TRANSFER;
const MAX_RESTORE_PACKETS: usize = 4096;
const MAX_RESTORE_PACKET_BYTES: usize = 64 << 20;
/// Remove only the accepted FIFO prefix, including when a later send hits backpressure.
fn enqueue_prefix<T>(queue: &mut Vec<T>, budget: Duration, mut send: impl FnMut(&T) -> Result<(), String>) -> (usize, Result<(), String>) {
    let started = Instant::now();
    let mut accepted = 0;
    let result = (|| {
        for item in queue.iter().take(256) {
            if accepted > 0 && started.elapsed() >= budget {
                break;
            }
            send(item)?;
            accepted += 1;
        }
        Ok(())
    })();
    drop(queue.drain(..accepted));
    (accepted, result)
}
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

enum RestoreUpdate {
    Progress { progress: photocraft_engine::collab::CheckpointProgress, prepare_ms: f64, max_step_ms: f64 },
    Complete { restore: Box<photocraft_engine::collab::CheckpointRestore>, prepare_ms: f64, max_step_ms: f64 },
    Failed(String),
}
struct NativeRestore {
    updates: std::sync::mpsc::Receiver<RestoreUpdate>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    room: RoomState,
    progress: photocraft_engine::collab::CheckpointProgress,
    complete: Option<Box<photocraft_engine::collab::CheckpointRestore>>,
    prepare_ms: f64,
    max_step_ms: f64,
}
impl Drop for NativeRestore {
    fn drop(&mut self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
impl NativeRestore {
    fn start(checkpoint: photocraft_collab::JournalCheckpoint, room: RoomState) -> Result<Self, String> {
        let total = checkpoint.entries.len().saturating_mul(2);
        let (tx, updates) = std::sync::mpsc::sync_channel(2);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = cancelled.clone();
        std::thread::Builder::new()
            .name("photocraft-checkpoint-restore".into())
            .spawn(move || {
                let result = (|| -> Result<RestoreUpdate, String> {
                    let preparing = Instant::now();
                    let mut restore = photocraft_engine::collab::CheckpointRestore::new(checkpoint).map_err(|e| e.to_string())?;
                    let prepare_ms = preparing.elapsed().as_secs_f64() * 1000.0;
                    let mut max_step_ms = 0.0f64;
                    let mut units = 0usize;
                    loop {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err("Checkpoint restoration cancelled".into());
                        }
                        let started = Instant::now();
                        let progress = restore.step(1).map_err(|e| e.to_string())?;
                        max_step_ms = max_step_ms.max(started.elapsed().as_secs_f64() * 1000.0);
                        if progress.done {
                            return Ok(RestoreUpdate::Complete { restore: Box::new(restore), prepare_ms, max_step_ms });
                        }
                        units = units.saturating_add(1);
                        if units.is_multiple_of(32) {
                            let _ = tx.try_send(RestoreUpdate::Progress { progress, prepare_ms, max_step_ms });
                        }
                    }
                })();
                let update = match result {
                    Ok(update) => update,
                    Err(error) => RestoreUpdate::Failed(error),
                };
                let _ = tx.send(update);
            })
            .map_err(|e| format!("Cannot start checkpoint restoration: {e}"))?;
        Ok(Self {
            updates,
            cancelled,
            room,
            progress: photocraft_engine::collab::CheckpointProgress { completed: 0, total, done: false, work_units: 0 },
            complete: None,
            prepare_ms: 0.0,
            max_step_ms: 0.0,
        })
    }
    fn poll(&mut self) -> Result<(), String> {
        if self.complete.is_some() {
            return Ok(());
        }
        for _ in 0..2 {
            match self.updates.try_recv() {
                Ok(RestoreUpdate::Progress { progress, prepare_ms, max_step_ms }) => {
                    self.progress = progress;
                    self.prepare_ms = prepare_ms;
                    self.max_step_ms = max_step_ms;
                }
                Ok(RestoreUpdate::Complete { restore, prepare_ms, max_step_ms }) => {
                    self.progress = restore.progress();
                    self.complete = Some(restore);
                    self.prepare_ms = prepare_ms;
                    self.max_step_ms = max_step_ms;
                    return Ok(());
                }
                Ok(RestoreUpdate::Failed(error)) => return Err(error),
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(()),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Err("Checkpoint restoration worker stopped before completion".into()),
            }
        }
        Ok(())
    }
    #[cfg(test)]
    fn completed(restore: photocraft_engine::collab::CheckpointRestore, room: RoomState) -> Self {
        let (_, updates) = std::sync::mpsc::sync_channel(2);
        let progress = restore.progress();
        Self {
            updates,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            room,
            progress,
            complete: Some(Box::new(restore)),
            prepare_ms: 0.0,
            max_step_ms: 0.0,
        }
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
    processed_incoming: usize,
    queued_outgoing: usize,
    worst_tick_ms: f64,
    restore: Option<NativeRestore>,
    restore_packets: VecDeque<Event>,
    restore_packet_bytes: usize,
    max_restore_step_ms: f64,
    checkpoint_prepare_ms: f64,
    join_started_doc: Option<(photocraft_doc::DocId, u64)>,
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
                processed_incoming: 0,
                queued_outgoing: 0,
                worst_tick_ms: 0.0,
                restore: None,
                restore_packets: VecDeque::new(),
                restore_packet_bytes: 0,
                max_restore_step_ms: 0.0,
                checkpoint_prepare_ms: 0.0,
                join_started_doc: None,
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
        let started = Instant::now();
        self.processed_incoming = 0;
        self.queued_outgoing = 0;
        let result = self.tick_inner(app);
        let tick_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.worst_tick_ms = self.worst_tick_ms.max(tick_ms);
        app.ui.collaboration.transport_metrics = serde_json::json!({
            "incomingEventsQueued": self.worker.events.len(),
            "canonicalOpsQueued": app.session.collaboration.canonical_outbox.len(),
            "clientOpsQueued": app.session.collaboration.outbox.len(),
            "outgoingJobs": self.outgoing.values().map(VecDeque::len).sum::<usize>(),
            "bufferedRequests": self.pending.len(),
            "checkpointRestorePending": self.restore.is_some(),
            "checkpointPacketsQueued": self.restore_packets.len(),
            "checkpointPacketBytes": self.restore_packet_bytes,
            "maxRestoreStepMs": self.max_restore_step_ms,
            "checkpointPrepareMs": self.checkpoint_prepare_ms,
            "processedIncoming": self.processed_incoming,
            "queuedOutgoing": self.queued_outgoing,
            "tickMs": tick_ms,
            "worstTickMs": self.worst_tick_ms,
        });
        result
    }
    fn tick_inner(&mut self, app: &mut PhotocraftApp) -> Result<(), String> {
        for intent in std::mem::take(&mut app.session.collaboration.network_intents) {
            self.restore = None;
            self.restore_packets.clear();
            self.restore_packet_bytes = 0;
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
                    app.ui.collaboration.signaling_url = server.clone();
                    app.session.active().ok_or("Open a document before creating a room")?;
                    app.ui.collaboration.transport_status = "Creating room…".into();
                    Request::Create { server, password, ice_servers }
                }
                NetworkIntent::Join { code, password, server, .. } => {
                    self.join_started_doc = app.session.active().map(|state| (state.doc.id, state.revision));
                    app.session.collaboration.document_id = None;
                    app.ui.collaboration.signaling_url = server.clone();
                    app.ui.collaboration.transport_status = "Joining room…".into();
                    Request::Join { server, code, password, ice_servers }
                }
                NetworkIntent::Leave => Request::Close,
            };
            self.worker.requests.try_send(request).map_err(|e| e.to_string())?;
        }
        self.flush_pending()?;
        let receiving_started = Instant::now();
        for count in 0..256 {
            // Always admit one item; a single engine edit/checkpoint cannot be preempted.
            // Leave the remaining FIFO untouched for the next room repaint.
            if count > 0 && receiving_started.elapsed() >= FRAME_WORK_BUDGET {
                break;
            }
            let event = if self.restore.is_none() && !self.restore_packets.is_empty() {
                let Some(event) = self.restore_packets.pop_front() else {
                    break;
                };
                if let Event::Packet { payload, .. } = &event {
                    self.restore_packet_bytes = self.restore_packet_bytes.saturating_sub(payload.len());
                }
                event
            } else {
                let Ok(event) = self.worker.events.try_recv() else {
                    break;
                };
                event
            };
            self.processed_incoming += 1;
            self.handle_event(app, event)?;
        }
        if let Some(mut pending) = self.restore.take() {
            if let Err(error) = pending.poll() {
                self.abandon_restore(app);
                return Err(error);
            }
            self.max_restore_step_ms = self.max_restore_step_ms.max(pending.max_step_ms);
            self.checkpoint_prepare_ms = pending.prepare_ms;
            if pending.complete.is_some() && !app.canvas_gesture_active() {
                let chosen_doc =
                    app.session.active().map(|state| (state.doc.id, state.revision)).filter(|state| Some(*state) != self.join_started_doc).map(|state| state.0);
                let restore = pending.complete.take().ok_or("Checkpoint completion missing")?;
                if let Err(error) = app.session.collaboration_commit_checkpoint(*restore, pending.room.clone()) {
                    self.abandon_restore(app);
                    return Err(error.to_string());
                }
                if let Some(index) = chosen_doc.and_then(|id| app.session.documents().iter().position(|state| state.doc.id == id)) {
                    app.session.set_active(index);
                }
                app.ui.collaboration.transport_pending = false;
                app.ui.collaboration.transport_status = "Connected — WebRTC drawing".into();
                publish_profile(app)?;
            } else {
                app.ui.collaboration.transport_status = format!("Restoring room drawing… {}/{}", pending.progress.completed, pending.progress.total);
                self.restore = Some(pending);
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
        let (accepted, send_result) = if self.host() {
            enqueue_prefix(&mut app.session.collaboration.canonical_outbox, FRAME_WORK_BUDGET, |event| {
                self.broadcast(None, ChannelKind::Edits, &Wire::Host(event.clone()))
            })
        } else if self.admission.as_ref().is_some_and(|a| self.connected.contains(&a.host)) {
            enqueue_prefix(&mut app.session.collaboration.outbox, FRAME_WORK_BUDGET, |message| {
                self.send(None, ChannelKind::Edits, &Wire::Client(message.clone()))
            })
        } else {
            (0, Ok(()))
        };
        self.queued_outgoing = accepted;
        match send_result {
            // Edit backpressure must not skip cursor/voice work. The unsent FIFO tail remains.
            Err(error) if error.starts_with("Collaboration send queue full") => {}
            result => result?,
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
    fn handle_event(&mut self, app: &mut PhotocraftApp, event: Event) -> Result<(), String> {
        match event {
            Event::Joined { admission } => {
                self.restore = None;
                self.restore_packets.clear();
                self.restore_packet_bytes = 0;
                app.session.collaboration = Collaboration::default();
                app.session.collaboration.document_id = if admission.peer == admission.host { app.session.active().map(|s| s.doc.id) } else { None };
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
                    self.restore = None;
                    self.restore_packets.clear();
                    self.restore_packet_bytes = 0;
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
                if self.restore.is_some() {
                    // Cursor/voice are ephemeral while admission is incomplete.
                    if channel != ChannelKind::Edits {
                        return Ok(());
                    }
                    if self.restore_packets.len() >= MAX_RESTORE_PACKETS || self.restore_packet_bytes.saturating_add(payload.len()) > MAX_RESTORE_PACKET_BYTES {
                        self.abandon_restore(app);
                        return Err("Checkpoint pending edit queue exceeded budget; rejoin the room".into());
                    }
                    self.restore_packet_bytes = self.restore_packet_bytes.saturating_add(payload.len());
                    self.restore_packets.push_back(Event::Packet { peer, channel, payload });
                    return Ok(());
                }
                if let Some(wire) = self.decode(&peer, channel, &payload)? {
                    self.receive(app, &peer, channel, wire)?;
                }
            }
            Event::Error(error) => {
                app.ui.collaboration.error = error;
                if self.admission.is_none() {
                    self.restore = None;
                    self.restore_packets.clear();
                    self.restore_packet_bytes = 0;
                    app.ui.collaboration.transport_pending = false;
                    app.session.collaboration = Default::default();
                    app.ui.collaboration.transport_status = "Disconnected — room admission failed".into();
                }
            }
            Event::Closed => {
                self.restore = None;
                self.restore_packets.clear();
                self.restore_packet_bytes = 0;
                app.ui.collaboration.transport_pending = false;
                app.session.collaboration = Default::default();
                app.ui.collaboration.transport_status = "Disconnected".into();
            }
        }
        Ok(())
    }
    fn abandon_restore(&mut self, app: &mut PhotocraftApp) {
        self.restore = None;
        self.restore_packets.clear();
        self.restore_packet_bytes = 0;
        self.admission = None;
        // Queued host edits must never apply to the pre-join local document after
        // an invalid checkpoint. Close the worker through the normal intent path.
        app.session.collaboration = Default::default();
        app.session.collaboration.network_intents.push(NetworkIntent::Leave);
        app.ui.collaboration.transport_pending = false;
        app.ui.collaboration.transport_status = "Disconnected — room restoration failed".into();
    }
    fn receive(&mut self, app: &mut PhotocraftApp, peer: &str, channel: ChannelKind, wire: Wire) -> Result<(), String> {
        let admission = self.admission.as_ref().ok_or("Packet received outside room")?;
        let host_id = admission.host.clone();
        if !self.host() && peer != host_id {
            return Err("Message came from a non-host peer".into());
        }
        match wire {
            Wire::Checkpoint { document: _, mut room, checkpoint } if !self.host() && channel == ChannelKind::Edits => {
                if app.session.collaboration.applied_revision != 0 || self.restore.is_some() {
                    return Err("Unexpected duplicate checkpoint".into());
                }
                room.local_peer = admission.peer.clone();
                room.host = host_id;
                room.code = admission.code.clone();
                self.restore = Some(match NativeRestore::start(checkpoint, room) {
                    Ok(restore) => restore,
                    Err(error) => {
                        self.abandon_restore(app);
                        return Err(error);
                    }
                });
                app.ui.collaboration.transport_pending = true;
                app.ui.collaboration.transport_status = "Restoring room drawing…".into();
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
            processed_incoming: 0,
            queued_outgoing: 0,
            worst_tick_ms: 0.0,
            restore: None,
            restore_packets: VecDeque::new(),
            restore_packet_bytes: 0,
            max_restore_step_ms: 0.0,
            checkpoint_prepare_ms: 0.0,
            join_started_doc: None,
        }
    }
    #[test]
    fn accepted_prefix_survives_backpressure_without_echo_or_loss() {
        let mut queue = vec![1, 2, 3];
        let mut sent = Vec::new();
        let (accepted, result) = enqueue_prefix(&mut queue, Duration::from_secs(1), |item| {
            if *item == 2 {
                return Err("full".into());
            }
            sent.push(*item);
            Ok(())
        });
        assert_eq!(accepted, 1);
        assert!(result.is_err());
        assert_eq!(queue, vec![2, 3]);
        let (_, result) = enqueue_prefix(&mut queue, Duration::from_secs(1), |item| {
            sent.push(*item);
            Ok(())
        });
        assert!(result.is_ok());
        assert!(queue.is_empty());
        assert_eq!(sent, vec![1, 2, 3]);
    }
    #[test]
    fn exhausted_frame_budget_still_makes_one_fifo_step() {
        let mut queue = vec![1, 2, 3];
        let (accepted, result) = enqueue_prefix(&mut queue, Duration::ZERO, |_| Ok(()));
        assert!(result.is_ok());
        assert_eq!(accepted, 1);
        assert_eq!(queue, vec![2, 3]);
    }
    #[test]
    fn staged_checkpoint_lifecycle_cancels_pending_fifo_without_touching_private_document() {
        for closed in [false, true] {
            let mut source = photocraft_engine::Session::new();
            source.execute("file.new", json!({"width":64,"height":64})).unwrap();
            source.execute("collab.room.create", json!({"peer":"host","code":"TEST01"})).unwrap();
            let mut room = source.collaboration.room.clone().unwrap();
            room.local_peer = "guest".into();
            let mut restore = photocraft_engine::collab::CheckpointRestore::new(source.collaboration_checkpoint().unwrap()).unwrap();
            while !restore.step(1).unwrap().done {}
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            app.run("file.new", json!({"width":32,"height":32})).unwrap();
            let id = app.session.active().unwrap().doc.id;
            let mut bridge = bridge();
            bridge.admission = Some(photocraft_collab::signaling::Admission {
                code: "TEST01".into(),
                peer: "guest".into(),
                host: "host".into(),
                token: "synthetic-test".into(),
            });
            bridge.restore = Some(NativeRestore::completed(restore, room));
            bridge.handle_event(&mut app, Event::Packet { peer: "host".into(), channel: ChannelKind::Edits, payload: vec![1] }).unwrap();
            bridge.handle_event(&mut app, Event::Packet { peer: "host".into(), channel: ChannelKind::Edits, payload: vec![2] }).unwrap();
            assert_eq!(bridge.restore_packets.len(), 2);
            assert_eq!(bridge.restore_packet_bytes, 2);
            bridge.handle_event(&mut app, if closed { Event::Closed } else { Event::PeerDisconnected { peer: "host".into() } }).unwrap();
            assert!(bridge.restore.is_none());
            assert!(bridge.restore_packets.is_empty());
            assert_eq!(bridge.restore_packet_bytes, 0);
            assert_eq!(app.session.documents().len(), 1);
            assert_eq!(app.session.active().unwrap().doc.id, id);
            bridge.worker.requests.try_send(Request::Close).unwrap();
        }
    }
    #[test]
    fn staged_checkpoint_defers_transform_and_preserves_new_private_revision_or_tab() {
        for switch_tab in [false, true] {
            let mut source = photocraft_engine::Session::new();
            source.execute("file.new", json!({"width":64,"height":64})).unwrap();
            source.execute("collab.room.create", json!({"peer":"host","code":"TEST01"})).unwrap();
            let mut room = source.collaboration.room.clone().unwrap();
            room.local_peer = "guest".into();
            let mut restore = photocraft_engine::collab::CheckpointRestore::new(source.collaboration_checkpoint().unwrap()).unwrap();
            while !restore.step(1).unwrap().done {}
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            app.run("file.new", json!({"width":64,"height":64})).unwrap();
            let mut bridge = bridge();
            bridge.join_started_doc = app.session.active().map(|state| (state.doc.id, state.revision));
            bridge.restore = Some(NativeRestore::completed(restore, room));
            if switch_tab {
                app.run("file.new", json!({"width":32,"height":32})).unwrap();
            } else {
                app.run("layer.new.layer", json!({"name":"Private edit during restore"})).unwrap();
            }
            let chosen = app.session.active().unwrap().doc.id;
            let count = app.session.documents().len();
            app.ui.transform = Some(
                serde_json::from_value(
                    json!({"session":1,"layer":1,"rect":[0,0,8,8],"quad":[[0,0],[8,0],[8,8],[0,8]],"pivot":[4,4],"interpolation":"bilinear"}),
                )
                .unwrap(),
            );
            bridge.tick_inner(&mut app).unwrap();
            assert!(bridge.restore.is_some());
            assert_eq!(app.session.documents().len(), count);
            assert_eq!(app.session.active().unwrap().doc.id, chosen);
            app.ui.transform = None;
            bridge.tick_inner(&mut app).unwrap();
            assert!(bridge.restore.is_none());
            assert_eq!(app.session.documents().len(), count + 1);
            assert_eq!(app.session.active().unwrap().doc.id, chosen);
            assert_ne!(app.session.collaboration.document_id, Some(chosen));
            assert!(!app.ui.collaboration.transport_pending);
            bridge.worker.requests.try_send(Request::Close).unwrap();
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
            app.run("file.new", json!({"width":64,"height":64,"background":"transparent"})).unwrap();
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
        late_app.run("file.new", json!({"width":64,"height":64,"background":"transparent"})).unwrap();
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
