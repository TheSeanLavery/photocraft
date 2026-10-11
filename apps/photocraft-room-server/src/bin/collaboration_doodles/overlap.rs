//! Aggressive deterministic same-layer overlap, with actual RTC and native-room modes.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
const CHUNKS: usize = 24;
const SAMPLES: usize = 192;
const ROUNDS: usize = 4;
fn number(args: &[String], flag: &str, default: usize, max: usize) -> TransportResult<usize> {
    let Some(i) = args.iter().position(|s| s == flag) else {
        return Ok(default);
    };
    let n = args.get(i + 1).ok_or("stress option missing value")?.parse::<usize>()?;
    if n == 0 || n > max {
        return Err(format!("{flag} must be between 1 and {max}").into());
    }
    Ok(n)
}
#[derive(Default)]
struct Metrics {
    pending: BTreeMap<(String, u64), Instant>,
    revisions: BTreeSet<u64>,
    latency: Vec<f64>,
    bytes: usize,
    submitted: usize,
    duplicates: usize,
    queue_high: usize,
    rtc_buffer_high: usize,
    host_inbox_high: usize,
    canonical_high: usize,
}
impl Metrics {
    fn submitted(&mut self, m: &ClientMessage) {
        self.pending.insert((m.peer.clone(), m.nonce), Instant::now());
        self.submitted += 1;
        self.queue_high = self.queue_high.max(self.pending.len());
    }
    fn received(&mut self, e: &HostMessage) {
        if !self.revisions.insert(e.revision) {
            self.duplicates += 1;
        }
        if let Some(at) = self.pending.remove(&(e.message.peer.clone(), e.message.nonce)) {
            self.latency.push(at.elapsed().as_secs_f64() * 1000.0);
        }
    }
    fn report(&mut self, elapsed: Duration) -> serde_json::Value {
        self.latency.sort_by(f64::total_cmp);
        let p = |q: f64| self.latency.get(((self.latency.len().saturating_sub(1)) as f64 * q).ceil() as usize).copied().unwrap_or(0.0);
        serde_json::json!({"submitted":self.submitted,"canonicalRevisions":self.revisions.len(),"duplicateCanonicalRevisions":self.duplicates,"unacknowledged":self.pending.len(),"outstandingOperationHighWater":self.queue_high,"rtcBufferedBytesHighWater":self.rtc_buffer_high,"hostRtcInboxPacketsHighWater":self.host_inbox_high,"hostCanonicalQueueHighWater":self.canonical_high,"applicationWireBytes":self.bytes,"ackLatencyMs":{"p50":p(0.5),"p95":p(0.95),"p99":p(0.99),"max":p(1.0)},"canonicalOperationsPerSecond":self.revisions.len() as f64 / elapsed.as_secs_f64().max(0.001)})
    }
}
fn points(artist: usize, round: usize, width: u32, height: u32) -> Vec<StrokePoint> {
    (0..SAMPLES)
        .map(|i| {
            let t = i as f64 / (SAMPLES - 1) as f64;
            let a = t * std::f64::consts::TAU * (3.0 + artist as f64) + round as f64 * 0.63;
            StrokePoint {
                x: width as f64 * (0.5 + 0.47 * a.sin()),
                y: height as f64 * (0.5 + 0.47 * (a * (1.33 + artist as f64 * 0.15)).cos()),
                pressure: 0.15 + 0.85 * (t * 17.0 + artist as f64).sin().abs() as f32,
                tilt_x: (a.sin() * 45.0) as f32,
                tilt_y: (a.cos() * 35.0) as f32,
                rotation: (t * 360.0) as f32,
                time: i as f64 * 4.0,
                ..Default::default()
            }
        })
        .collect()
}
fn settings(artist: usize, round: usize) -> BrushSettings {
    let mut b = brush(artist * 7 + round);
    b.size = 18.0 + artist as f32 * 9.0;
    b.spacing = 0.15;
    b.opacity = 0.35 + artist as f32 * 0.15;
    b.erase = (artist + round) % 3 == 2;
    b.seed = 98700 + (artist * 1024 + round) as u64;
    b.flow = 0.65;
    b.scattering.enabled = artist == 1;
    b.scattering.both_axes = true;
    b.scattering.count = 2;
    b.scattering.scatter.jitter = 0.25;
    b.dual_brush.enabled = artist == 3;
    b.dual_brush.size = 13.0;
    b.dual_brush.hardness = 0.25;
    b
}
fn operation(artist: usize, round: usize, stage: usize, layer: photocraft_doc::LayerId, samples: &[StrokePoint], peer: &str) -> TransportResult<Operation> {
    let id = format!("{peer}-overlap-{round}");
    Ok(if stage == 0 {
        Operation::StrokeBegin {
            stroke: Box::new(StrokeStart { id, layer, brush: settings(artist, round), selection: vec![], lock_transparency: false, zoom: 1.0 }),
        }
    } else if stage == CHUNKS + 1 {
        Operation::StrokeEnd { id, sequence: CHUNKS as u32 }
    } else {
        let start = (stage - 1) * SAMPLES / CHUNKS;
        Operation::StrokeChunk {
            id,
            sequence: (stage - 1) as u32,
            points: samples.get(start..start + SAMPLES / CHUNKS).ok_or("invalid stress sample range")?.to_vec(),
        }
    })
}
fn equal(host: &Session, clients: &[Session]) -> TransportResult<()> {
    let expected = photocraft_compose::flatten(&host.active().ok_or("host document absent")?.doc).px;
    for s in clients {
        if photocraft_compose::flatten(&s.active().ok_or("client document absent")?.doc).px != expected {
            return Err("overlap raster divergence".into());
        }
    }
    Ok(())
}
fn output(args: &[String], index: usize, fallback: &str) -> TransportResult<PathBuf> {
    let path = args.get(index).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(fallback));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
async fn broadcast(host: &mut Session, links: &mut [Link], clients: &mut [Session], metrics: &mut Metrics) -> TransportResult<()> {
    metrics.canonical_high = metrics.canonical_high.max(host.collaboration.canonical_outbox.len());
    for e in std::mem::take(&mut host.collaboration.canonical_outbox) {
        for (l, s) in links.iter_mut().zip(clients.iter_mut()) {
            metrics.bytes += deliver_host(l, s, &e).await?;
        }
        metrics.received(&e);
    }
    Ok(())
}
pub(super) async fn standalone(args: &[String]) -> TransportResult<()> {
    let out = output(args, 1, "plan/evidence/collaboration-overlap/standalone")?;
    let rounds = number(args, "--rounds", ROUNDS, 1024)?;
    let width = number(args, "--width", 512, 8192)? as u32;
    let height = number(args, "--height", 320, 8192)? as u32;
    if u64::from(width) * u64::from(height) > 24_000_000 {
        return Err("stress canvas exceeds 24 MP".into());
    }
    let pace = number(args, "--step-ms", 1, 5000)? as u64;
    let start = Instant::now();
    let mut host = Session::new();
    host.execute("file.new", serde_json::json!({"width":width,"height":height,"fill":"transparent"}))?;
    let base = (*host.active().ok_or("host missing")?.doc).clone();
    let layer = host.active().and_then(|s| s.active_layer).ok_or("layer missing")?;
    host.collaboration.document_id = Some(base.id);
    host.collaboration.room = Some(RoomState::new("OVERLAP".into(), "stress-0".into(), "stress-0".into()));
    let mut links = Vec::new();
    let mut clients = Vec::new();
    for artist in 1..4 {
        let mut s = Session::new();
        s.add_document(base.clone(), None);
        s.collaboration.document_id = Some(base.id);
        s.collaboration.room = Some(RoomState::new("OVERLAP".into(), "stress-0".into(), format!("stress-{artist}")));
        clients.push(s);
        let (h, hi) = RtcPeer::new(vec![]).await?;
        let (c, ci) = RtcPeer::new(vec![]).await?;
        let answer = h.answer(&c.offer().await?).await?;
        c.accept_answer(&answer).await?;
        links.push(Link { host: h, client: c, host_inbox: hi, client_inbox: ci });
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut ready = true;
            for l in &links {
                ready &= l.client.is_open().await && l.host.is_open().await;
            }
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    let setup = start.elapsed();
    let drawing = Instant::now();
    let mut metrics = Metrics::default();
    let mut nonce = [0u64; 4];
    let mut duplicate_ignored = 0;
    for round in 0..rounds {
        let samples: Vec<_> = (0..4).map(|a| points(a, round, width, height)).collect();
        for stage in 0..CHUNKS + 2 {
            // All artists enqueue before host drains: true bursts and deterministic arrival jitter.
            for artist in 0..4 {
                let n = nonce.get_mut(artist).ok_or("artist missing")?;
                *n += 1;
                let m = ClientMessage {
                    version: PROTOCOL_VERSION,
                    peer: format!("stress-{artist}"),
                    nonce: *n,
                    operation: operation(artist, round, stage, layer, samples.get(artist).ok_or("samples missing")?, &format!("stress-{artist}"))?,
                };
                metrics.submitted(&m);
                if artist == 0 {
                    host.collaboration_accept("stress-0", m)?;
                } else {
                    for p in wire::encode_reliable(*n, &wire::Wire::Client(m))? {
                        metrics.bytes += p.len();
                        links.get(artist - 1).ok_or("link missing")?.client.send(ChannelKind::Edits, &p).await?;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(pace + ((stage * 7 + round * 3) % 13) as u64)).await;
            for link in &links {
                metrics.rtc_buffer_high = metrics.rtc_buffer_high.max(link.client.buffered_bytes(ChannelKind::Edits).await);
                metrics.host_inbox_high = metrics.host_inbox_high.max(link.host_inbox.len());
            }
            for (i, l) in links.iter_mut().enumerate() {
                let wire::Wire::Client(m) = receive_wire(&mut l.host_inbox).await? else {
                    return Err("nonclient edit".into());
                };
                let duplicate = m.clone();
                host.collaboration_accept(&format!("stress-{}", i + 1), m)?;
                // Probe replay at the actual RTC host boundary; never emit a second canonical edit.
                if stage == 3 {
                    for packet in wire::encode_reliable(duplicate.nonce, &wire::Wire::Client(duplicate))? {
                        metrics.bytes += packet.len();
                        l.client.send(ChannelKind::Edits, &packet).await?;
                    }
                    let wire::Wire::Client(replayed) = receive_wire(&mut l.host_inbox).await? else {
                        return Err("nonclient replay".into());
                    };
                    let revision = host.collaboration.applied_revision;
                    let queued = host.collaboration.canonical_outbox.len();
                    host.collaboration_accept(&format!("stress-{}", i + 1), replayed)?;
                    if host.collaboration.applied_revision == revision && host.collaboration.canonical_outbox.len() == queued {
                        duplicate_ignored += 1;
                    } else {
                        return Err("replayed operation emitted a second edit".into());
                    }
                }
            }
            if round > 0 && stage == CHUNKS / 2 {
                nonce[0] += 1;
                let m = ClientMessage { version: PROTOCOL_VERSION, peer: "stress-0".into(), nonce: nonce[0], operation: Operation::Undo };
                metrics.submitted(&m);
                host.collaboration_accept("stress-0", m)?;
                if args.iter().any(|a| a == "--redo") {
                    nonce[0] += 1;
                    let m = ClientMessage { version: PROTOCOL_VERSION, peer: "stress-0".into(), nonce: nonce[0], operation: Operation::Redo };
                    metrics.submitted(&m);
                    host.collaboration_accept("stress-0", m)?;
                }
            }
            if stage % 4 == 3 || stage == CHUNKS + 1 {
                broadcast(&mut host, &mut links, &mut clients, &mut metrics).await?;
            }
            if stage % 8 == 7 && !args.iter().any(|a| a == "--no-frames") {
                render(&host, &out.join(format!("round-{round}-stage-{stage:02}.png")))?;
            }
        }
        equal(&host, &clients)?;
    }
    equal(&host, &clients)?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    for link in &mut links {
        if link.host_inbox.try_recv().is_ok() || link.client_inbox.try_recv().is_ok() {
            return Err("unexpected edit after quiet settle: possible echo loop".into());
        }
    }
    render(&host, &out.join("host-final.png"))?;
    render(clients.first().ok_or("client missing")?, &out.join("client-final.png"))?;
    if !metrics.pending.is_empty() || metrics.revisions.len() != metrics.submitted || metrics.duplicates != 0 || duplicate_ignored != rounds * 3 {
        return Err("missing acknowledgement, duplicate canonical edit, or applied replay".into());
    }
    let report = serde_json::json!({"artists":4,"actualWebrtcLinks":3,"sameLayer":true,"wholeCanvas":true,"rounds":rounds,"canvas":[width,height],"chunksPerStroke":CHUNKS,"canonicalBurstStages":4,"pressurePoints":4*rounds*SAMPLES,"setupMs":setup.as_secs_f64()*1000.0,"drawingSeconds":drawing.elapsed().as_secs_f64(),"allCanvasesEqual":true,"inflightUndo":rounds.saturating_sub(1),"inflightRedo":if args.iter().any(|a|a=="--redo") {rounds.saturating_sub(1)} else {0},"pacingMs":pace,"replayedClientEditsIgnored":duplicate_ignored,"quietUnexpectedPackets":0,"metrics":metrics.report(drawing.elapsed()),"note":"latency includes configured 0-12ms arrival jitter and native engine apply/fanout; one canonical acknowledgement per client operation is expected, not a feedback loop"});
    std::fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{report}");
    for l in links {
        l.client.close().await?;
        l.host.close().await?;
    }
    Ok(())
}
pub(super) async fn native(args: &[String]) -> TransportResult<()> {
    use photocraft_collab::transport::{Request, Worker};
    let server = args.get(1).ok_or("--overlap-join URL CODE [password-or-dash] [output]")?.clone();
    let code = args.get(2).ok_or("room code missing")?.clone();
    let password = args.get(3).filter(|v| v.as_str() != "-").cloned();
    let out = output(args, 4, "plan/evidence/collaboration-overlap/native")?;
    let rounds = number(args, "--rounds", ROUNDS, 1024)?;
    let started = Instant::now();
    let mut bots = Vec::new();
    for _ in 0..3 {
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
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        for b in &mut bots {
            b.receive()?;
        }
        if bots.iter().all(|b| b.ready) {
            break;
        }
        if Instant::now() > until {
            return Err("native overlap bootstrap timeout".into());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let setup = started.elapsed();
    let drawing = Instant::now();
    let step = std::env::var("PHOTOCRAFT_OVERLAP_STEP_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(30).clamp(1, 2000);
    let initial = bots.first().ok_or("bots missing")?.session.collaboration.applied_revision;
    let mut received = 0;
    let mut sent = 0;
    let mut stage_ms = Vec::new();
    let mut frames = 0;
    for round in 0..rounds {
        for stage in 0..CHUNKS + 2 {
            let at = Instant::now();
            for (artist, b) in bots.iter_mut().enumerate() {
                let d = b.session.active().ok_or("native document missing")?;
                let peer = b.admission.as_ref().ok_or("admission missing")?.peer.clone();
                let layer = d.active_layer.ok_or("native raster target missing")?;
                let samples = points(artist + 1, round, d.doc.size.width, d.doc.size.height);
                b.send(operation(artist + 1, round, stage, layer, &samples, &peer)?)?;
                sent += 1;
                if round > 0 && stage == CHUNKS / 2 && artist == 0 {
                    b.send(Operation::Undo)?;
                    sent += 1;
                }
                let p = samples.get(stage.saturating_sub(1).min(CHUNKS - 1) * SAMPLES / CHUNKS).ok_or("cursor absent")?;
                b.worker.requests.try_send(Request::Send {
                    peer: None,
                    channel: ChannelKind::Presence,
                    payload: wire::encode_unreliable(&wire::Wire::Presence {
                        peer,
                        presence: Presence {
                            profile: Profile { name: format!("Overlap {}", artist + 1), color: settings(artist + 1, round).color, ..Default::default() },
                            position: Some([p.x, p.y]),
                            layer: Some(layer),
                            tool: if settings(artist + 1, round).erase { "eraser" } else { "brush" }.into(),
                            speaking: false,
                        },
                    })?,
                })?;
            }
            let until = Instant::now() + Duration::from_millis(step + ((stage * 7) % 13) as u64);
            while Instant::now() < until {
                for b in &mut bots {
                    received += b.receive()?;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            stage_ms.push(at.elapsed().as_secs_f64() * 1000.0);
            if stage % 8 == 0 {
                render(&bots.first().ok_or("bots missing")?.session, &out.join(format!("native-frame-{frames:03}.png")))?;
                frames += 1;
            }
        }
    }
    let target = initial + sent;
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        for b in &mut bots {
            received += b.receive()?;
        }
        if bots.iter().all(|b| b.session.collaboration.applied_revision >= target) {
            break;
        }
        if Instant::now() > until {
            return Err("native overlap convergence timeout".into());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    for _ in 0..20 {
        for b in &mut bots {
            received += b.receive()?;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let first = bots.first().ok_or("bots missing")?;
    let expected = photocraft_compose::flatten(&first.session.active().ok_or("doc missing")?.doc).px;
    for b in &bots {
        if photocraft_compose::flatten(&b.session.active().ok_or("doc missing")?.doc).px != expected {
            return Err("native overlap divergence".into());
        }
    }
    render(&first.session, &out.join("native-final.png"))?;
    stage_ms.sort_by(f64::total_cmp);
    let report = serde_json::json!({"realBotArtists":3,"nativeHostFourthArtistDrivenSeparately":true,"setupMs":setup.as_secs_f64()*1000.0,"submitted":sent,"canonicalDeliveries":received,"finalRevision":first.session.collaboration.applied_revision,"allThreeBotCanvasesEqual":true,"drawingSeconds":drawing.elapsed().as_secs_f64(),"stageWallMs":{"p50":stage_ms.get(stage_ms.len()/2),"p95":stage_ms.get(stage_ms.len()*95/100),"p99":stage_ms.get(stage_ms.len()*99/100)},"note":"stage wall times include configured pacing; native host canvas equality must be checked separately by capture/control"});
    std::fs::write(out.join("native-report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{report}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    for b in bots {
        let _ = b.worker.requests.try_send(Request::Close);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostile_stress_options_are_bounded() {
        assert!(number(&["--rounds".into(), "0".into()], "--rounds", 4, 1024).is_err());
        assert!(number(&["--rounds".into(), "999999999999999999999".into()], "--rounds", 4, 1024).is_err());
        assert!(number(&["--rounds".into()], "--rounds", 4, 1024).is_err());
    }
    #[test]
    fn artists_cover_entire_canvas_with_reproducible_pressure() {
        for artist in 0..4 {
            let p = points(artist, 0, 512, 320);
            let min_x = p.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
            let max_x = p.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
            let min_y = p.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
            let max_y = p.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
            assert!(max_x - min_x > 512.0 * 0.9 && max_y - min_y > 320.0 * 0.9);
            assert!(p.iter().all(|p| p.x.is_finite() && p.y.is_finite() && p.pressure >= 0.15 && p.pressure <= 1.0));
            assert_eq!(settings(artist, 0), settings(artist, 0));
        }
    }
}
