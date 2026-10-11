//! Pure bounds and generation policy shared by the Window/worker bridge and native tests.
pub const LIMIT: usize = 64 * 1024 * 1024;
// A single full-protocol checkpoint can use the empty lane, plus bounded clone metadata.
pub const MAX_SINGLE: usize = 512 * 1024 * 1024 + LIMIT;
pub fn accepts(jobs: usize, bytes: usize, charge: usize) -> bool {
    jobs < 256 && charge <= MAX_SINGLE && ((jobs == 0 && bytes == 0) || bytes.checked_add(charge).is_some_and(|sum| sum <= LIMIT))
}
// Used by Window scheduling; the same policy module is included in the worker binary.
#[allow(dead_code)]
pub fn snapshot_due(busy: bool, available: bool, waiting_ms: f64) -> bool {
    (!busy && available) || (waiting_ms.is_finite() && waiting_ms >= 1500.0)
}
#[allow(dead_code)]
pub fn bulk_due(busy: bool, sent_bulk: bool, age_ms: f64) -> bool {
    !busy || (!sent_bulk && age_ms.is_finite() && age_ms >= 250.0)
}
/// Queue age is not worker liveness: received replies can wait behind a costly engine edit.
/// A higher reply proves a missing FIFO result; otherwise require a full silence window.
#[allow(dead_code)]
pub fn worker_stalled(now: f64, posted: f64, received: bool, last_progress: f64, id: u64, highest_reply: u64) -> bool {
    if received {
        return false;
    }
    let age = now - posted;
    (highest_reply > id && age >= 15_000.0) || (age >= 30_000.0 && now - last_progress >= 30_000.0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn received_worker_results_wait_for_slow_engine_without_false_timeout() {
        assert!(!worker_stalled(60_000.0, 0.0, true, 0.0, 1, 1));
        assert!(!worker_stalled(30_000.0, 0.0, false, 29_900.0, 50, 49));
        assert!(!worker_stalled(14_999.0, 0.0, false, 14_900.0, 1, 2));
        assert!(worker_stalled(15_000.0, 0.0, false, 14_900.0, 1, 2));
        assert!(!worker_stalled(29_999.0, 0.0, false, 0.0, 1, 0));
        assert!(worker_stalled(30_000.0, 0.0, false, 0.0, 1, 0));
        assert!(!worker_stalled(100_000.0, 99_999.0, false, 0.0, 1, 0));
    }
    #[test]
    fn bounded_ipc_backpressure_preserves_fifo_retry_and_releases_credit() {
        let mut queue = std::collections::VecDeque::new();
        let mut bytes = 0usize;
        for id in 0..256 {
            assert!(accepts(queue.len(), bytes, 1024));
            queue.push_back((id, 1024));
            bytes += 1024;
        }
        assert!(!accepts(queue.len(), bytes, 1024));
        let (id, charge) = queue.pop_front().unwrap();
        assert_eq!(id, 0);
        bytes -= charge;
        assert!(accepts(queue.len(), bytes, 1024));
        queue.push_back((256, 1024));
        assert_eq!(queue.front().map(|(id, _)| *id), Some(1));
        assert!(!accepts(0, LIMIT, 1));
        assert!(!accepts(0, usize::MAX, 1));
        assert!(accepts(0, 0, LIMIT + 1));
        assert!(accepts(0, 0, 512 * 1024 * 1024));
        assert!(!accepts(1, 1, LIMIT + 1));
        assert!(!accepts(1, LIMIT + 1, 1));
        assert!(!accepts(0, 0, MAX_SINGLE + 1));
        assert!(accepts(0, 0, LIMIT));
    }
    #[test]
    fn sustained_drawing_cannot_starve_bootstrap_or_bulk_progress() {
        assert!(!snapshot_due(true, true, 1499.0));
        assert!(snapshot_due(true, false, 1500.0));
        assert!(!bulk_due(true, false, 249.0));
        assert!(bulk_due(true, false, 250.0));
        assert!(!bulk_due(true, true, 5000.0));
        assert!(!snapshot_due(true, false, f64::NAN));
    }
    #[test]
    fn async_bootstrap_reservation_orders_edits_arriving_during_encoding() {
        // The actual bridge queues a None slot before admitting the peer to fanout.
        let checkpoint = std::rc::Rc::new(std::cell::RefCell::new(None::<u64>));
        let mut lane = std::collections::VecDeque::from([checkpoint.clone()]);
        lane.push_back(std::rc::Rc::new(std::cell::RefCell::new(Some(43))));
        lane.push_back(std::rc::Rc::new(std::cell::RefCell::new(Some(44))));
        assert!(lane.front().unwrap().borrow().is_none());
        *checkpoint.borrow_mut() = Some(42);
        let revisions: Vec<_> = lane.into_iter().map(|slot| slot.borrow().unwrap()).collect();
        assert_eq!(revisions, [42, 43, 44]);
    }
}
