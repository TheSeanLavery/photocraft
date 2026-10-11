//! Native WebRTC transport. Separate reliable edits and lossy presence/audio avoid
//! cursor packets holding document operations behind an ordered delivery queue.
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, mpsc};
pub use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::{
    api::APIBuilder,
    data_channel::{RTCDataChannel, data_channel_init::RTCDataChannelInit, data_channel_state::RTCDataChannelState},
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration, policy::ice_transport_policy::RTCIceTransportPolicy,
        sdp::session_description::RTCSessionDescription,
    },
};

pub type TransportResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const MAX_PACKET: usize = 48 * 1024;
const MAX_BUFFERED: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelKind {
    Edits,
    Presence,
    Voice,
}
impl ChannelKind {
    fn label(self) -> &'static str {
        match self {
            Self::Edits => "edits",
            Self::Presence => "presence",
            Self::Voice => "voice",
        }
    }
    fn from_label(label: &str) -> Option<Self> {
        match label {
            "edits" => Some(Self::Edits),
            "presence" => Some(Self::Presence),
            "voice" => Some(Self::Voice),
            _ => None,
        }
    }
}
#[derive(Debug)]
pub struct Incoming {
    pub channel: ChannelKind,
    pub payload: Vec<u8>,
}
pub struct RtcPeer {
    connection: Arc<RTCPeerConnection>,
    channels: Arc<Mutex<HashMap<ChannelKind, Arc<RTCDataChannel>>>>,
    inbox: mpsc::Sender<Incoming>,
}
impl RtcPeer {
    pub async fn new(ice_servers: Vec<RTCIceServer>) -> TransportResult<(Self, mpsc::Receiver<Incoming>)> {
        Self::new_with_relay(ice_servers, std::env::var("PHOTOCRAFT_RTC_RELAY_ONLY").is_ok_and(|v| v == "1")).await
    }
    /// Force relay for Internet acceptance, so local candidates cannot mask a TURN failure.
    pub async fn new_with_relay(ice_servers: Vec<RTCIceServer>, relay_only: bool) -> TransportResult<(Self, mpsc::Receiver<Incoming>)> {
        let ice_transport_policy = if relay_only { RTCIceTransportPolicy::Relay } else { RTCIceTransportPolicy::All };
        let connection =
            Arc::new(APIBuilder::new().build().new_peer_connection(RTCConfiguration { ice_servers, ice_transport_policy, ..Default::default() }).await?);
        let channels = Arc::new(Mutex::new(HashMap::new()));
        let (inbox, receiver) = mpsc::channel(512);
        let remote_channels = channels.clone();
        let remote_inbox = inbox.clone();
        connection.on_data_channel(Box::new(move |channel| {
            let channels = remote_channels.clone();
            let inbox = remote_inbox.clone();
            Box::pin(async move {
                if let Some(kind) = ChannelKind::from_label(channel.label()) {
                    attach(channel, kind, channels, inbox).await;
                } else {
                    let _ = channel.close().await;
                }
            })
        }));
        Ok((Self { connection, channels, inbox }, receiver))
    }
    pub async fn offer(&self) -> TransportResult<String> {
        for kind in [ChannelKind::Edits, ChannelKind::Presence, ChannelKind::Voice] {
            let init = RTCDataChannelInit {
                ordered: Some(kind == ChannelKind::Edits),
                max_retransmits: (kind != ChannelKind::Edits).then_some(0),
                ..Default::default()
            };
            let channel = self.connection.create_data_channel(kind.label(), Some(init)).await?;
            attach(channel, kind, self.channels.clone(), self.inbox.clone()).await;
        }
        let mut gathered = self.connection.gathering_complete_promise().await;
        self.connection.set_local_description(self.connection.create_offer(None).await?).await?;
        tokio::time::timeout(Duration::from_secs(15), gathered.recv()).await?;
        Ok(serde_json::to_string(&self.connection.local_description().await.ok_or("missing local SDP")?)?)
    }
    pub async fn answer(&self, offer: &str) -> TransportResult<String> {
        self.connection.set_remote_description(serde_json::from_str::<RTCSessionDescription>(offer)?).await?;
        let mut gathered = self.connection.gathering_complete_promise().await;
        self.connection.set_local_description(self.connection.create_answer(None).await?).await?;
        tokio::time::timeout(Duration::from_secs(15), gathered.recv()).await?;
        Ok(serde_json::to_string(&self.connection.local_description().await.ok_or("missing local SDP")?)?)
    }
    pub async fn accept_answer(&self, answer: &str) -> TransportResult<()> {
        self.connection.set_remote_description(serde_json::from_str(answer)?).await?;
        Ok(())
    }
    pub async fn send(&self, channel: ChannelKind, payload: &[u8]) -> TransportResult<()> {
        if payload.len() > MAX_PACKET {
            return Err("WebRTC packet exceeds limit; chunk payload".into());
        }
        let reliable = channel == ChannelKind::Edits;
        let channel = self.channels.lock().await.get(&channel).cloned().ok_or("data channel not negotiated")?;
        if channel.ready_state() != RTCDataChannelState::Open {
            return Err("data channel not open".into());
        }
        if channel.buffered_amount().await > MAX_BUFFERED {
            if !reliable {
                return Err("WebRTC send queue full".into());
            }
            // Reliable backpressure is not connection failure. This wait is bounded in
            // memory and canceled when the peer/worker closes; other peer/live writers run independently.
            while channel.buffered_amount().await > MAX_BUFFERED {
                if channel.ready_state() != RTCDataChannelState::Open {
                    return Err("data channel closed while draining".into());
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        channel.send(&Bytes::copy_from_slice(payload)).await?;
        Ok(())
    }
    /// Bytes currently buffered by SCTP for this data channel (zero before negotiation).
    /// This is transport telemetry; it excludes the application worker's pending requests.
    pub async fn buffered_bytes(&self, kind: ChannelKind) -> usize {
        let channel = self.channels.lock().await.get(&kind).cloned();
        match channel {
            Some(channel) => channel.buffered_amount().await,
            None => 0,
        }
    }
    pub async fn is_open(&self) -> bool {
        self.channels.lock().await.get(&ChannelKind::Edits).is_some_and(|c| c.ready_state() == RTCDataChannelState::Open)
    }
    pub async fn close(&self) -> TransportResult<()> {
        if let Err(error) = self.connection.close().await {
            // The stack closes every transport before reporting collected errors.
            // A remote SCTP shutdown racing our local data-channel reset is benign.
            let message = error.to_string();
            if !message.lines().all(|line| line.trim().is_empty() || line.trim() == "data_channels: sending reset packet in non-Established state") {
                return Err(error.into());
            }
        }
        Ok(())
    }
}
impl Drop for RtcPeer {
    fn drop(&mut self) {
        // Also release sockets when negotiation is canceled or SDP validation fails.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let connection = self.connection.clone();
            runtime.spawn(async move {
                let _ = connection.close().await;
            });
        }
    }
}
async fn attach(
    channel: Arc<RTCDataChannel>,
    kind: ChannelKind,
    channels: Arc<Mutex<HashMap<ChannelKind, Arc<RTCDataChannel>>>>,
    inbox: mpsc::Sender<Incoming>,
) {
    channel.on_message(Box::new(move |message| {
        let inbox = inbox.clone();
        Box::pin(async move {
            if message.data.len() <= MAX_PACKET {
                let incoming = Incoming { channel: kind, payload: message.data.to_vec() };
                if kind == ChannelKind::Edits {
                    let _ = inbox.send(incoming).await;
                } else {
                    let _ = inbox.try_send(incoming);
                }
            }
        })
    }));
    channels.lock().await.insert(kind, channel);
}

/// Nonblocking bridge for native GUI frames. The worker owns its Tokio runtime.
#[derive(Debug)]
pub enum Request {
    Create { server: String, password: Option<String>, ice_servers: Vec<RTCIceServer> },
    Join { server: String, code: String, password: Option<String>, ice_servers: Vec<RTCIceServer> },
    Send { peer: Option<String>, channel: ChannelKind, payload: Vec<u8> },
    Close,
}
#[derive(Debug)]
pub enum Event {
    Joined { admission: crate::signaling::Admission },
    PeerConnected { peer: String },
    PeerDisconnected { peer: String },
    Packet { peer: String, channel: ChannelKind, payload: Vec<u8> },
    Error(String),
    Closed,
}
type Epoch = Arc<std::sync::atomic::AtomicU64>;
struct ScopedEvent {
    generation: u64,
    event: Event,
}
struct ScopedRequest {
    generation: u64,
    request: Request,
}
#[derive(Clone)]
struct EventSender {
    sender: std::sync::mpsc::SyncSender<ScopedEvent>,
    depth: Arc<std::sync::atomic::AtomicUsize>,
    latest: Epoch,
    generation: u64,
}
impl EventSender {
    fn scoped(&self, generation: u64) -> Self {
        Self { generation, ..self.clone() }
    }
    fn try_send(&self, event: Event) -> Result<(), std::sync::mpsc::TrySendError<Event>> {
        use std::sync::atomic::Ordering;
        if self.generation != self.latest.load(Ordering::Acquire) {
            return Ok(());
        }
        self.depth.fetch_add(1, Ordering::Relaxed);
        let result = self.sender.try_send(ScopedEvent { generation: self.generation, event });
        if result.is_err() {
            self.depth.fetch_sub(1, Ordering::Relaxed);
        }
        result.map_err(|error| match error {
            std::sync::mpsc::TrySendError::Full(event) => std::sync::mpsc::TrySendError::Full(event.event),
            std::sync::mpsc::TrySendError::Disconnected(event) => std::sync::mpsc::TrySendError::Disconnected(event.event),
        })
    }
}
/// Nonblocking event inbox; stale room callbacks never enter the next session.
pub struct EventReceiver {
    receiver: std::sync::mpsc::Receiver<ScopedEvent>,
    depth: Arc<std::sync::atomic::AtomicUsize>,
    latest: Epoch,
}
impl EventReceiver {
    pub fn len(&self) -> usize {
        self.depth.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn try_recv(&self) -> Result<Event, std::sync::mpsc::TryRecvError> {
        loop {
            let event = self.receiver.try_recv()?;
            self.depth.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            if event.generation == self.latest.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(event.event);
            }
        }
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, std::sync::mpsc::RecvTimeoutError> {
        let started = std::time::Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            let event = self.receiver.recv_timeout(remaining)?;
            self.depth.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            if event.generation == self.latest.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(event.event);
            }
            if started.elapsed() >= timeout {
                return Err(std::sync::mpsc::RecvTimeoutError::Timeout);
            }
        }
    }
}
fn event_channel(capacity: usize, latest: Epoch) -> (EventSender, EventReceiver) {
    let (sender, receiver) = std::sync::mpsc::sync_channel(capacity);
    let depth = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let generation = latest.load(std::sync::atomic::Ordering::Acquire);
    (EventSender { sender, depth: depth.clone(), latest: latest.clone(), generation }, EventReceiver { receiver, depth, latest })
}
/// Independent bounded lanes keep Leave and live cursors/audio responsive to a slow edit peer.
#[derive(Clone)]
pub struct RequestSender {
    edits: mpsc::Sender<ScopedRequest>,
    live: mpsc::Sender<ScopedRequest>,
    control: mpsc::Sender<ScopedRequest>,
    latest: Epoch,
    enqueue: Arc<std::sync::Mutex<()>>,
}
impl RequestSender {
    pub fn try_send(&self, request: Request) -> Result<(), mpsc::error::TrySendError<Request>> {
        use std::sync::atomic::Ordering;
        // Serialize epoch publication with all lane enqueues, including cloned senders.
        let _guard = self.enqueue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let control = !matches!(request, Request::Send { .. });
        let lane = match &request {
            Request::Send { channel: ChannelKind::Edits, .. } => &self.edits,
            Request::Send { .. } => &self.live,
            _ => &self.control,
        };
        let permit = match lane.try_reserve() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Full(_)) => return Err(mpsc::error::TrySendError::Full(request)),
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(mpsc::error::TrySendError::Closed(request)),
        };
        let current = self.latest.load(Ordering::Acquire);
        let generation = if control {
            let Some(next) = current.checked_add(1) else {
                return Err(mpsc::error::TrySendError::Closed(request));
            };
            self.latest.store(next, Ordering::Release);
            next
        } else {
            current
        };
        permit.send(ScopedRequest { generation, request });
        Ok(())
    }
}
fn discard_room_sends(edits: &mut mpsc::Receiver<ScopedRequest>, live: &mut mpsc::Receiver<ScopedRequest>) {
    while edits.try_recv().is_ok() {}
    while live.try_recv().is_ok() {}
}
async fn receive_request(
    edits: &mut mpsc::Receiver<ScopedRequest>,
    live: &mut mpsc::Receiver<ScopedRequest>,
    control: &mut mpsc::Receiver<ScopedRequest>,
    accept_edits: bool,
) -> Option<ScopedRequest> {
    tokio::select! {
        biased;
        request = control.recv() => request,
        request = live.recv() => request,
        request = edits.recv(), if accept_edits => request,
    }
}
pub struct Worker {
    pub requests: RequestSender,
    pub events: EventReceiver,
}
impl Worker {
    pub fn start() -> std::io::Result<Self> {
        let (edits, receiver) = mpsc::channel(512);
        let (live, live_receiver) = mpsc::channel(128);
        let (control, control_receiver) = mpsc::channel(8);
        let latest = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let requests = RequestSender { edits, live, control, latest: latest.clone(), enqueue: Arc::new(std::sync::Mutex::new(())) };
        let (sender, events) = event_channel(512, latest);
        std::thread::Builder::new().name("photocraft-webrtc".into()).spawn(move || {
            match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(runtime) => runtime.block_on(run_worker(receiver, live_receiver, control_receiver, sender)),
                Err(error) => {
                    let _ = sender.try_send(Event::Error(error.to_string()));
                }
            }
        })?;
        Ok(Self { requests, events })
    }
}
struct Link {
    rtc: Arc<RtcPeer>,
    inbox: mpsc::Receiver<Incoming>,
    announced: bool,
    pending: Option<Event>,
    reliable: mpsc::Sender<Vec<u8>>,
    lossy: mpsc::Sender<(ChannelKind, Vec<u8>)>,
    queued: VecDeque<Vec<u8>>,
    failures: mpsc::Receiver<String>,
    writers: Vec<tokio::task::JoinHandle<()>>,
}
impl Link {
    fn new(rtc: RtcPeer, inbox: mpsc::Receiver<Incoming>) -> Self {
        let rtc = Arc::new(rtc);
        let (reliable, mut edits) = mpsc::channel::<Vec<u8>>(32);
        let (lossy, mut live) = mpsc::channel::<(ChannelKind, Vec<u8>)>(32);
        let (errors, failures) = mpsc::channel(8);
        let edit_rtc = rtc.clone();
        let edit_errors = errors.clone();
        let edit_writer = tokio::spawn(async move {
            while let Some(bytes) = edits.recv().await {
                if let Err(error) = edit_rtc.send(ChannelKind::Edits, &bytes).await {
                    let _ = edit_errors.try_send(error.to_string());
                    let _ = edit_rtc.close().await;
                    break;
                }
            }
        });
        let live_rtc = rtc.clone();
        let live_writer = tokio::spawn(async move {
            while let Some((channel, bytes)) = live.recv().await {
                if let Err(error) = live_rtc.send(channel, &bytes).await {
                    let _ = errors.try_send(error.to_string());
                }
            }
        });
        Self { rtc, inbox, announced: false, pending: None, reliable, lossy, queued: VecDeque::new(), failures, writers: vec![edit_writer, live_writer] }
    }
    fn enqueue(&mut self, channel: ChannelKind, payload: Vec<u8>) -> Result<(), Vec<u8>> {
        if channel != ChannelKind::Edits {
            let _ = self.lossy.try_send((channel, payload));
            return Ok(());
        }
        if !self.queued.is_empty() {
            if self.queued.len() == 32 {
                return Err(payload);
            }
            self.queued.push_back(payload);
            return Ok(());
        }
        match self.reliable.try_send(payload) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(bytes)) => {
                self.queued.push_back(bytes);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(bytes)) => Err(bytes),
        }
    }
    fn flush(&mut self) {
        while let Some(bytes) = self.queued.pop_front() {
            if let Err(error) = self.reliable.try_send(bytes) {
                self.queued.push_front(error.into_inner());
                break;
            }
        }
    }
}
impl Drop for Link {
    fn drop(&mut self) {
        for task in &self.writers {
            task.abort();
        }
    }
}
struct ConnectedRoom {
    server: String,
    admission: crate::signaling::Admission,
    ice_servers: Vec<RTCIceServer>,
    links: HashMap<String, Link>,
    signals: mpsc::Receiver<TransportResult<Vec<crate::signaling::Signal>>>,
    poll_task: tokio::task::JoinHandle<()>,
    negotiations: mpsc::Receiver<(String, TransportResult<Link>)>,
    negotiation_sender: mpsc::Sender<(String, TransportResult<Link>)>,
    negotiation_tasks: Vec<tokio::task::JoinHandle<()>>,
}
async fn post<T: Serialize + ?Sized>(client: &reqwest::Client, server: &str, path: &str, request: &T) -> TransportResult<reqwest::Response> {
    Ok(client.post(format!("{}{path}", server.trim_end_matches('/'))).json(request).send().await?.error_for_status()?)
}
#[derive(Deserialize)]
struct IceResponse {
    #[serde(rename = "iceServers")]
    ice_servers: Vec<RTCIceServer>,
}
async fn room_ice(client: &reqwest::Client, server: &str, admission: &crate::signaling::Admission) -> TransportResult<Vec<RTCIceServer>> {
    let response = client
        .post(format!("{}/ice", server.trim_end_matches('/')))
        .timeout(Duration::from_secs(20))
        .json(&crate::signaling::Auth::from(admission))
        .send()
        .await?;
    // Older local signaling servers intentionally have no TURN service.
    if response.status() == reqwest::StatusCode::NOT_FOUND
        && reqwest::Url::parse(server).is_ok_and(|url| url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
    {
        return Ok(vec![]);
    }
    let mut response = response.error_for_status()?;
    if response.content_length().is_some_and(|n| n > 64 * 1024) {
        return Err("ICE response too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > (64 * 1024usize).saturating_sub(bytes.len()) {
            return Err("ICE response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let ice: IceResponse = serde_json::from_slice(&bytes)?;
    if ice.ice_servers.is_empty() || ice.ice_servers.len() > 16 || ice.ice_servers.iter().any(|s| s.urls.len() > 16) {
        return Err("Invalid room ICE configuration".into());
    }
    Ok(ice.ice_servers)
}
async fn open_room(
    client: &reqwest::Client,
    server: String,
    code: Option<String>,
    password: Option<String>,
    ice_servers: Vec<RTCIceServer>,
) -> TransportResult<ConnectedRoom> {
    let endpoint = if code.is_some() { "/join" } else { "/create" };
    let admission = post(client, &server, endpoint, &crate::signaling::RoomRequest { code, password }).await?.json::<crate::signaling::Admission>().await?;
    let ice_servers = if ice_servers.is_empty() {
        match room_ice(client, &server, &admission).await {
            Ok(servers) => servers,
            Err(error) => {
                let _ = post(client, &server, "/leave", &crate::signaling::Auth::from(&admission)).await;
                return Err(error);
            }
        }
    } else {
        ice_servers
    };
    let mut links = HashMap::new();
    if admission.peer != admission.host {
        let (rtc, inbox) = RtcPeer::new(ice_servers.clone()).await?;
        let sdp = rtc.offer().await?;
        post(
            client,
            &server,
            "/signal",
            &crate::signaling::SendSignal {
                auth: (&admission).into(),
                signal: crate::signaling::Signal { from: admission.peer.clone(), to: admission.host.clone(), sdp, offer: true },
            },
        )
        .await?;
        links.insert(admission.host.clone(), Link::new(rtc, inbox));
    }
    let (signal_sender, signals) = mpsc::channel(64);
    let (negotiation_sender, negotiations) = mpsc::channel(64);
    let poll_client = client.clone();
    let poll_server = server.clone();
    let auth = crate::signaling::Auth::from(&admission);
    let poll_task = tokio::spawn(async move {
        loop {
            let result =
                async { post(&poll_client, &poll_server, "/poll", &auth).await?.json::<Vec<crate::signaling::Signal>>().await.map_err(Into::into) }.await;
            let failed = result.is_err();
            if signal_sender.send(result).await.is_err() {
                break;
            }
            tokio::time::sleep(if failed { Duration::from_secs(2) } else { Duration::from_millis(150) }).await;
        }
    });
    Ok(ConnectedRoom { server, admission, ice_servers, links, signals, poll_task, negotiations, negotiation_sender, negotiation_tasks: Vec::new() })
}
async fn tick_room(client: &reqwest::Client, room: &mut ConnectedRoom, events: &EventSender) -> TransportResult<()> {
    room.negotiation_tasks.retain(|task| !task.is_finished());
    while let Ok((peer, result)) = room.negotiations.try_recv() {
        match result {
            Ok(link) => {
                room.links.insert(peer, link);
            }
            Err(e) => {
                let _ = events.try_send(Event::Error(e.to_string()));
            }
        }
    }
    while let Ok(batch) = room.signals.try_recv() {
        let signals = match batch {
            Ok(signals) => signals,
            Err(e) => {
                let _ = events.try_send(Event::Error(e.to_string()));
                continue;
            }
        };
        for signal in signals {
            if signal.sdp.is_empty() {
                if let Some(link) = room.links.remove(&signal.from) {
                    let _ = link.rtc.close().await;
                }
                let _ = events.try_send(Event::PeerDisconnected { peer: signal.from });
                continue;
            }
            if signal.offer && room.admission.peer == room.admission.host {
                if room.links.len() + room.negotiation_tasks.len() >= 63 {
                    continue;
                }
                let client = client.clone();
                let server = room.server.clone();
                let admission = room.admission.clone();
                let ice = room.ice_servers.clone();
                let sender = room.negotiation_sender.clone();
                room.negotiation_tasks.push(tokio::spawn(async move {
                    let peer = signal.from.clone();
                    let result = async {
                        let (rtc, inbox) = RtcPeer::new(ice).await?;
                        let sdp = rtc.answer(&signal.sdp).await?;
                        post(
                            &client,
                            &server,
                            "/signal",
                            &crate::signaling::SendSignal {
                                auth: (&admission).into(),
                                signal: crate::signaling::Signal { from: admission.peer, to: peer.clone(), sdp, offer: false },
                            },
                        )
                        .await?;
                        Ok(Link::new(rtc, inbox))
                    }
                    .await;
                    let _ = sender.send((peer, result)).await;
                }));
            } else if !signal.offer
                && let Some(link) = room.links.get(&signal.from)
            {
                link.rtc.accept_answer(&signal.sdp).await?;
            }
        }
    }
    let mut departed = Vec::new();
    for (peer, link) in &mut room.links {
        use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
        if matches!(link.rtc.connection.connection_state(), RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
            departed.push(peer.clone());
            continue;
        }
        if !link.announced && link.rtc.is_open().await && events.try_send(Event::PeerConnected { peer: peer.clone() }).is_ok() {
            link.announced = true;
        }
        link.flush();
        while let Ok(error) = link.failures.try_recv() {
            let _ = events.try_send(Event::Error(error));
        }
        drain_link(peer, link, events);
    }
    for peer in departed {
        room.links.remove(&peer);
        if room.admission.peer == room.admission.host {
            let client = client.clone();
            let server = room.server.clone();
            let auth = crate::signaling::Auth::from(&room.admission);
            let events = events.clone();
            room.negotiation_tasks.push(tokio::spawn(async move {
                match post(&client, &server, "/kick", &crate::signaling::Kick { auth, peer: peer.clone() }).await {
                    Ok(_) => {
                        let _ = events.try_send(Event::PeerDisconnected { peer });
                    }
                    Err(error) => {
                        let _ = events.try_send(Event::Error(format!("cannot revoke disconnected member: {error}")));
                    }
                }
            }));
        } else {
            let _ = events.try_send(Event::PeerDisconnected { peer });
        }
    }
    Ok(())
}
async fn close_room(client: &reqwest::Client, room: ConnectedRoom) {
    room.poll_task.abort();
    for task in &room.negotiation_tasks {
        task.abort();
    }
    for link in room.links.values() {
        let _ = link.rtc.close().await;
    }
    let _ = post(client, &room.server, "/leave", &crate::signaling::Auth::from(&room.admission)).await;
}
async fn run_worker(
    mut requests: mpsc::Receiver<ScopedRequest>,
    mut live: mpsc::Receiver<ScopedRequest>,
    mut control: mpsc::Receiver<ScopedRequest>,
    mut events: EventSender,
) {
    let client = match reqwest::Client::builder().user_agent("PhotoCraft-dev").timeout(Duration::from_secs(20)).build() {
        Ok(c) => c,
        Err(e) => {
            let _ = events.try_send(Event::Error(e.to_string()));
            return;
        }
    };
    let mut room: Option<ConnectedRoom> = None;
    let mut embedded_service: Option<tokio::task::JoinHandle<()>> = None;
    let mut held: Option<(VecDeque<String>, ChannelKind, Vec<u8>)> = None;
    let mut interval = tokio::time::interval(Duration::from_millis(20));

    loop {
        tokio::select! {
            request = receive_request(&mut requests, &mut live, &mut control, held.is_none()) => {
                let Some(request) = request else {break;};
                if request.generation != events.latest.load(std::sync::atomic::Ordering::Acquire) {continue;}
                if !matches!(request.request, Request::Send { .. }) {events=events.scoped(request.generation);}
                match request.request {
                    Request::Create {server,password,ice_servers} => {
                        held = None;
                        discard_room_sends(&mut requests, &mut live);
                        if server.trim_end_matches('/')=="http://127.0.0.1:5548" && embedded_service.is_none() {
                            match tokio::net::TcpListener::bind("127.0.0.1:5548").await {
                                Ok(listener)=> {embedded_service=Some(tokio::spawn(async move {let _=axum::serve(listener,crate::signaling::router()).await;}));},
                                Err(error) if error.kind()==std::io::ErrorKind::AddrInUse=>{},
                                Err(error)=> {let _=events.try_send(Event::Error(format!("cannot start local room service: {error}")));continue;},
                            }
                        }
                        if let Some(old)=room.take(){close_room(&client,old).await;}
                        let opened=open_room(&client,server,None,password,ice_servers).await; discard_room_sends(&mut requests,&mut live);
                        match opened {Ok(new)=>{let _=events.try_send(Event::Joined {admission:new.admission.clone()});room=Some(new);},Err(e)=>{let _=events.try_send(Event::Error(e.to_string()));}}
                    }
                    Request::Join {server,code,password,ice_servers} => {
                        held = None;
                        discard_room_sends(&mut requests, &mut live);
                        if let Some(old)=room.take(){close_room(&client,old).await;}
                        let opened=open_room(&client,server,Some(code),password,ice_servers).await; discard_room_sends(&mut requests,&mut live);
                        match opened {Ok(new)=>{let _=events.try_send(Event::Joined {admission:new.admission.clone()});room=Some(new);},Err(e)=>{let _=events.try_send(Event::Error(e.to_string()));}}
                    }
                    Request::Send {peer,channel,payload} => {
                        if payload.len()>MAX_PACKET {let _=events.try_send(Event::Error("packet exceeds limit".into()));continue;}
                        if let Some(current)=room.as_mut(){
                            let mut peers:VecDeque<_>=current.links.keys().filter(|id|peer.as_ref().is_none_or(|wanted|wanted==*id)).cloned().collect();
                            while let Some(id)=peers.pop_front(){
                                if let Some(link)=current.links.get_mut(&id) && link.enqueue(channel,payload.clone()).is_err(){ peers.push_front(id); held=Some((peers,channel,payload));break; }
                            }
                        }
                    }
                    Request::Close => {held=None;discard_room_sends(&mut requests,&mut live);if let Some(old)=room.take(){close_room(&client,old).await;}let _=events.try_send(Event::Closed);}
                }
            }
            _ = interval.tick() => {
                if let Some(current)=room.as_mut(){
                    if let Err(e)=tick_room(&client,current,&events).await {let _=events.try_send(Event::Error(e.to_string()));}
                    if let Some((mut peers,channel,payload))=held.take(){
                        while let Some(id)=peers.pop_front(){
                            if let Some(link)=current.links.get_mut(&id) && link.enqueue(channel,payload.clone()).is_err(){peers.push_front(id);held=Some((peers,channel,payload));break;}
                        }
                    }
                }
            }
        }
    }
    if let Some(old) = room {
        close_room(&client, old).await;
    }
    if let Some(service) = embedded_service {
        service.abort();
    }
}

fn drain_link(peer: &str, link: &mut Link, events: &EventSender) {
    loop {
        let event = if let Some(event) = link.pending.take() {
            event
        } else if let Ok(packet) = link.inbox.try_recv() {
            Event::Packet { peer: peer.to_owned(), channel: packet.channel, payload: packet.payload }
        } else {
            break;
        };
        match events.try_send(event) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(event)) => {
                link.pending = Some(event);
                break;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    #[test]
    fn event_depth_tracks_successful_enqueues_and_failed_backpressure() {
        let (sender, receiver) = event_channel(1, Arc::new(std::sync::atomic::AtomicU64::new(0)));
        assert!(receiver.is_empty());
        sender.try_send(Event::Closed).unwrap();
        assert_eq!(receiver.len(), 1);
        assert!(sender.try_send(Event::Closed).is_err());
        assert_eq!(receiver.len(), 1);
        receiver.try_recv().unwrap();
        assert_eq!(receiver.len(), 0);
    }
    #[test]
    fn room_generation_rejects_queued_and_late_callbacks_of_every_kind() {
        use std::sync::atomic::Ordering;
        let latest = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (old, receiver) = event_channel(16, latest.clone());
        let events = || {
            vec![
                Event::Closed,
                Event::Joined {
                    admission: crate::signaling::Admission { code: "OLD001".into(), peer: "old".into(), token: "test".into(), host: "old".into() },
                },
                Event::Error("old 401".into()),
                Event::Packet { peer: "old".into(), channel: ChannelKind::Edits, payload: vec![1] },
                Event::PeerConnected { peer: "old".into() },
                Event::PeerDisconnected { peer: "old".into() },
            ]
        };
        for event in events() {
            old.try_send(event).unwrap();
        }
        latest.store(1, Ordering::Release);
        for event in events() {
            old.try_send(event).unwrap();
        }
        let current = old.scoped(1);
        current.try_send(Event::Error("current".into())).unwrap();
        assert!(matches!(receiver.try_recv(), Ok(Event::Error(error)) if error=="current"));
        assert!(receiver.is_empty());
        assert!(matches!(receiver.recv_timeout(Duration::from_millis(1)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)));
        // An already queued stale item is also filtered by the blocking receiver.
        current.try_send(Event::Closed).unwrap();
        latest.store(2, Ordering::Release);
        current.scoped(2).try_send(Event::Error("newest".into())).unwrap();
        assert!(matches!(receiver.recv_timeout(Duration::from_secs(1)),Ok(Event::Error(error)) if error=="newest"));
        assert!(receiver.is_empty());
    }

    #[test]
    fn rejected_control_does_not_advance_epoch_and_sends_use_accepted_epoch() {
        use std::sync::atomic::Ordering;
        let (edits, mut edit_rx) = mpsc::channel(1);
        let (live, _live_rx) = mpsc::channel(1);
        let (control, mut control_rx) = mpsc::channel(1);
        let latest = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sender = RequestSender { edits, live, control, latest: latest.clone(), enqueue: Arc::new(std::sync::Mutex::new(())) };
        sender.try_send(Request::Close).unwrap();
        assert_eq!(latest.load(Ordering::Acquire), 1);
        assert!(matches!(sender.try_send(Request::Close), Err(mpsc::error::TrySendError::Full(Request::Close))));
        assert_eq!(latest.load(Ordering::Acquire), 1);
        sender.try_send(Request::Send { peer: None, channel: ChannelKind::Edits, payload: vec![9] }).unwrap();
        assert_eq!(edit_rx.try_recv().unwrap().generation, 1);
        assert_eq!(control_rx.try_recv().unwrap().generation, 1);
        latest.store(u64::MAX, Ordering::Release);
        assert!(matches!(sender.try_send(Request::Close), Err(mpsc::error::TrySendError::Closed(Request::Close))));
        assert_eq!(latest.load(Ordering::Acquire), u64::MAX);
        assert!(control_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn blocked_reliable_requests_leave_control_and_voice_live() {
        let (edits, mut edit_rx) = mpsc::channel(1);
        let (live, mut live_rx) = mpsc::channel(1);
        let (control, mut control_rx) = mpsc::channel(1);
        let sender =
            RequestSender { edits, live, control, latest: Arc::new(std::sync::atomic::AtomicU64::new(0)), enqueue: Arc::new(std::sync::Mutex::new(())) };
        sender.try_send(Request::Send { peer: None, channel: ChannelKind::Edits, payload: vec![1] }).unwrap();
        assert!(sender.try_send(Request::Send { peer: None, channel: ChannelKind::Edits, payload: vec![2] }).is_err());
        sender.try_send(Request::Send { peer: None, channel: ChannelKind::Voice, payload: vec![3] }).unwrap();
        sender.try_send(Request::Close).unwrap();
        assert!(matches!(receive_request(&mut edit_rx, &mut live_rx, &mut control_rx, false).await, Some(ScopedRequest { request: Request::Close, .. })));
        assert!(matches!(
            receive_request(&mut edit_rx, &mut live_rx, &mut control_rx, false).await,
            Some(ScopedRequest { request: Request::Send { channel: ChannelKind::Voice, .. }, .. })
        ));
        assert!(matches!(edit_rx.try_recv(),Ok(ScopedRequest {request:Request::Send {channel:ChannelKind::Edits,payload,..},..}) if payload == vec![1]));
    }
    #[tokio::test]
    async fn saturated_edit_lane_preserves_order_and_does_not_block_live_lane() {
        let (rtc, inbox) = RtcPeer::new(Vec::new()).await.unwrap();
        let (reliable, mut edits) = mpsc::channel(1);
        let (lossy, mut live) = mpsc::channel(1);
        let (_, failures) = mpsc::channel(1);
        let mut link =
            Link { rtc: Arc::new(rtc), inbox, announced: false, pending: None, reliable, lossy, queued: VecDeque::new(), failures, writers: Vec::new() };
        for index in 0u8..33 {
            assert!(link.enqueue(ChannelKind::Edits, vec![index]).is_ok());
        }
        assert_eq!(link.enqueue(ChannelKind::Edits, vec![33]), Err(vec![33]));
        assert!(link.enqueue(ChannelKind::Voice, vec![77]).is_ok());
        assert_eq!(live.try_recv().unwrap(), (ChannelKind::Voice, vec![77]));
        for index in 0u8..33 {
            assert_eq!(edits.try_recv().unwrap(), vec![index]);
            link.flush();
        }
        assert!(link.queued.is_empty());
        link.rtc.close().await.unwrap();
    }
}
