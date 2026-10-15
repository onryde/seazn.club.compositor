//! Bounded sliding window of timestamps: "N events within span". Never grows past `cap`.
use crate::time::{elapsed, Ms};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    stamps: VecDeque<Ms>,
    cap: usize,
}

impl Window {
    /// A zero cap is clamped to one, so the window always holds the latest event and stays bounded.
    pub fn new(cap: usize) -> Self {
        Self {
            stamps: VecDeque::with_capacity(cap.max(1)),
            cap: cap.max(1),
        }
    }

    /// Drops the stamps more than `span` old from the front. Stamps on a monotonic clock arrive in time
    /// order, so the stale ones are all at the front. A reordered stamp can only delay pruning behind
    /// it; `record` returns `count`, not the buffer length, so the answer never depends on the order,
    /// and the cap bounds the buffer either way.
    fn prune(&mut self, now: Ms, span: Ms) {
        while self.stamps.front().is_some_and(|&s| elapsed(now, s) > span) {
            self.stamps.pop_front();
        }
    }

    /// Records an event at `now` and returns how many events lie within `span`, this one included.
    /// At the cap the oldest stamp is dropped.
    pub fn record(&mut self, now: Ms, span: Ms) -> usize {
        self.prune(now, span);
        if self.stamps.len() == self.cap {
            self.stamps.pop_front();
        }
        self.stamps.push_back(now);
        self.count(now, span)
    }

    /// How many recorded events lie within `span` of `now` (a stamp exactly `span` old counts).
    pub fn count(&self, now: Ms, span: Ms) -> usize {
        self.stamps.iter().filter(|&&s| elapsed(now, s) <= span).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_inside_span() {
        let mut w = Window::new(16);
        w.record(0, 300_000);
        w.record(100_000, 300_000);
        assert_eq!(w.record(299_999, 300_000), 3);
        // at 300_001 the stamp at 0 is outside the 5-min span
        assert_eq!(w.count(300_001, 300_000), 2);
    }

    #[test]
    fn empty_window_counts_zero() {
        assert_eq!(Window::new(4).count(10, 10), 0);
    }

    #[test]
    fn never_exceeds_cap() {
        let mut w = Window::new(4);
        for i in 0..100 {
            w.record(i, 1_000_000);
        }
        assert_eq!(w.count(100, 1_000_000), 4, "bounded: oldest dropped beyond cap");
    }

    #[test]
    fn zero_cap_is_clamped_to_one() {
        let mut w = Window::new(0);
        assert_eq!(w.record(5, 10), 1);
    }

    #[test]
    fn stamp_exactly_span_old_still_counts() {
        let mut w = Window::new(4);
        w.record(0, 100);
        assert_eq!(w.count(100, 100), 1);
        assert_eq!(
            w.record(100, 100),
            2,
            "a stamp exactly `span` old is still inside"
        );
    }

    // A second call: the clamp only shows on the second record. Unclamped, a zero cap never equals
    // the length after the first push, so the window would grow without bound.
    #[test]
    fn zero_cap_stays_bounded_on_a_second_record() {
        let mut w = Window::new(0);
        assert_eq!(w.record(5, 10), 1);
        assert_eq!(w.record(6, 10), 1, "the clamped cap of one still holds");
        assert_eq!(w.count(6, Ms::MAX), 1, "one stamp retained, not two");
    }

    // After an interruption: once every stamp ages out, the count is 0, the next event counts as 1,
    // and the aged-out stamps are gone from the buffer, not merely skipped.
    #[test]
    fn window_drains_after_its_span_and_restarts_at_one() {
        let mut w = Window::new(4);
        w.record(0, 10);
        w.record(1, 10);
        assert_eq!(w.record(2, 10), 3);
        assert_eq!(w.count(13, 10), 0, "all three are more than 10 ms old");
        assert_eq!(w.record(13, 10), 1, "the first event after the gap counts alone");
        assert_eq!(w.count(13, Ms::MAX), 1, "the aged-out stamps were dropped");
    }

    #[test]
    fn a_second_record_at_the_same_instant_counts_twice() {
        let mut w = Window::new(4);
        assert_eq!(w.record(5, 10), 1);
        assert_eq!(
            w.record(5, 10),
            2,
            "two events in the same millisecond are two events"
        );
        assert_eq!(w.count(5, 0), 2, "age 0 is inside a zero span");
    }

    #[test]
    fn a_zero_span_counts_only_the_current_instant() {
        let mut w = Window::new(4);
        assert_eq!(w.record(5, 0), 1);
        assert_eq!(w.record(6, 0), 1, "a stamp 1 ms old is outside a zero span");
        assert_eq!(w.count(6, 0), 1);
        assert_eq!(w.count(7, 0), 0);
    }

    // Boundaries: the whole u64 range of time and span, with no overflow anywhere.
    #[test]
    fn extremes_of_time_and_span_do_not_overflow() {
        let mut w = Window::new(4);
        assert_eq!(w.record(0, Ms::MAX), 1);
        assert_eq!(
            w.record(Ms::MAX, Ms::MAX),
            2,
            "age u64::MAX is inside a u64::MAX span"
        );
        assert_eq!(
            w.count(Ms::MAX, Ms::MAX - 1),
            1,
            "one millisecond less and the oldest is out"
        );
        assert_eq!(
            Window::new(4).count(Ms::MAX, Ms::MAX),
            0,
            "empty at the top of the range"
        );
    }

    // AGENTS.md §4 "every queue has a cap and drops the oldest": at the cap, 0 goes and 10, 20 stay.
    #[test]
    fn at_the_cap_the_oldest_is_dropped_not_the_newest() {
        let mut w = Window::new(2);
        w.record(0, 100);
        w.record(10, 100);
        assert_eq!(w.record(20, 100), 2);
        assert_eq!(w.count(105, 100), 2, "10 and 20 are kept; 0 would be 105 ms old");
    }

    #[test]
    fn pruning_drops_only_the_stale_front() {
        let mut w = Window::new(4);
        w.record(0, 10);
        w.record(8, 10);
        assert_eq!(
            w.record(12, 10),
            2,
            "0 is 12 ms old and goes; 8 is 4 ms old and stays"
        );
    }

    // The time-order assumption is guarded, not commented: after a stamp that arrives out of order,
    // `record` still returns the events within `span`. At 105 with a 10 ms span, 100 and 105 are
    // inside and 50 (55 ms old) is not.
    #[test]
    fn record_counts_only_the_span_after_a_reordered_stamp() {
        let mut w = Window::new(4);
        w.record(100, 10);
        w.record(50, 10);
        assert_eq!(w.record(105, 10), 2);
        assert_eq!(w.count(105, 10), 2);
    }

    // time.rs: a reordered stamp has age 0, never a wrapped, huge age.
    #[test]
    fn a_reordered_stamp_reads_as_fresh_never_ancient() {
        let mut w = Window::new(4);
        w.record(100, 10);
        assert_eq!(
            w.count(50, 10),
            1,
            "a stamp from after `now` is age 0, inside the span"
        );
    }
}
