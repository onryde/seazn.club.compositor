//! Programme push link (RTMPS to CF Input B): swap the sink on an error or when the bytes it sends
//! stall for 5 s; back off 0.5 s → 2 s between failed connects; ignore every other generation.
//!
//! ```text
//! Backoff --due tick: build the next sink--> Connecting --first bytes sent--> Connected
//!    ^  ^                                       |   ^                          |   |
//!    |  +------ connect timeout, or error ------+   +-- no bytes for 5 s: -----+   |
//!    |                                                  swap at once, no wait      |
//!    +------------------------------------- error ---------------------------------+
//! ```
//!
//! - **Liveness is bytes SENT, never bytes acked.** F-005: CF Input B sends no RTMP acknowledgements
//!   (rtmp2sink `out-bytes-acked` stayed 0 for 290 s while `out-bytes-total` grew), so an ack rule
//!   would stall every healthy push and never connect one. The adapter reports the sink's own
//!   monotonic bytes-sent total, counted from 0 for each new sink (C09 feeds `out-bytes-total`).
//! - **Progress** is a total above the highest this sink has reported. A total that repeats or goes
//!   backwards is no progress: it neither connects a sink nor keeps one alive.
//! - **Connected** is the first progress after a build. **Stalled** is no progress for
//!   `output_stall_after_ms` while Connected; the sink is then swapped at once, with no back-off.
//! - **Back-off.** An error, or a connect that times out (`output_connect_timeout_ms`; rtmp2sink's
//!   "Socket I/O timed out", F-004 rerun 1), is one more consecutive failure, and failure number f
//!   waits `delay(f - 1)`: 0.5, 1, 2, 2 s. It never gives up. A first progress resets it.
//! - **Generations.** Every build is a new generation, and an event from any other generation is
//!   ignored in both directions: a late error from a torn-down sink cannot knock down its replacement,
//!   and late bytes from it can neither connect the replacement nor keep it alive.
//! - **The 15 s budget** (`OUTPUT_GAP_BUDGET_MS`, F-004) holds from an error through one failed
//!   connect, not two: `tests/links.rs` measures every recovery path, and docs/_INDEX.md records the
//!   paths that exceed it.
use crate::backoff::delay;
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputState {
    /// The current sink is being built; `attempt` consecutive failures came before it.
    Connecting { attempt: u32, since: Ms },
    /// Bytes flow. `last_progress` is when the total last grew; `sent` is the highest total this sink
    /// has reported.
    Connected { last_progress: Ms, sent: u64 },
    /// `attempt` consecutive failures so far; the next sink is built at the first tick at or after
    /// `until`.
    Backoff { attempt: u32, until: Ms },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLink {
    state: OutputState,
    generation: u64,
}

impl Default for OutputLink {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputLink {
    /// `Backoff { attempt: 0, until: 0 }`: the first tick builds sink generation 1.
    pub fn new() -> Self {
        Self {
            state: OutputState::Backoff { attempt: 0, until: 0 },
            generation: 0,
        }
    }

    pub fn state(&self) -> OutputState {
        self.state
    }

    /// The number of sink builds so far, and the generation an adapter event must carry to count.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.state, OutputState::Connected { .. })
    }

    /// The current sink's bytes-sent total, counted from 0 when the sink was built (C09: rtmp2sink
    /// `out-bytes-total`). Only a total above the highest one reported counts as progress.
    pub fn on_progress(&mut self, now: Ms, gen: u64, total_bytes: u64) {
        if gen != self.generation {
            return;
        }
        let sent = match self.state {
            OutputState::Connecting { .. } => 0, // a new sink counts from 0
            OutputState::Connected { sent, .. } => sent,
            // The sink failed: its late bytes change nothing. The next build replaces it.
            OutputState::Backoff { .. } => return,
        };
        if total_bytes > sent {
            self.state = OutputState::Connected {
                last_progress: now,
                sent: total_bytes,
            };
        }
    }

    /// The current sink failed: a bus error, including a connect that timed out in rtmp2sink.
    pub fn on_error(&mut self, now: Ms, gen: u64, t: &Thresholds) {
        if gen != self.generation {
            return;
        }
        let failed_before = match self.state {
            OutputState::Connected { .. } => 0,
            OutputState::Connecting { attempt, .. } => attempt,
            // Already failed: a second error from the same sink changes nothing.
            OutputState::Backoff { .. } => return,
        };
        self.back_off(now, failed_before, t);
    }

    /// `true` ⇒ effect `SwapSink(generation())`.
    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool {
        match self.state {
            OutputState::Backoff { attempt, until } if now >= until => self.build(now, attempt),
            OutputState::Connecting { attempt, since }
                if elapsed(now, since) >= t.output_connect_timeout_ms =>
            {
                self.back_off(now, attempt, t);
                false
            }
            // Stalled: swap at once. The stall already spent 5 s of the 15 s budget.
            OutputState::Connected { last_progress, .. }
                if elapsed(now, last_progress) >= t.output_stall_after_ms =>
            {
                self.build(now, 0)
            }
            _ => false,
        }
    }

    /// The next sink generation, carrying the failures before it.
    fn build(&mut self, now: Ms, attempt: u32) -> bool {
        self.generation += 1;
        self.state = OutputState::Connecting { attempt, since: now };
        true
    }

    /// Failure number `failed_before + 1` waits `delay(failed_before)`. Both additions saturate: the
    /// clock is a u64 (time.rs), and "never gives up" has no last attempt.
    fn back_off(&mut self, now: Ms, failed_before: u32, t: &Thresholds) {
        let wait = delay(failed_before, t.output_backoff_base_ms, t.output_backoff_cap_ms);
        self.state = OutputState::Backoff {
            attempt: failed_before.saturating_add(1),
            until: now.saturating_add(wait),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Thresholds {
        Thresholds::default()
    }

    /// Build, then the first bytes: a link Connected at generation 1, `bytes` sent at `now`.
    fn connected(now: Ms, bytes: u64) -> OutputLink {
        let mut o = OutputLink::new();
        assert!(o.on_tick(now, &t()));
        o.on_progress(now, 1, bytes);
        assert!(o.is_connected());
        o
    }

    /// Ticks forward from `now` in 10 ms steps until the next build; returns its time.
    fn next_build(o: &mut OutputLink, mut now: Ms, th: &Thresholds) -> Ms {
        while !o.on_tick(now, th) {
            now += 10;
        }
        now
    }

    /// A destination that refuses until `available_at`, then sends its first bytes `connect_ms`
    /// after each swap.
    fn reconnect_time(available_at: Ms, connect_ms: Ms) -> Ms {
        let th = t();
        let mut o = OutputLink::new();
        let mut pending: Option<(Ms, u64)> = None;
        let mut now = 0;
        while now < 120_000 {
            if o.on_tick(now, &th) {
                pending = Some((now, o.generation()));
            }
            if let Some((at, gen)) = pending {
                if now >= at + connect_ms {
                    pending = None;
                    if now >= available_at {
                        o.on_progress(now, gen, 4_096);
                        assert!(o.is_connected(), "the first bytes connect the sink");
                        return now;
                    }
                    o.on_error(now, gen, &th);
                }
            }
            now += 33;
        }
        panic!("never reconnected");
    }

    // ---- the brief's 6 (C04 Step 5), on bytes sent instead of acks (F-005) ----

    #[test]
    fn reconnects_within_5_s_of_destination_returning() {
        // PDF chaos: destination disconnects for 30 s → output back within 5 s of availability
        let back = reconnect_time(30_000, 400);
        assert!(
            back - 30_000 <= 5_000,
            "reconnected {} ms after availability",
            back - 30_000
        );
    }

    #[test]
    fn progress_stall_swaps_the_sink_within_5_s() {
        let th = t();
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        o.on_progress(100, 1, 16_000);
        let (mut now, mut total) = (100, 16_000);
        while now < 10_000 {
            total += 16_000;
            o.on_progress(now, 1, total);
            now += 33;
        } // healthy until 10 s; then the bytes stop and every poll reports the same total
        let last = now - 33;
        let mut swapped_at = None;
        while now < 20_000 {
            o.on_progress(now, 1, total);
            if o.on_tick(now, &th) {
                swapped_at = Some(now);
                break;
            }
            now += 33;
        }
        let at = swapped_at.expect("stall never detected");
        assert!(
            at - last >= th.output_stall_after_ms,
            "swapped {} ms after the last bytes",
            at - last
        );
        assert!(
            at - last <= th.output_stall_after_ms + 33,
            "detected {} ms after the last bytes",
            at - last
        );
        assert_eq!(o.generation(), 2, "a stall swaps immediately; no back-off wait");
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 0,
                since: at
            }
        );
    }

    #[test]
    fn stale_generation_error_is_ignored() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        o.on_progress(10, 1, 4_096);
        o.on_error(20, 1, &th);
        assert!(o.on_tick(1_000, &th), "the 0.5 s back-off ended at 520");
        o.on_progress(1_100, 2, 4_096);
        o.on_error(1_200, 1, &th); // late error from the old sink
        assert!(o.is_connected());
    }

    #[test]
    fn progress_from_old_generation_does_not_keep_new_sink_alive() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        o.on_progress(0, 1, 4_096);
        o.on_error(10, 1, &th);
        assert!(o.on_tick(600, &th), "the 0.5 s back-off ended at 510");
        o.on_progress(700, 2, 4_096);
        for (i, now) in (700..20_000).step_by(100).enumerate() {
            o.on_progress(now, 1, 8_192 + i as u64 * 16_000); // gen 1's total keeps growing
        }
        assert!(
            o.on_tick(700 + th.output_stall_after_ms, &th),
            "gen-1 bytes must not mask a gen-2 stall"
        );
    }

    #[test]
    fn connect_that_hangs_times_out() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        assert!(!o.on_tick(th.output_connect_timeout_ms - 1, &th));
        assert!(matches!(o.state(), OutputState::Connecting { .. }), "1 ms early");
        o.on_tick(th.output_connect_timeout_ms, &th);
        assert!(matches!(o.state(), OutputState::Backoff { attempt: 1, .. }));
    }

    #[test]
    fn backoff_never_exceeds_the_2_s_cap() {
        let th = t();
        let mut o = OutputLink::new();
        let mut now = 0;
        for _ in 0..10 {
            now = next_build(&mut o, now, &th);
            o.on_error(now, o.generation(), &th);
            let OutputState::Backoff { until, .. } = o.state() else {
                panic!("an error must back off, got {:?}", o.state());
            };
            assert!(until - now <= th.output_backoff_cap_ms);
        }
    }

    // ---- connected, progress and its guard ----

    #[test]
    fn first_tick_builds_generation_one_and_waits_for_bytes() {
        let th = t();
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        assert_eq!(o.generation(), 1);
        assert_eq!(o.state(), OutputState::Connecting { attempt: 0, since: 0 });
        assert!(!o.on_tick(1, &th), "no second build while connecting");
        assert!(!o.is_connected());
    }

    // A new sink starts at 0 bytes: a poll that reports 0 is no progress, the first total above it is.
    #[test]
    fn connected_is_the_first_progress_after_a_build() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        o.on_progress(1_000, 1, 0);
        o.on_progress(2_000, 1, 0);
        assert!(!o.is_connected(), "0 bytes sent is not connected");
        o.on_progress(3_000, 1, 4_096);
        assert_eq!(
            o.state(),
            OutputState::Connected {
                last_progress: 3_000,
                sent: 4_096
            }
        );
    }

    #[test]
    fn a_sink_that_never_sends_a_byte_times_out() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        for now in (0..th.output_connect_timeout_ms).step_by(1_000) {
            o.on_progress(now, 1, 0); // C09's poll, every second, reading 0
        }
        assert!(!o.on_tick(th.output_connect_timeout_ms - 1, &th));
        o.on_tick(th.output_connect_timeout_ms, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: th.output_connect_timeout_ms + th.output_backoff_base_ms
            }
        );
    }

    // The guard on the total: a repeat, a backwards total, and one still below the highest are all
    // no progress, so the stall clock keeps running from the last real growth.
    #[test]
    fn a_repeated_or_backwards_total_is_no_progress() {
        let th = t();
        let mut o = connected(1_000, 1_000);
        o.on_progress(1_500, 1, 1_500);
        let grown = OutputState::Connected {
            last_progress: 1_500,
            sent: 1_500,
        };
        assert_eq!(o.state(), grown, "a larger total is progress");
        o.on_progress(2_000, 1, 1_500); // a repeat
        o.on_progress(3_000, 1, 1_200); // backwards
        o.on_progress(4_000, 1, 1_499); // above the first total, below the highest
        assert_eq!(o.state(), grown);
        assert!(!o.on_tick(1_500 + th.output_stall_after_ms - 1, &th));
        assert!(
            o.on_tick(1_500 + th.output_stall_after_ms, &th),
            "stalled since the last growth"
        );
    }

    // ---- sequences: a second call, after an interruption, both generation directions ----

    #[test]
    fn a_second_error_while_backing_off_is_ignored() {
        let th = t();
        let mut o = connected(0, 1_000);
        o.on_error(100, 1, &th);
        let s = o.state();
        o.on_error(150, 1, &th);
        assert_eq!(o.state(), s);
    }

    #[test]
    fn late_bytes_from_a_failed_sink_do_not_revive_it() {
        let th = t();
        let mut o = connected(0, 1_000);
        o.on_error(100, 1, &th);
        let s = o.state();
        o.on_progress(200, 1, 2_000);
        assert_eq!(o.state(), s, "the sink failed; its late bytes change nothing");
    }

    // A stall during back-off is ignored: the bytes stopped 4.9 s before the error, so the whole wait
    // lies past 5 s without bytes, and still nothing is built before the wait ends.
    #[test]
    fn a_stall_during_backoff_is_ignored() {
        let th = t();
        let mut o = connected(0, 1_000);
        let e = th.output_stall_after_ms - 100;
        o.on_error(e, 1, &th);
        let wait = th.output_backoff_base_ms;
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: e + wait
            }
        );
        for now in e..e + wait {
            assert!(!o.on_tick(now, &th), "built at {now}, before the back-off ended");
        }
        assert_eq!(o.generation(), 1);
        assert!(o.on_tick(e + wait, &th));
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 1,
                since: e + wait
            }
        );
    }

    // An output error during a stall swap: the stalled sink errors as it is torn down (old generation,
    // ignored); the new sink's own error backs off as failure 1.
    #[test]
    fn an_error_during_a_stall_swap_counts_only_from_the_new_sink() {
        let th = t();
        let mut o = connected(0, 1_000);
        let swap = th.output_stall_after_ms;
        assert!(o.on_tick(swap, &th));
        assert_eq!(o.generation(), 2);
        o.on_error(swap + 10, 1, &th);
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 0,
                since: swap
            }
        );
        o.on_error(swap + 20, 2, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: swap + 20 + th.output_backoff_base_ms
            }
        );
    }

    // Progress after a swap, from the old generation: the old sink's last poll must not connect the
    // new one, which then times out because nothing did.
    #[test]
    fn old_sink_bytes_after_a_swap_never_connect_the_new_sink() {
        let th = t();
        let mut o = connected(0, 1_000);
        let swap = th.output_stall_after_ms;
        assert!(o.on_tick(swap, &th));
        o.on_progress(swap + 500, 1, 900_000);
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 0,
                since: swap
            }
        );
        assert!(!o.on_tick(swap + th.output_connect_timeout_ms - 1, &th));
        o.on_tick(swap + th.output_connect_timeout_ms, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: swap + th.output_connect_timeout_ms + th.output_backoff_base_ms
            },
            "nothing connected generation 2"
        );
    }

    // Errors while connecting continue the sequence: the C02 brief default, 0.5 s → 2 s.
    #[test]
    fn errors_while_connecting_follow_the_backoff_half_1_2_2() {
        let th = t();
        let mut o = OutputLink::new();
        let mut now = 0;
        let mut waits = Vec::new();
        for _ in 0..5 {
            now = next_build(&mut o, now, &th);
            o.on_error(now, o.generation(), &th);
            let OutputState::Backoff { until, .. } = o.state() else {
                panic!("an error must back off, got {:?}", o.state());
            };
            waits.push(until - now);
        }
        let (base, cap) = (th.output_backoff_base_ms, th.output_backoff_cap_ms);
        let want: Vec<Ms> = (0..5).map(|a| (base << a).min(cap)).collect();
        assert_eq!(want, vec![500, 1_000, 2_000, 2_000, 2_000], "0.5 s → 2 s");
        assert_eq!(waits, want);
    }

    #[test]
    fn a_first_progress_resets_the_backoff_to_its_base() {
        let th = t();
        let mut o = OutputLink::new();
        let mut now = 0;
        for _ in 0..3 {
            now = next_build(&mut o, now, &th);
            o.on_error(now, o.generation(), &th);
        }
        assert!(matches!(o.state(), OutputState::Backoff { attempt: 3, .. }));
        now = next_build(&mut o, now, &th);
        o.on_progress(now, o.generation(), 4_096);
        o.on_error(now + 1, o.generation(), &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: now + 1 + th.output_backoff_base_ms
            }
        );
    }

    // stall → recover → stall: the second stall swaps again, at once.
    #[test]
    fn a_second_stall_after_recovery_swaps_again() {
        let th = t();
        let mut o = connected(0, 1_000);
        let swap = th.output_stall_after_ms;
        assert!(o.on_tick(swap, &th));
        o.on_progress(swap + 300, 2, 4_096);
        assert!(o.is_connected());
        let second = swap + 300 + th.output_stall_after_ms;
        assert!(!o.on_tick(second - 1, &th));
        assert!(o.on_tick(second, &th), "the second stall swaps too");
        assert_eq!(o.generation(), 3);
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 0,
                since: second
            }
        );
    }

    // A generation the domain never built cannot be the current sink's.
    #[test]
    fn a_generation_not_yet_built_is_ignored() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        o.on_progress(10, 2, 4_096);
        assert!(!o.is_connected());
        let mut o = connected(0, 1_000);
        o.on_error(10, 2, &th);
        o.on_progress(20, 2, 9_000);
        assert_eq!(
            o.state(),
            OutputState::Connected {
                last_progress: 0,
                sent: 1_000
            }
        );
    }

    // The clock is a u64 (time.rs): a back-off near its top saturates instead of overflowing.
    #[test]
    fn backoff_from_the_top_of_the_clock_range_saturates() {
        let th = t();
        let top = Ms::MAX - 10;
        let mut o = connected(top, 1_000);
        o.on_error(top + 5, 1, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: Ms::MAX
            }
        );
        assert!(o.on_tick(Ms::MAX, &th), "the build still comes, at the top");
    }
}
