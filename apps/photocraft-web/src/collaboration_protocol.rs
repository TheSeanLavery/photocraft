//! Browser-independent invitation and queue policy, also checked by native unit tests.
use std::collections::HashSet;

/// Return the first unfinished message for each peer. A later transfer must never interleave
/// fragments with an earlier transfer on that peer's ordered edits channel.
pub(crate) fn first_per_peer<'a>(peers: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let mut seen = HashSet::new();
    peers.enumerate().filter_map(|(index, peer)| seen.insert(peer).then_some(index)).collect()
}

/// Activate peers only after their checkpoint has been accepted by the bounded reliable queue.
pub(crate) fn admit_bootstraps(
    pending: &mut Vec<String>,
    connected: &mut HashSet<String>,
    enqueue: impl FnOnce(Vec<String>) -> Result<(), String>,
) -> Result<(), String> {
    enqueue(pending.clone())?;
    connected.extend(pending.drain(..));
    Ok(())
}

/// A slow endpoint must not discard a relay route already gathered from another server.
pub(crate) fn candidate_kind(sdp: &str, wanted: &[&str]) -> bool {
    sdp.lines().filter(|line| line.starts_with("a=candidate:")).any(|line| {
        let mut fields = line.split_ascii_whitespace();
        while let Some(field) = fields.next() {
            if field == "typ" {
                return fields.next().is_some_and(|kind| wanted.contains(&kind));
            }
        }
        false
    })
}

/// The deployed /join/CODE landing route forwards to /app/?room=CODE. Reject ambiguous,
/// malformed or repeated codes instead of silently joining a different room.
pub(crate) fn room_from_location(path: &str, query: &str) -> Result<Option<String>, String> {
    let mut room = None;
    for pair in query.trim_start_matches('?').split('&') {
        if let Some(code) = pair.strip_prefix("room=") {
            if room.is_some() {
                return Err("An invitation may contain only one room code".into());
            }
            room = Some(photocraft_collab::invite::normalize_code(code)?);
        }
    }
    if let Some(code) = path.strip_prefix("/join/") {
        let code = photocraft_collab::invite::normalize_code(code)?;
        if room.as_ref().is_some_and(|query_code| *query_code != code) {
            return Err("The invitation contains conflicting room codes".into());
        }
        room = Some(code);
    }
    Ok(room)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_collab::wire::{self, Decoder, Wire};

    #[test]
    fn selects_fair_peer_lanes_without_interleaving_transfers() {
        assert_eq!(first_per_peer(["slow", "fast", "slow", "third", "fast"].into_iter()), vec![0, 1, 3]);
        assert_eq!(first_per_peer(["slow", "slow", "fast"].into_iter()), vec![0, 2]);
    }

    #[test]
    fn full_send_queue_keeps_guests_pending_until_checkpoint_retry_succeeds() {
        let mut pending = vec!["guest".into()];
        let mut connected = HashSet::from(["existing-peer".into()]);
        assert!(admit_bootstraps(&mut pending, &mut connected, |_| Err("full".into())).is_err());
        assert_eq!(pending, vec!["guest"]);
        assert!(!connected.contains("guest"));
        assert!(connected.contains("existing-peer"));
        admit_bootstraps(&mut pending, &mut connected, |peers| {
            assert_eq!(peers, vec!["guest"]);
            Ok(())
        })
        .unwrap();
        assert!(pending.is_empty());
        assert!(connected.contains("guest"));
    }

    #[test]
    fn partial_ice_gathering_retains_internet_routes_without_host_only_fallback() {
        assert!(candidate_kind("a=candidate:1 1 UDP 100 203.0.113.1 4000 typ relay", &["relay"]));
        assert!(candidate_kind("a=candidate:2 1 UDP 100 203.0.113.2 4001 typ srflx", &["relay", "srflx", "prflx"]));
        assert!(!candidate_kind("a=candidate:3 1 UDP 100 local.local 4002 typ host", &["relay", "srflx", "prflx"]));
        assert!(!candidate_kind("a=ice-options:trickle\na=typ relay", &["relay"]));
    }

    #[test]
    fn landing_and_browser_routes_agree_and_reject_ambiguous_invites() {
        assert_eq!(room_from_location("/app/", "?room=abc123&webgl").unwrap(), Some("ABC123".into()));
        assert_eq!(room_from_location("/join/ABC123", "").unwrap(), Some("ABC123".into()));
        assert!(room_from_location("/app/", "?room=ABC123&room=DEF456").is_err());
        assert!(room_from_location("/join/ABC123", "?room=DEF456").is_err());
        assert!(room_from_location("/app/", "?room=ABC%20123").is_err());
        assert_eq!(room_from_location("/app/", "?webgl").unwrap(), None);
    }

    #[test]
    fn native_wire_fragments_reassemble_in_browser_order_without_echo() {
        let payload = Wire::Reject("test ".repeat(9000));
        let bytes = wire::encode_message(&payload).unwrap();
        let mut decoder = Decoder::default();
        let mut completed = 0;
        for index in 0..bytes.len().div_ceil(wire::FRAGMENT_BYTES) {
            let packet = wire::fragment_message(11, &bytes, index).unwrap();
            if let Some(message) = decoder.push("authenticated-native-host", &packet).unwrap() {
                assert!(matches!(message, Wire::Reject(ref text) if text == &"test ".repeat(9000)));
                completed += 1;
            }
        }
        assert_eq!(completed, 1);
        let presence = Wire::Left("peer".into());
        let unreliable = wire::encode_unreliable(&presence).unwrap();
        assert!(matches!(wire::decode_unreliable(&unreliable).unwrap(), Wire::Left(ref id) if id == "peer"));
    }
}
