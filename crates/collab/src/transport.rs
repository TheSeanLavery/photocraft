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
    peer_connection::{RTCPeerConnection, configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription},
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
        let connection = Arc::new(APIBuilder::new().build().new_peer_connection(RTCConfiguration { ice_servers, ..Default::default() }).await?);
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
            tokio::time::timeout(Duration::from_secs(5), async {
                while channel.buffered_amount().await > MAX_BUFFERED {
                    if channel.ready_state() != RTCDataChannelState::Open {
                        return Err("data channel closed while draining");
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Ok::<(), &str>(())
            })
            .await??;
        }
        channel.send(&Bytes::copy_from_slice(payload)).await?;
        Ok(())
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
pub struct Worker {
    pub requests: mpsc::Sender<Request>,
    pub events: std::sync::mpsc::Receiver<Event>,
}
impl Worker {
    pub fn start() -> std::io::Result<Self> {
        let (requests, receiver) = mpsc::channel(512);
        let (sender, events) = std::sync::mpsc::sync_channel(512);
        std::thread::Builder::new().name("photocraft-webrtc".into()).spawn(move || {
            match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(runtime) => runtime.block_on(run_worker(receiver, sender)),
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
async fn open_room(
    client: &reqwest::Client,
    server: String,
    code: Option<String>,
    password: Option<String>,
    ice_servers: Vec<RTCIceServer>,
) -> TransportResult<ConnectedRoom> {
    let endpoint = if code.is_some() { "/join" } else { "/create" };
    let admission = post(client, &server, endpoint, &crate::signaling::RoomRequest { code, password }).await?.json::<crate::signaling::Admission>().await?;
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
async fn tick_room(client: &reqwest::Client, room: &mut ConnectedRoom, events: &std::sync::mpsc::SyncSender<Event>) -> TransportResult<()> {
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
async fn run_worker(mut requests: mpsc::Receiver<Request>, events: std::sync::mpsc::SyncSender<Event>) {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
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
            request = requests.recv(), if held.is_none() => {
                let Some(request) = request else {break;};
                match request {
                    Request::Create {server,password,ice_servers} => {
                        if server.trim_end_matches('/')=="http://127.0.0.1:5548" && embedded_service.is_none() {
                            match tokio::net::TcpListener::bind("127.0.0.1:5548").await {
                                Ok(listener)=> {embedded_service=Some(tokio::spawn(async move {let _=axum::serve(listener,crate::signaling::router()).await;}));},
                                Err(error) if error.kind()==std::io::ErrorKind::AddrInUse=>{},
                                Err(error)=> {let _=events.try_send(Event::Error(format!("cannot start local room service: {error}")));continue;},
                            }
                        }
                        if let Some(old)=room.take(){close_room(&client,old).await;}
                        match open_room(&client,server,None,password,ice_servers).await {Ok(new)=>{let _=events.try_send(Event::Joined {admission:new.admission.clone()});room=Some(new);},Err(e)=>{let _=events.try_send(Event::Error(e.to_string()));}}
                    }
                    Request::Join {server,code,password,ice_servers} => {
                        if let Some(old)=room.take(){close_room(&client,old).await;}
                        match open_room(&client,server,Some(code),password,ice_servers).await {Ok(new)=>{let _=events.try_send(Event::Joined {admission:new.admission.clone()});room=Some(new);},Err(e)=>{let _=events.try_send(Event::Error(e.to_string()));}}
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
                    Request::Close => {if let Some(old)=room.take(){close_room(&client,old).await;}let _=events.try_send(Event::Closed);}
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

fn drain_link(peer: &str, link: &mut Link, events: &std::sync::mpsc::SyncSender<Event>) {
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
