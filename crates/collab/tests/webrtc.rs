#![cfg(all(feature = "native", not(target_arch = "wasm32")))]
use photocraft_collab::{
    signaling::{Auth, RoomRequest, RoomService, SendSignal, Signal},
    transport::{ChannelKind, RtcPeer},
};
use std::time::Duration;
/// Real Internet acceptance. The public service supplies credentials after admission.
#[tokio::test]
#[ignore = "requires PHOTOCRAFT_PLAYTEST_URL and live Cloudflare TURN"]
async fn cloudflare_public_signaling_and_forced_relay_all_channels() {
    use photocraft_collab::transport::RTCIceServer;
    #[derive(serde::Deserialize)]
    struct Ice {
        #[serde(rename = "iceServers")]
        servers: Vec<RTCIceServer>,
    }
    let server = std::env::var("PHOTOCRAFT_PLAYTEST_URL").expect("live public server URL");
    let http = reqwest::Client::builder().user_agent("PhotoCraft-dev").timeout(Duration::from_secs(30)).build().unwrap();
    let host_admission: photocraft_collab::signaling::Admission = http
        .post(format!("{server}/create"))
        .json(&RoomRequest { code: None, password: Some("relay-acceptance".into()) })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let guest_admission: photocraft_collab::signaling::Admission = http
        .post(format!("{server}/join"))
        .json(&RoomRequest { code: Some(host_admission.code.clone()), password: Some("relay-acceptance".into()) })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut forged = Auth::from(&guest_admission);
    forged.token = "invalid".into();
    assert_eq!(http.post(format!("{server}/ice")).json(&forged).send().await.unwrap().status(), reqwest::StatusCode::UNAUTHORIZED);
    let mut peers = Vec::new();
    for admission in [&host_admission, &guest_admission] {
        let response = http.post(format!("{server}/ice")).json(&Auth::from(admission)).send().await.unwrap().error_for_status().unwrap();
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        let ice: Ice = response.json().await.unwrap();
        peers.push(RtcPeer::new_with_relay(ice.servers, true).await.unwrap());
    }
    let (guest, mut guest_inbox) = peers.pop().unwrap();
    let (host, mut host_inbox) = peers.pop().unwrap();
    let offer = guest.offer().await.unwrap();
    assert!(offer.contains("typ relay"), "TURN must allocate a relay candidate");
    assert!(!offer.contains("typ host"), "forced relay must exclude direct candidates");
    http.post(format!("{server}/signal"))
        .json(&SendSignal {
            auth: Auth::from(&guest_admission),
            signal: Signal { from: guest_admission.peer.clone(), to: host_admission.peer.clone(), sdp: offer, offer: true },
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let signals: Vec<Signal> =
        http.post(format!("{server}/poll")).json(&Auth::from(&host_admission)).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let answer = host.answer(&signals.first().unwrap().sdp).await.unwrap();
    assert!(answer.contains("typ relay"));
    http.post(format!("{server}/signal"))
        .json(&SendSignal {
            auth: Auth::from(&host_admission),
            signal: Signal { from: host_admission.peer.clone(), to: guest_admission.peer.clone(), sdp: answer, offer: false },
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let signals: Vec<Signal> =
        http.post(format!("{server}/poll")).json(&Auth::from(&guest_admission)).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    guest.accept_answer(&signals.first().unwrap().sdp).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !host.is_open().await || !guest.is_open().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for channel in [ChannelKind::Edits, ChannelKind::Presence, ChannelKind::Voice] {
        guest.send(channel, b"Cloudflare relay acceptance").await.unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(10), host_inbox.recv()).await.unwrap().unwrap();
        assert_eq!(packet.channel, channel);
        assert_eq!(packet.payload, b"Cloudflare relay acceptance");
        host.send(channel, b"accepted").await.unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(10), guest_inbox.recv()).await.unwrap().unwrap().payload, b"accepted");
    }
    host.close().await.unwrap();
    guest.close().await.unwrap();
    http.post(format!("{server}/leave")).json(&Auth::from(&host_admission)).send().await.unwrap().error_for_status().unwrap();
    println!("Public HTTPS signaling and forced Cloudflare relay passed: edits, presence, voice bidirectionally");
}
#[tokio::test]
async fn encrypted_localhost_roundtrip_all_channels() {
    let (host, mut host_inbox) = RtcPeer::new(vec![]).await.unwrap();
    let (client, mut client_inbox) = RtcPeer::new(vec![]).await.unwrap();
    let offer = client.offer().await.unwrap();
    let answer = host.answer(&offer).await.unwrap();
    client.accept_answer(&answer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !host.is_open().await || !client.is_open().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for channel in [ChannelKind::Edits, ChannelKind::Presence, ChannelKind::Voice] {
        tokio::time::sleep(Duration::from_millis(100)).await;
        client.send(channel, b"pressure=.72 tilt=15 seed=44").await.unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(3), host_inbox.recv()).await.unwrap().unwrap();
        assert_eq!(packet.channel, channel);
        assert_eq!(packet.payload, b"pressure=.72 tilt=15 seed=44");
        host.send(channel, b"accepted=1").await.unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(3), client_inbox.recv()).await.unwrap().unwrap();
        assert_eq!(packet.payload, b"accepted=1");
    }
    assert!(client.send(ChannelKind::Edits, &vec![0; 50000]).await.is_err());
    host.close().await.unwrap();
    client.close().await.unwrap();
}
#[tokio::test]
async fn room_password_host_routing_and_admission_bounds() {
    let service = RoomService::default();
    let host = service.create(RoomRequest { code: None, password: Some("secret".into()) }).await.unwrap();
    assert!(service.join(RoomRequest { code: Some(host.code.clone()), password: None }).await.is_err());
    let client = service.join(RoomRequest { code: Some(host.code.clone()), password: Some("secret".into()) }).await.unwrap();
    let other = service.join(RoomRequest { code: Some(host.code.clone()), password: Some("secret".into()) }).await.unwrap();
    assert!(
        service
            .send(SendSignal {
                auth: Auth::from(&client),
                signal: Signal { from: client.peer.clone(), to: other.peer.clone(), sdp: "sdp".into(), offer: true }
            })
            .await
            .is_err()
    );
    service
        .send(SendSignal { auth: Auth::from(&client), signal: Signal { from: client.peer.clone(), to: host.peer.clone(), sdp: "sdp".into(), offer: true } })
        .await
        .unwrap();
    assert_eq!(service.poll(Auth::from(&host)).await.unwrap().len(), 1);
    let mut forged = Auth::from(&host);
    forged.token = "invalid".into();
    assert!(service.poll(forged).await.is_err());
    for _ in 3..64 {
        service.join(RoomRequest { code: Some(host.code.clone()), password: Some("secret".into()) }).await.unwrap();
    }
    assert!(service.join(RoomRequest { code: Some(host.code.clone()), password: Some("secret".into()) }).await.is_err());
    assert!(service.kick(photocraft_collab::signaling::Kick { auth: Auth::from(&client), peer: other.peer.clone() }).await.is_err());
    service.kick(photocraft_collab::signaling::Kick { auth: Auth::from(&host), peer: other.peer.clone() }).await.unwrap();
    assert!(service.poll(Auth::from(&other)).await.is_err());
    service.join(RoomRequest { code: Some(host.code.clone()), password: Some("secret".into()) }).await.unwrap();
    service.leave(Auth::from(&host)).await.unwrap();
    assert!(service.poll(Auth::from(&client)).await.is_err());
}

#[test]
fn room_workers_negotiate_over_http_then_send_over_webrtc() {
    use photocraft_collab::transport::{Event, Request, Worker};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let task = runtime.spawn(async move {
        axum::serve(listener, photocraft_collab::signaling::router()).await.unwrap();
    });
    let host = Worker::start().unwrap();
    let client = Worker::start().unwrap();
    host.requests.try_send(Request::Create { server: server.clone(), password: Some("test".into()), ice_servers: vec![] }).unwrap();
    let admission = match host.events.recv_timeout(Duration::from_secs(10)).unwrap() {
        Event::Joined { admission } => admission,
        event => panic!("unexpected {event:?}"),
    };
    client.requests.try_send(Request::Join { server, code: admission.code, password: Some("test".into()), ice_servers: vec![] }).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let event = client.events.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).unwrap();
        match event {
            Event::PeerConnected { .. } => break,
            Event::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    client.requests.try_send(Request::Send { peer: None, channel: ChannelKind::Edits, payload: b"stroke begin".to_vec() }).unwrap();
    loop {
        let event = host.events.recv_timeout(Duration::from_secs(5)).unwrap();
        match event {
            Event::Packet { payload, .. } => {
                assert_eq!(payload, b"stroke begin");
                break;
            }
            Event::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    client.requests.try_send(Request::Close).unwrap();
    host.requests.try_send(Request::Close).unwrap();
    task.abort();
}

/// Explicit scale gate; creates 32 real DTLS/SCTP clients, not simulated sockets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "32-peer network scale test"]
async fn thirty_two_real_clients_stream_pressure_chunks() {
    let started = std::time::Instant::now();
    let mut links = Vec::new();
    for _ in 0..32 {
        let (host, inbox) = RtcPeer::new(vec![]).await.unwrap();
        let (client, _) = RtcPeer::new(vec![]).await.unwrap();
        let offer = client.offer().await.unwrap();
        let answer = host.answer(&offer).await.unwrap();
        client.accept_answer(&answer).await.unwrap();
        links.push((host, client, inbox));
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut open = true;
            for (host, client, _) in &links {
                open &= host.is_open().await && client.is_open().await;
            }
            if open {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let mut bytes = 0usize;
    for chunk in 0..20 {
        for (_, client, _) in &links {
            let payload = format!("stroke:42 chunk:{chunk} pressure:0.72 tilt:15 seed:44");
            bytes += payload.len();
            client.send(ChannelKind::Edits, payload.as_bytes()).await.unwrap();
        }
    }
    for (_, _, inbox) in &mut links {
        for chunk in 0..20 {
            let packet = tokio::time::timeout(Duration::from_secs(10), inbox.recv()).await.unwrap().unwrap();
            assert!(String::from_utf8(packet.payload).unwrap().contains(&format!("chunk:{chunk} ")));
        }
    }
    for (host, client, _) in &links {
        host.close().await.unwrap();
        client.close().await.unwrap();
    }
    println!("32 real clients: 640 ordered pressure chunks, {bytes} payload bytes, elapsed {:?}", started.elapsed());
}

#[test]
#[ignore = "32-worker real room stress"]
fn thirty_two_workers_join_one_room_and_stream_without_signaling_stalls() {
    use photocraft_collab::transport::{Event, Request, Worker};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let task = runtime.spawn(async move {
        axum::serve(listener, photocraft_collab::signaling::router()).await.unwrap();
    });
    let host = Worker::start().unwrap();
    host.requests.try_send(Request::Create { server: server.clone(), password: None, ice_servers: vec![] }).unwrap();
    let admission = match host.events.recv_timeout(Duration::from_secs(10)).unwrap() {
        Event::Joined { admission } => admission,
        event => panic!("unexpected {event:?}"),
    };
    let started = std::time::Instant::now();
    let mut clients = Vec::new();
    for _ in 0..31 {
        let client = Worker::start().unwrap();
        client.requests.try_send(Request::Join { server: server.clone(), code: admission.code.clone(), password: None, ice_servers: vec![] }).unwrap();
        clients.push(client);
    }
    for client in &clients {
        loop {
            match client.events.recv_timeout(Duration::from_secs(30)).unwrap() {
                Event::PeerConnected { .. } => break,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
    }
    for (i, client) in clients.iter().enumerate() {
        client.requests.try_send(Request::Send { peer: None, channel: ChannelKind::Edits, payload: format!("pressure stream {i}").into_bytes() }).unwrap();
    }
    let mut packets = 0;
    while packets < 31 {
        match host.events.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Packet { .. } => packets += 1,
            Event::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    // A stalled signaling endpoint must not hold packets between existing peers.
    task.abort();
    std::thread::sleep(Duration::from_millis(300));
    clients[0].requests.try_send(Request::Send { peer: None, channel: ChannelKind::Edits, payload: b"still live".to_vec() }).unwrap();
    let packet_at = std::time::Instant::now();
    loop {
        match host.events.recv_timeout(Duration::from_secs(2)).unwrap() {
            Event::Packet { payload, .. } => {
                assert_eq!(payload, b"still live");
                break;
            }
            Event::Error(_) => {}
            _ => {}
        }
    }
    assert!(packet_at.elapsed() < Duration::from_millis(500));
    println!(
        "32-worker room: all31 clients connected and messages received in {:?}; signaling outage packet latency {:?}",
        started.elapsed(),
        packet_at.elapsed()
    );
    for client in clients {
        let _ = client.requests.try_send(Request::Close);
    }
    let _ = host.requests.try_send(Request::Close);
}
