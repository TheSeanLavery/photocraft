#![cfg(all(feature = "native", not(target_arch = "wasm32")))]
//! A paused application reader resumes reliable packets in order without disconnecting.
//! Both the old and current transports pass: this does not measure time above the SCTP threshold.
use photocraft_collab::transport::{ChannelKind, RtcPeer};
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reliable_slow_reader_recovers_without_losing_packets() {
    let (sender, _) = RtcPeer::new_with_relay(Vec::new(), false).await.unwrap();
    let (reader, mut inbox) = RtcPeer::new_with_relay(Vec::new(), false).await.unwrap();
    let answer = reader.answer(&sender.offer().await.unwrap()).await.unwrap();
    sender.accept_answer(&answer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sender.is_open().await || !reader.is_open().await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let sender = Arc::new(sender);
    let writing = sender.clone();
    let producer = tokio::spawn(async move {
        for sequence in 0u32..1200 {
            let mut packet = vec![7; 16 * 1024];
            packet.get_mut(..4).unwrap().copy_from_slice(&sequence.to_le_bytes());
            writing.send(ChannelKind::Edits, &packet).await?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    // Pause consumption for six seconds. This exercises queue recovery, not the duration
    // for which the sender remains above its SCTP buffered-amount threshold.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(!producer.is_finished(), "a blocked reliable writer must remain pending, not disconnect");
    tokio::time::timeout(Duration::from_secs(30), async {
        for sequence in 0u32..1200 {
            let packet = inbox.recv().await.unwrap();
            assert_eq!(packet.channel, ChannelKind::Edits);
            assert_eq!(packet.payload.get(..4), Some(sequence.to_le_bytes().as_slice()));
        }
        producer.await.unwrap().unwrap();
    })
    .await
    .unwrap();
    assert!(sender.is_open().await && reader.is_open().await);
    sender.close().await.unwrap();
    reader.close().await.unwrap();
}
