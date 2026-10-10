//! A reproducible 32-artist test over real WebRTC, producing native-engine PNG frames.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use photocraft_collab::{
    transport::{ChannelKind, Incoming, RtcPeer, TransportResult},
    *,
};
use photocraft_engine::Session;
use photocraft_paint::{BrushSettings, StrokePoint, TipShape};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
const ARTISTS: usize = 32;
const STEPS: usize = 12;
const POINTS: usize = 96;
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 512;
struct Link {
    host: RtcPeer,
    client: RtcPeer,
    host_inbox: mpsc::Receiver<Incoming>,
    client_inbox: mpsc::Receiver<Incoming>,
}
async fn receive_wire(inbox: &mut mpsc::Receiver<Incoming>) -> TransportResult<wire::Wire> {
    let mut decoder = wire::Decoder::default();
    loop {
        if let Some(message) = decoder.push("sender", &receive(inbox).await?)? {
            return Ok(message);
        }
    }
}
async fn deliver_host(link: &mut Link, replica: &mut Session, event: &HostMessage) -> TransportResult<usize> {
    let packets = wire::encode_reliable(event.revision, &wire::Wire::Host(event.clone()))?;
    let size = packets.iter().map(Vec::len).sum();
    let sender = &link.host;
    let inbox = &mut link.client_inbox;
    let sending = async {
        for packet in packets {
            sender.send(ChannelKind::Edits, &packet).await?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    };
    let (_, received) = tokio::try_join!(sending, receive_wire(inbox))?;
    let wire::Wire::Host(event) = received else {
        return Err("expected canonical host event".into());
    };
    replica.collaboration_receive(&event)?;
    Ok(size)
}
fn render(session: &Session, path: &Path) -> TransportResult<()> {
    let doc = &session.active().ok_or("no active document")?.doc;
    let rgba = photocraft_compose::flatten(doc).over_background([0.96, 0.97, 0.99]).to_rgba8();
    image::save_buffer(path, &rgba.pixels, rgba.width, rgba.height, image::ColorType::Rgba8)?;
    Ok(())
}
fn doodle(artist: usize) -> Vec<StrokePoint> {
    let cx = 64.0 + (artist % 8) as f64 * 128.0;
    let cy = 64.0 + (artist / 8) as f64 * 128.0;
    (0..POINTS)
        .map(|i| {
            let t = i as f64 / (POINTS - 1) as f64;
            let a = t * std::f64::consts::TAU;
            let (x, y) = match artist % 6 {
                0 => (a.cos() * 43.0, a.sin() * 43.0),
                1 => {
                    let r = 8.0 + t * 37.0;
                    ((a * 3.0).cos() * r, (a * 3.0).sin() * r)
                }
                2 => (32.0 * a.sin().powi(3), -(26.0 * a.cos() - 10.0 * (2.0 * a).cos() - 4.0 * (3.0 * a).cos() - 2.0 * (4.0 * a).cos())),
                3 => {
                    let r = 28.0 + 14.0 * (5.0 * a).cos();
                    (r * a.cos(), r * a.sin())
                }
                4 => (42.0 * (a * 3.0).sin(), 42.0 * (a * 2.0).sin()),
                _ => {
                    let r = 34.0 + 10.0 * (3.0 * a).cos();
                    (r * a.cos(), r * a.sin())
                }
            };
            StrokePoint {
                x: cx + x,
                y: cy + y,
                pressure: 0.35 + 0.65 * (std::f32::consts::PI * t as f32).sin(),
                tilt_x: 25.0 * (a.sin() as f32),
                tilt_y: 15.0 * (a.cos() as f32),
                rotation: t as f32 * 360.0,
                time: i as f64 * 8.0,
                ..Default::default()
            }
        })
        .collect()
}
fn brush(artist: usize) -> BrushSettings {
    let hue = artist as f32 / ARTISTS as f32 * std::f32::consts::TAU;
    let color = [0.48 + 0.40 * hue.cos(), 0.48 + 0.40 * (hue + 2.094).cos(), 0.48 + 0.40 * (hue + 4.188).cos(), 1.0];
    BrushSettings {
        size: 3.0 + (artist % 5) as f32 * 1.6,
        hardness: if artist.is_multiple_of(4) { 0.15 } else { 0.85 },
        opacity: 0.8,
        pressure_size: true,
        pressure_opacity: true,
        seed: 1000 + artist as u64,
        color,
        tip: if artist.is_multiple_of(3) { TipShape::Sampled(photocraft_paint::GrayTile::from_fn(8, 8, |_, _| 1.0)) } else { TipShape::Round },
        noise: artist % 4 == 1,
        wet_edges: artist % 4 == 2,
        roundness: if artist % 4 == 3 { 0.35 } else { 1.0 },
        angle: artist as f32 * 13.0,
        ..Default::default()
    }
}
async fn receive(inbox: &mut mpsc::Receiver<Incoming>) -> TransportResult<Vec<u8>> {
    Ok(tokio::time::timeout(Duration::from_secs(10), inbox.recv()).await?.ok_or("RTC inbox closed")?.payload)
}
#[tokio::main]
async fn main() -> TransportResult<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--join") {
        return join_native(&args).await;
    }
    let output = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("target/collaboration-evidence"));
    std::fs::create_dir_all(&output)?;
    let started = Instant::now();
    let mut host = Session::new();
    host.execute("file.new", serde_json::json!({"width":WIDTH,"height":HEIGHT,"fill":"transparent"}))?;
    let base = (*host.active().ok_or("missing document")?.doc).clone();
    let layer = host.active().and_then(|d| d.active_layer).ok_or("missing layer")?;
    host.collaboration.document_id = host.active().map(|s| s.doc.id);
    host.collaboration.room = Some(RoomState::new("DOODLES".into(), "artist-0".into(), "artist-0".into()));
    let mut replicas = Vec::new();
    let mut links = Vec::new();
    for artist in 1..ARTISTS {
        let mut session = Session::new();
        session.add_document(base.clone(), None);
        session.collaboration.document_id = session.active().map(|s| s.doc.id);
        session.collaboration.room = Some(RoomState::new("DOODLES".into(), "artist-0".into(), format!("artist-{artist}")));
        replicas.push(session);
        let (host_rtc, host_inbox) = RtcPeer::new(vec![]).await?;
        let (client, client_inbox) = RtcPeer::new(vec![]).await?;
        let offer = client.offer().await?;
        let answer = host_rtc.answer(&offer).await?;
        client.accept_answer(&answer).await?;
        links.push(Link { host: host_rtc, client, host_inbox, client_inbox });
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut open = true;
            for link in &links {
                open &= link.host.is_open().await && link.client.is_open().await;
            }
            if open {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let connected_ms = started.elapsed().as_millis();
    let strokes: Vec<_> = (0..ARTISTS).map(doodle).collect();
    let mut nonces = [0u64; ARTISTS];
    let mut wire_bytes = 0usize;
    let mut latency = Vec::new();
    let mut events = 0usize;
    render(&host, &output.join("frame-000.png"))?;
    for stage in 0..STEPS + 2 {
        // All artists submit their current stroke segment before the host receives any.
        for artist in 0..ARTISTS {
            let id = format!("artist-{artist}-stroke");
            let operation = if stage == 0 {
                Operation::StrokeBegin {
                    stroke: Box::new(StrokeStart { id, layer, brush: brush(artist), selection: Vec::new(), lock_transparency: false, zoom: 1.0 }),
                }
            } else if stage == STEPS + 1 {
                Operation::StrokeEnd { id, sequence: STEPS as u32 }
            } else {
                let start = (stage - 1) * POINTS / STEPS;
                let stop = stage * POINTS / STEPS;
                Operation::StrokeChunk {
                    id,
                    sequence: (stage - 1) as u32,
                    points: strokes.get(artist).and_then(|s| s.get(start..stop)).ok_or("doodle range invalid")?.to_vec(),
                }
            };
            let nonce = nonces.get_mut(artist).ok_or("artist out of bounds")?;
            *nonce += 1;
            let message = ClientMessage { version: PROTOCOL_VERSION, peer: format!("artist-{artist}"), nonce: *nonce, operation };
            if artist == 0 {
                host.collaboration_accept("artist-0", message)?;
            } else {
                for payload in wire::encode_reliable(*nonce, &wire::Wire::Client(message))? {
                    wire_bytes += payload.len();
                    links.get(artist - 1).ok_or("link unavailable")?.client.send(ChannelKind::Edits, &payload).await?;
                }
            }
        }
        let tick = Instant::now();
        for (i, link) in links.iter_mut().enumerate() {
            let wire::Wire::Client(message) = receive_wire(&mut link.host_inbox).await? else {
                return Err("expected client event".into());
            };
            host.collaboration_accept(&format!("artist-{}", i + 1), message)?;
        }
        let canonical = std::mem::take(&mut host.collaboration.canonical_outbox);
        for event in &canonical {
            events += 1;
            for (link, replica) in links.iter_mut().zip(&mut replicas) {
                wire_bytes += deliver_host(link, replica, event).await?;
            }
        }
        latency.push(tick.elapsed().as_secs_f64() * 1000.0);
        render(&host, &output.join(format!("frame-{:03}.png", stage + 1)))?;
    }
    let expected = photocraft_compose::flatten(&host.active().ok_or("missing host")?.doc).px;
    for replica in &replicas {
        if photocraft_compose::flatten(&replica.active().ok_or("missing replica")?.doc).px != expected {
            return Err("32-client canvas divergence".into());
        }
    }
    render(replicas.first().ok_or("missing first replica")?, &output.join("client-final.png"))?;
    render(&host, &output.join("host-final.png"))?;
    let stroke_wire_bytes = wire_bytes;
    // Per-author undo must converge without erasing any other artist's work.
    nonces[0] += 1;
    host.collaboration_accept("artist-0", ClientMessage { version: PROTOCOL_VERSION, peer: "artist-0".into(), nonce: nonces[0], operation: Operation::Undo })?;
    for event in std::mem::take(&mut host.collaboration.canonical_outbox) {
        for (link, replica) in links.iter_mut().zip(&mut replicas) {
            wire_bytes += deliver_host(link, replica, &event).await?;
        }
    }
    let undo = photocraft_compose::flatten(&host.active().ok_or("missing host")?.doc).px;
    for replica in &replicas {
        if photocraft_compose::flatten(&replica.active().ok_or("missing replica")?.doc).px != undo {
            return Err("per-author undo divergence".into());
        }
    }
    render(&host, &output.join("host-own-undo.png"))?;
    latency.sort_by(f64::total_cmp);
    let median = latency.get(latency.len() / 2).copied().unwrap_or(0.0);
    let maximum = latency.last().copied().unwrap_or(0.0);
    let report = serde_json::json!({"artists":ARTISTS,"realWebrtcConnections":links.len(),"authoritativeOperations":events,"pressurePoints":POINTS*ARTISTS,"applicationWireBytes":wire_bytes,"strokeWireBytes":stroke_wire_bytes,"undoWireBytes":wire_bytes.saturating_sub(stroke_wire_bytes),"connectionSetupMs":connected_ms,"stageReceiveApplyBroadcastMs":{"median":median,"max":maximum},"all32CanvasesEqual":true,"perAuthorUndoConverged":true,"elapsedSeconds":started.elapsed().as_secs_f64(),"note":"timings include serial native engine replay on all 32 replicas; brush events send inputs, canonical undo sends a document delta"});
    std::fs::write(output.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    for link in links {
        link.host.close().await?;
        link.client.close().await?;
    }
    Ok(())
}

struct Bot {
    worker: photocraft_collab::transport::Worker,
    admission: Option<photocraft_collab::signaling::Admission>,
    session: Session,
    decoder: photocraft_collab::wire::Decoder,
    transfer: u64,
    ready: bool,
    connected: bool,
    packets: usize,
    profiled: bool,
}
impl Bot {
    fn receive(&mut self) -> TransportResult<usize> {
        use photocraft_collab::{
            transport::Event,
            wire::{Wire, decode_unreliable},
        };
        let mut received = 0;
        for _ in 0..256 {
            let Ok(event) = self.worker.events.try_recv() else {
                break;
            };
            match event {
                Event::Joined { admission } => self.admission = Some(admission),
                Event::PeerConnected { .. } => self.connected = true,
                Event::Packet { peer, channel, payload } => {
                    self.packets += 1;
                    let wire = if channel == ChannelKind::Edits { self.decoder.push(&peer, &payload)? } else { Some(decode_unreliable(&payload)?) };
                    match wire {
                        Some(Wire::Checkpoint { document, mut room, checkpoint }) => {
                            let admission = self.admission.as_ref().ok_or("checkpoint before admission")?;
                            let limits = photocraft_format::LoadOptions {
                                max_manifest_bytes: 8 << 20,
                                max_blob_bytes: 64 << 20,
                                max_total_bytes: 512 << 20,
                                preserve_ids: true,
                            };
                            let anchor = if document.is_empty() { &checkpoint.baseline } else { &document };
                            let doc = photocraft_format::load_from_bytes_with(anchor, &limits)?;
                            room.local_peer = admission.peer.clone();
                            room.host = admission.host.clone();
                            room.code = admission.code.clone();
                            self.session.add_document(doc, None);
                            self.session.collaboration.document_id = self.session.active().map(|s| s.doc.id);
                            self.session.collaboration.room = Some(room);
                            self.session.collaboration_install_checkpoint(checkpoint)?;
                            self.ready = true;
                        }
                        Some(Wire::Bootstrap { document, events, mut room }) => {
                            let admission = self.admission.as_ref().ok_or("bootstrap before admission")?;
                            let limits = photocraft_format::LoadOptions {
                                max_manifest_bytes: 8 << 20,
                                max_blob_bytes: 64 << 20,
                                max_total_bytes: 512 << 20,
                                preserve_ids: true,
                            };
                            let doc = photocraft_format::load_from_bytes_with(&document, &limits)?;
                            room.local_peer = admission.peer.clone();
                            room.host = admission.host.clone();
                            room.code = admission.code.clone();
                            room.chat.clear();
                            room.notes.clear();
                            self.session.add_document(doc, None);
                            self.session.collaboration.document_id = self.session.active().map(|s| s.doc.id);
                            self.session.collaboration.room = Some(room);
                            for event in events {
                                self.session.collaboration_receive(&event)?;
                            }
                            self.ready = true;
                        }
                        Some(Wire::Host(event)) => {
                            self.session.collaboration_receive(&event)?;
                            received += 1;
                        }
                        Some(Wire::Reject(error)) => return Err(format!("native host rejected bot: {error}").into()),
                        _ => {}
                    }
                }
                Event::Error(error) => return Err(error.into()),
                Event::PeerDisconnected { .. } => return Err("native host disconnected".into()),
                _ => {}
            }
        }
        Ok(received)
    }
    fn send(&mut self, operation: Operation) -> TransportResult<()> {
        let message = self.session.collaboration.submit(operation)?;
        self.transfer = self.transfer.checked_add(1).ok_or("transfer overflow")?;
        for payload in photocraft_collab::wire::encode_reliable(self.transfer, &photocraft_collab::wire::Wire::Client(message))? {
            self.worker.requests.try_send(photocraft_collab::transport::Request::Send { peer: None, channel: ChannelKind::Edits, payload })?;
        }
        Ok(())
    }
}
async fn join_native(args: &[String]) -> TransportResult<()> {
    use photocraft_collab::{
        transport::{Request, Worker},
        wire::{Wire, encode_unreliable},
    };
    let server = args.get(1).ok_or("usage: --join <server-url> <room-code> [password-or-dash] [output-dir]")?.clone();
    let code = args.get(2).ok_or("room code required")?.clone();
    let password = args.get(3).filter(|p| p.as_str() != "-").cloned();
    let output = args.get(4).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("target/collaboration-native-bots"));
    std::fs::create_dir_all(&output)?;
    let started = Instant::now();
    let mut bots = Vec::new();
    for _ in 0..30 {
        let worker = Worker::start()?;
        worker.requests.try_send(Request::Join { server: server.clone(), code: code.clone(), password: password.clone(), ice_servers: vec![] })?;
        bots.push(Bot {
            worker,
            admission: None,
            session: Session::new(),
            decoder: Default::default(),
            transfer: 0,
            ready: false,
            connected: false,
            packets: 0,
            profiled: false,
        });
    }
    let strokes: Vec<_> = (0..30).map(doodle).collect();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut progress_at = Instant::now();
    loop {
        for (i, bot) in bots.iter_mut().enumerate() {
            bot.receive()?;
            if bot.ready && !bot.profiled {
                let admission = bot.admission.as_ref().ok_or("bot not admitted")?;
                let presence = Presence {
                    profile: Profile { name: format!("Doodle {:02}", i + 1), color: brush(i).color, icon: Some("✦".into()), ..Default::default() },
                    position: strokes.get(i).and_then(|s| s.first()).map(|p| [p.x, p.y]),
                    layer: bot.session.active().and_then(|s| s.active_layer),
                    tool: "brush".into(),
                    speaking: false,
                };
                bot.worker.requests.try_send(Request::Send {
                    peer: None,
                    channel: ChannelKind::Presence,
                    payload: encode_unreliable(&Wire::Presence { peer: admission.peer.clone(), presence })?,
                })?;
                bot.profiled = true;
            }
        }
        let ready = bots.iter().filter(|b| b.ready).count();
        if progress_at.elapsed() > Duration::from_secs(1) {
            println!(
                "native bots: admitted={} RTC={} bootstrapped={} packets={:?}",
                bots.iter().filter(|b| b.admission.is_some()).count(),
                bots.iter().filter(|b| b.connected).count(),
                ready,
                bots.iter().map(|b| b.packets).collect::<Vec<_>>()
            );
            progress_at = Instant::now();
        }
        if ready == 30 {
            break;
        }
        if Instant::now() > deadline {
            return Err(format!("only {ready}/30 native-room bots received bootstrap").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!("30 real WebRTC bots bootstrapped from native host in {:?}", started.elapsed());
    for (i, bot) in bots.iter_mut().enumerate() {
        let admission = bot.admission.as_ref().ok_or("bot not admitted")?;
        let presence = Presence {
            profile: Profile {
                name: format!("Doodle {:02}", i + 1),
                color: brush(i).color,
                icon: Some(if i.is_multiple_of(2) { "✦" } else { "●" }.into()),
                ..Default::default()
            },
            position: strokes.get(i).and_then(|s| s.first()).map(|p| [p.x, p.y]),
            layer: bot.session.active().and_then(|s| s.active_layer),
            tool: "brush".into(),
            speaking: false,
        };
        bot.worker.requests.try_send(Request::Send {
            peer: None,
            channel: ChannelKind::Presence,
            payload: encode_unreliable(&Wire::Presence { peer: admission.peer.clone(), presence })?,
        })?;
    }
    let step_ms = std::env::var("PHOTOCRAFT_DOODLE_STEP_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(250).clamp(50, 5000);
    let initial = bots.first().ok_or("no bots")?.session.collaboration.applied_revision;
    let mut received = 0usize;
    let mut frames = 0;
    for stage in 0..STEPS + 2 {
        for (artist, bot) in bots.iter_mut().enumerate() {
            let layer = bot.session.active().and_then(|s| s.active_layer).ok_or("native target layer missing")?;
            let id = format!("{}-doodle", bot.admission.as_ref().ok_or("bot not admitted")?.peer);
            let operation = if stage == 0 {
                Operation::StrokeBegin {
                    stroke: Box::new(StrokeStart { id, layer, brush: brush(artist), selection: Vec::new(), lock_transparency: false, zoom: 1.0 }),
                }
            } else if stage == STEPS + 1 {
                Operation::StrokeEnd { id, sequence: STEPS as u32 }
            } else {
                let start = (stage - 1) * POINTS / STEPS;
                let stop = stage * POINTS / STEPS;
                Operation::StrokeChunk {
                    id,
                    sequence: (stage - 1) as u32,
                    points: strokes.get(artist).and_then(|s| s.get(start..stop)).ok_or("doodle range invalid")?.to_vec(),
                }
            };
            bot.send(operation)?;
            if let Some(point) = strokes.get(artist).and_then(|s| s.get((stage.saturating_sub(1) * POINTS / STEPS).min(POINTS - 1))) {
                let peer = bot.admission.as_ref().ok_or("bot not admitted")?.peer.clone();
                let presence = Presence {
                    profile: Profile { name: format!("Doodle {:02}", artist + 1), color: brush(artist).color, icon: Some("✦".into()), ..Default::default() },
                    position: Some([point.x, point.y]),
                    layer: Some(layer),
                    tool: "brush".into(),
                    speaking: false,
                };
                bot.worker.requests.try_send(Request::Send {
                    peer: None,
                    channel: ChannelKind::Presence,
                    payload: encode_unreliable(&Wire::Presence { peer, presence })?,
                })?;
            }
        }
        // Keep consuming authoritative edits while artists progress in parallel.
        let until = Instant::now() + Duration::from_millis(step_ms);
        while Instant::now() < until {
            for bot in &mut bots {
                received += bot.receive()?;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if let Some(bot) = bots.first() {
            render(&bot.session, &output.join(format!("native-frame-{frames:03}.png")))?;
            frames += 1;
        }
    }
    let target = initial + ((STEPS + 2) * 30) as u64;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        for bot in &mut bots {
            received += bot.receive()?;
        }
        if bots.iter().all(|b| b.session.collaboration.applied_revision >= target) {
            break;
        }
        if Instant::now() > deadline {
            return Err(format!(
                "native broadcast convergence timed out: revisions {:?}, expected {target}",
                bots.iter().map(|b| b.session.collaboration.applied_revision).collect::<Vec<_>>()
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Quiet-room settling makes transient revision differences observable before comparison.
    for _ in 0..5 {
        for bot in &mut bots {
            received += bot.receive()?;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let first = bots.first().ok_or("no bots")?;
    let expected = photocraft_compose::flatten(&first.session.active().ok_or("no bot document")?.doc).px;
    for bot in &bots {
        if photocraft_compose::flatten(&bot.session.active().ok_or("no bot document")?.doc).px != expected {
            return Err("native-room bot canvases diverged".into());
        }
    }
    render(&first.session, &output.join("native-bots-final.png"))?;
    let report = serde_json::json!({"extraRealWebrtcClients":30,"nativeVisibleClientsExpected":2,"strokes":30,"pressurePoints":30*POINTS,"chunksPerStroke":STEPS,"allBotCanvasesEqual":true,"finalRevision":first.session.collaboration.applied_revision,"hostOperationsReceivedAcrossBots":received,"elapsedSeconds":started.elapsed().as_secs_f64(),"roomCode":code});
    std::fs::write(output.join("native-report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    // Leave bots alive for five seconds so the native recording includes the finished artwork.
    for _ in 0..50 {
        for bot in &mut bots {
            let _ = bot.receive();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for bot in bots {
        let _ = bot.worker.requests.try_send(Request::Close);
    }
    Ok(())
}
