//! Bounded peer liveness and packet preflight; usable without browser bindings in regressions.
#[derive(Default)]
pub(super) struct DisconnectGrace {
    since: Option<f64>,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum HealthAction {
    Keep,
    Schedule,
    Close,
}
impl DisconnectGrace {
    pub(super) fn update(&mut self, health: &str, now: f64) -> HealthAction {
        if !now.is_finite() {
            return HealthAction::Keep;
        }
        match health {
            "failed" | "closed" => HealthAction::Close,
            "connected" | "completed" => {
                self.since = None;
                HealthAction::Keep
            }
            "disconnected" if self.since.is_none() => {
                self.since = Some(now);
                HealthAction::Schedule
            }
            "timeout" if self.since.is_some_and(|since| now - since >= 30_000.0) => HealthAction::Close,
            _ => HealthAction::Keep,
        }
    }
}
pub(super) fn packet_ready(open: bool, buffered: usize, packet_bytes: usize) -> bool {
    open && buffered.saturating_add(packet_bytes) <= 128 * 1024
}
pub(super) fn same_instance<T>(current: &std::rc::Rc<T>, expected: &std::rc::Rc<T>) -> bool {
    std::rc::Rc::ptr_eq(current, expected)
}
pub(super) fn release_credit(jobs: usize, bytes: usize, departed_jobs: usize, departed_bytes: usize) -> (usize, usize) {
    (jobs.saturating_sub(departed_jobs), bytes.saturating_sub(departed_bytes))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_events_and_decode_replies_cannot_target_replacement() {
        let first = std::rc::Rc::new(1_u64);
        let queued_event = first.clone();
        let replacement = std::rc::Rc::new(2_u64);
        assert!(same_instance(&first, &queued_event));
        assert!(!same_instance(&replacement, &queued_event));
        assert!(!same_instance(&replacement, &std::rc::Rc::new(2_u64)));
        let third = std::rc::Rc::new(3_u64);
        assert!(!same_instance(&third, &replacement));
    }
    #[test]
    fn departing_peer_releases_only_its_credits() {
        let (jobs, bytes) = release_credit(580 + 3, 1293176 + 1000, 580, 1293176);
        assert_eq!((jobs, bytes), (3, 1000));
        assert_eq!(release_credit(jobs, bytes, 0, 0), (3, 1000));
        assert_eq!(release_credit(0, 0, usize::MAX, usize::MAX), (0, 0));
    }
    #[test]
    fn transient_disconnect_keeps_reliable_lane_and_cancels_deadline() {
        let mut grace = DisconnectGrace::default();
        assert_eq!(grace.update("disconnected", 0.0), HealthAction::Schedule);
        assert_eq!(grace.update("disconnected", 10.0), HealthAction::Keep);
        assert_eq!(grace.update("timeout", 29_999.0), HealthAction::Keep);
        assert_eq!(grace.update("completed", 29_999.0), HealthAction::Keep);
        assert_eq!(grace.update("timeout", 60_000.0), HealthAction::Keep);
    }
    #[test]
    fn a_replacement_disconnect_has_its_own_deadline() {
        let mut grace = DisconnectGrace::default();
        grace.update("disconnected", 0.0);
        grace.update("connected", 20_000.0);
        assert_eq!(grace.update("disconnected", 25_000.0), HealthAction::Schedule);
        assert_eq!(grace.update("timeout", 30_000.0), HealthAction::Keep);
        assert_eq!(grace.update("timeout", 55_000.0), HealthAction::Close);
        assert_eq!(grace.update("failed", 55_001.0), HealthAction::Close);
    }
    #[test]
    fn blocked_sctp_preflight_never_needs_fragment_allocation() {
        assert!(!packet_ready(false, 0, 1024));
        assert!(!packet_ready(true, 128 * 1024, 1));
        assert!(!packet_ready(true, usize::MAX, 1));
        assert!(packet_ready(true, 128 * 1024 - 1024, 1024));
    }
}
