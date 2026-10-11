//! Browser-independent invitation and queue policy, also checked by native unit tests.
use std::collections::HashSet;

/// A single deadline covers receiving, encoding and both transport flush opportunities.
/// Quotas are burst guards, not the old eight-packet throughput bottleneck.
pub(crate) struct FrameBudget {
    started: f64,
    pub(crate) busy: bool,
    events: usize,
    operations: usize,
    packets: usize,
}
#[derive(Default)]
pub(crate) struct InputPriority {
    last_active: Option<f64>,
}
impl InputPriority {
    pub(crate) fn busy(&mut self, now: f64, pointer_down: bool) -> bool {
        if pointer_down {
            self.last_active = Some(now);
        }
        pointer_down || self.last_active.is_some_and(|last| now - last < 250.0)
    }
}
impl FrameBudget {
    pub(crate) fn new(started: f64, busy: bool) -> Self {
        Self { started, busy, events: 0, operations: 0, packets: 0 }
    }
    pub(crate) fn available(&self, now: f64) -> bool {
        now - self.started < if self.busy { 1.0 } else { 2.0 }
    }
    pub(crate) fn event(&mut self, now: f64) -> bool {
        if self.events >= 64 || (self.events > 0 && now - self.started >= if self.busy { 0.5 } else { 1.0 }) {
            return false;
        }
        self.events += 1;
        true
    }
    pub(crate) fn operation(&mut self, now: f64) -> bool {
        if self.operations >= 32 || (self.operations > 0 && now - self.started >= if self.busy { 0.75 } else { 1.5 }) {
            return false;
        }
        self.operations += 1;
        true
    }
    pub(crate) fn packet(&mut self, now: f64) -> bool {
        if self.packets >= 64 || (self.packets > 0 && !self.available(now)) {
            return false;
        }
        self.packets += 1;
        true
    }
    pub(crate) fn snapshot(&self, now: f64) -> bool {
        !self.busy && self.available(now)
    }
}
/// Choose only lane fronts, round-robin across at most 64 peers; never scan queued jobs.
pub(crate) fn next_peer<'a>(lanes: impl Iterator<Item = (&'a str, bool)>, last: Option<&str>, allow_bulk: bool, prefer_bulk: bool) -> Option<String> {
    let lanes: Vec<_> = lanes.filter(|(_, bulk)| allow_bulk || !bulk).collect();
    let priority_bulk = prefer_bulk && lanes.iter().any(|(_, bulk)| *bulk);
    let priority = lanes.iter().any(|(_, bulk)| *bulk == priority_bulk);
    lanes
        .iter()
        .filter(|(_, bulk)| !priority || *bulk == priority_bulk)
        .find(|(peer, _)| last.is_none_or(|last| *peer > last))
        .or_else(|| lanes.iter().find(|(_, bulk)| !priority || *bulk == priority_bulk))
        .map(|(peer, _)| (*peer).to_owned())
}
/// Byte and job capacity checks happen before any fanout lane is modified.
pub(crate) fn fits_queue(jobs: usize, bytes: usize, peers: usize, payload: usize, max_bytes: usize) -> bool {
    jobs.saturating_add(peers) <= 4096 && bytes.saturating_add(payload.saturating_mul(peers)) <= max_bytes
}
/// A checkpoint can activate guests only at its exact current canonical revision.
pub(crate) fn snapshot_current(cached: u64, current: u64) -> bool {
    cached == current
}

/// A posted task continues only after real progress. Full SCTP channels await bufferedamountlow,
/// and idle pages never create an unbounded MessageChannel self-post loop.
pub(crate) fn continue_background(hidden: bool, queued: bool, progressed: bool, healthy: bool) -> bool {
    hidden && queued && progressed && healthy
}

/// Bounded optional diagnostic samples; no stroke data or identity is retained.
#[derive(Default)]
pub(crate) struct Samples {
    values: std::collections::VecDeque<f64>,
}
impl Samples {
    pub(crate) fn observe(&mut self, value: f64) {
        if value.is_finite() && value >= 0.0 {
            self.values.push_back(value);
            if self.values.len() > 240 {
                self.values.pop_front();
            }
        }
    }
    pub(crate) fn summary(&self) -> serde_json::Value {
        let mut sorted: Vec<_> = self.values.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let percentile = |percent: usize| {
            let index = sorted.len().saturating_sub(1).saturating_mul(percent) / 100;
            sorted.get(index).copied().unwrap_or(0.0)
        };
        serde_json::json!({"samples":sorted.len(),"p50":percentile(50),"p95":percentile(95),"max":sorted.last().copied().unwrap_or(0.0)})
    }
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

/// Keep the shareable page address aligned with the admitted room without reloading the editor.
pub(crate) fn room_page_address(path: &str, query: &str, code: Option<&str>) -> Result<String, String> {
    let mut parameters: Vec<String> =
        query.trim_start_matches('?').split('&').filter(|p| !p.is_empty() && !p.starts_with("room=")).map(str::to_owned).collect();
    if let Some(code) = code {
        parameters.push(format!("room={}", photocraft_collab::invite::normalize_code(code)?));
    }
    let path = if path.starts_with("/join/") { "/app/" } else { path };
    Ok(if parameters.is_empty() { path.to_owned() } else { format!("{path}?{}", parameters.join("&")) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_collab::wire::{self, Decoder, Wire};

    #[test]
    fn shared_page_tracks_new_room_and_clears_old_invites_on_leave() {
        let address = room_page_address("/app/", "?cpu&room=OLDROOM&room=STALE1", Some("newroom")).unwrap();
        let (path, query) = address.split_once('?').unwrap();
        assert_eq!(room_from_location(path, query).unwrap(), Some("NEWROOM".into()));
        assert!(query.starts_with("cpu&"));
        assert_eq!(room_page_address(path, query, None).unwrap(), "/app/?cpu");
        assert_eq!(room_page_address("/join/OLDROOM", "", Some("NEWROOM")).unwrap(), "/app/?room=NEWROOM");
        assert!(room_page_address("/app/", "", Some("bad/code")).is_err());
    }

    #[test]
    fn shared_budget_bounds_two_flushes_and_prioritizes_drawing() {
        let mut priority = InputPriority::default();
        assert!(priority.busy(0.0, true));
        assert!(priority.busy(249.0, false));
        assert!(!priority.busy(250.0, false));
        let mut budget = FrameBudget::new(0.0, true);
        assert!(!budget.snapshot(0.1));
        for _ in 0..32 {
            assert!(budget.packet(0.2));
        }
        for _ in 0..32 {
            assert!(budget.packet(0.3));
        }
        assert!(!budget.packet(0.4));
        assert!(!budget.packet(1.0));
        let mut budget = FrameBudget::new(0.0, false);
        assert!(budget.event(2.1));
        assert!(!budget.event(2.1));
        assert!(budget.operation(2.1));
        assert!(!budget.operation(2.1));
        assert!(!budget.snapshot(2.1));
    }
    #[test]
    fn round_robin_uses_lane_fronts_and_live_edits_precede_bulk() {
        let lanes = [("a", false), ("b", false), ("new", true)];
        assert_eq!(next_peer(lanes.into_iter(), None, true, false), Some("a".into()));
        assert_eq!(next_peer(lanes.into_iter(), Some("a"), true, false), Some("b".into()));
        assert_eq!(next_peer(lanes.into_iter(), Some("b"), true, false), Some("a".into()));
        assert_eq!(next_peer([("new", true)].into_iter(), None, false, false), None);
        assert_eq!(next_peer([("new", true)].into_iter(), None, true, false), Some("new".into()));
        let mut lane = std::collections::VecDeque::from(["begin", "chunk", "end"]);
        assert_eq!(lane.front(), Some(&"begin"));
        lane.pop_front();
        assert_eq!(lane.front(), Some(&"chunk"));
    }
    #[test]
    fn statistics_window_is_bounded_and_invalid_samples_are_rejected() {
        let mut samples = Samples::default();
        for value in 0..1000 {
            samples.observe(f64::from(value));
        }
        samples.observe(f64::NAN);
        samples.observe(-1.0);
        samples.observe(f64::INFINITY);
        let summary = samples.summary();
        assert_eq!(summary["samples"], 240);
        assert_eq!(summary["max"], 999.0);
        assert!(summary["p50"].as_f64().unwrap() > 800.0);
        assert_eq!(next_peer([("live", false), ("new", true)].into_iter(), None, true, true), Some("new".into()));
    }

    #[test]
    fn congested_peer_does_not_reorder_or_block_another_peers_wire_transfers() {
        use std::cell::Cell;
        use std::collections::{BTreeMap, VecDeque};
        let mut lanes = BTreeMap::from([("slow", VecDeque::from([1u64, 2, 3])), ("ready", (10u64..4106).collect::<VecDeque<_>>())]);
        let mut visited = 0;
        let mut received = Vec::new();
        let mut last = None;
        for _ in 0..64 {
            let inspected = Cell::new(0);
            let next = next_peer(
                lanes.iter().filter(|(peer, _)| **peer != "slow").filter_map(|(peer, lane)| {
                    inspected.set(inspected.get() + 1);
                    lane.front().map(|_| (*peer, false))
                }),
                last.as_deref(),
                true,
                false,
            )
            .unwrap();
            visited += inspected.get();
            received.push(lanes.get_mut(next.as_str()).unwrap().pop_front().unwrap());
            last = Some(next);
        }
        assert_eq!(received, (10u64..74).collect::<Vec<_>>());
        assert_eq!(lanes["slow"], VecDeque::from([1, 2, 3]));
        assert_eq!(visited, 64, "only lane fronts, independent of 4096-job backlog");
        let mut stalled = FrameBudget::new(0.0, true);
        assert!(stalled.packet(100.0), "one reliable progress step even after a costly edit");
        assert!(!stalled.packet(100.0));
    }

    #[test]
    fn hidden_host_tasks_continue_without_a_frame_but_stop_on_backpressure_or_idle() {
        assert!(continue_background(true, true, true, true));
        assert!(!continue_background(false, true, true, true));
        assert!(!continue_background(true, false, true, true));
        assert!(!continue_background(true, true, false, true));
        assert!(!continue_background(true, true, true, false));
        let mut remaining: usize = 1024;
        let mut tasks = 0;
        while continue_background(true, remaining > 0, true, true) {
            remaining = remaining.saturating_sub(64);
            tasks += 1;
        }
        assert_eq!(tasks, 16, "background work is independent of one-Hz paint callbacks");
    }

    #[test]
    fn fanout_capacity_and_stale_snapshot_never_partially_admit() {
        assert!(fits_queue(4094, 10, 2, 5, 20));
        assert!(!fits_queue(4095, 10, 2, 5, 20));
        assert!(!fits_queue(0, 10, 2, 6, 20));
        assert!(!fits_queue(usize::MAX, 0, 2, 1, 20));
        assert!(snapshot_current(7, 7));
        assert!(!snapshot_current(7, 8));
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
