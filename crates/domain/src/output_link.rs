//! Programme push link (RTMPS to CF Input B): a sink is Connected only once its bytes-sent total has
//! kept growing for `output_connected_after_ms`; swap it on an error or when its bytes stall for 5 s;
//! back off 0.5 s → 2 s between failed attempts; ignore every other generation.
//!
//! ```text
//! Backoff --due tick: build--> Connecting --first growth--> Proving --growth >= the bound----> Connected
//!  ^  ^  ^                         |                           |                                 |    |
//!  |  |  +-- timeout, or error ----+                           |                                 |    |
//!  |  +-- error, or no growth for 5 s: one more failure -------+                                 |    |
//!  +-- error: failure 1 (Connected reset the count) ---------------------------------------------+    |
//!                              Connecting <-- no growth for 5 s: a swap at once, attempt 0 ----------+
//! ```
//!
//! - **Liveness is bytes SENT, never bytes acked.** F-005: CF Input B sends no RTMP acknowledgements
//!   (rtmp2sink `out-bytes-acked` stayed 0 for 290 s while `out-bytes-total` grew), so an ack rule
//!   would stall every healthy push and never connect one. The adapter reports the sink's own
//!   monotonic bytes-sent total, counted from 0 for each new sink (C09 feeds `out-bytes-total`).
//! - **Progress** (growth) is a total above the highest this sink has reported. A total that repeats or
//!   goes backwards is no progress: it neither starts, nor proves, nor keeps alive a sink.
//! - **Connected is earned, not given** (F-011, Owed 38). The first nonzero total is the 476 B RTMP
//!   `connect` command, and a refused publish counts 262 KB (CF) to 1 MB (mediamtx) before its error
//!   arrives, so the first growth only starts a **Proving** window. The sink is Connected at the first
//!   growth at least `output_connected_after_ms` after its first growth. An error or a stall ends the
//!   window first, and then the sink never connected.
//! - **The connect timeout** (`output_connect_timeout_ms`; rtmp2sink's "Socket I/O timed out", F-004
//!   rerun 1) runs from the build to the first growth. Once bytes flow, the stall rule takes over.
//! - **Stalled** is no progress for `output_stall_after_ms`. From Connected the sink is swapped at once,
//!   with no back-off and the count reset. While Proving it is a failed attempt like any other.
//! - **Back-off.** Every end before Connected is one more consecutive failure: a connect that times
//!   out, an error, or a stall while proving. Failure number f waits `delay(f - 1)`: 0.5, 1, 2, 2 s. It
//!   never gives up. **Only Connected resets it**: an error from Connected is failure 1 again, and a
//!   Connected sink's stall swaps with the count at 0.
//! - **Generations.** Every build is a new generation, and an event from any other generation is
//!   ignored in both directions: a late error from a torn-down sink cannot knock down its replacement,
//!   and late bytes from it can neither start, prove nor keep alive the replacement.
//! - **The 15 s budget** (`OUTPUT_GAP_BUDGET_MS`, F-004) holds from an error through one failed
//!   connect, not two: `tests/links.rs` measures every recovery path, and docs/_INDEX.md records the
//!   paths that exceed it. The gap ends when the new sink's bytes flow, not when it is Connected.
//! - **Every build carries its reason** (`SwapReason`, Owed 87): what ended the sink before it. An
//!   error is the sink's own (rtmp2sink's socket timeout among them); a connect timeout and a stall
//!   are this module's rules. A build after a back-off carries the failure that started it, and the
//!   first build carries `First`.
use crate::backoff::delay;
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputState {
    /// The current sink is being built and has sent no byte; `attempt` consecutive failures came
    /// before it.
    Connecting { attempt: u32, since: Ms },
    /// The sink's total has grown, but not yet for `output_connected_after_ms`. `first_progress` is its
    /// first growth, `last_progress` its latest, `sent` the highest total; `attempt` failures came
    /// before it and still count until it is Connected.
    Proving {
        attempt: u32,
        first_progress: Ms,
        last_progress: Ms,
        sent: u64,
    },
    /// The sink earned Connected. `last_progress` is when the total last grew; `sent` is the highest
    /// total this sink has reported.
    Connected { last_progress: Ms, sent: u64 },
    /// `attempt` consecutive failures so far; the next sink is built at the first tick at or after
    /// `until`.
    Backoff { attempt: u32, until: Ms },
}

/// Why a sink generation was built: what ended the one before it (Owed 87).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapReason {
    /// Generation 1: no sink came before it.
    First,
    /// The sink before it reported a failure (`on_error`): a bus error, rtmp2sink's own socket
    /// timeout and a refused publish among them.
    Error,
    /// The sink before it sent no byte within `output_connect_timeout_ms` of its build.
    ConnectTimeout,
    /// The stall rule: the sink before it sent nothing new for `output_stall_after_ms`, proving or
    /// Connected.
    Stall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLink {
    state: OutputState,
    generation: u64,
    /// Why the current generation was built.
    built_for: SwapReason,
    /// What ended the last sink: the reason the next build carries.
    ended: SwapReason,
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
            built_for: SwapReason::First,
            ended: SwapReason::First,
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

    /// Why the current generation was built (`First` before any build).
    pub fn swap_reason(&self) -> SwapReason {
        self.built_for
    }

    /// The current sink's bytes-sent total, counted from 0 when the sink was built (C09: rtmp2sink
    /// `out-bytes-total`). Only a total above the highest one reported counts as progress.
    pub fn on_progress(&mut self, now: Ms, gen: u64, total_bytes: u64, t: &Thresholds) {
        if gen != self.generation {
            return;
        }
        self.state = match self.state {
            // A new sink counts from 0: its first growth starts the proof, and never connects it.
            OutputState::Connecting { attempt, .. } if total_bytes > 0 => OutputState::Proving {
                attempt,
                first_progress: now,
                last_progress: now,
                sent: total_bytes,
            },
            OutputState::Proving {
                attempt,
                first_progress,
                sent,
                ..
            } if total_bytes > sent => {
                if elapsed(now, first_progress) >= t.output_connected_after_ms {
                    OutputState::Connected {
                        last_progress: now,
                        sent: total_bytes,
                    }
                } else {
                    OutputState::Proving {
                        attempt,
                        first_progress,
                        last_progress: now,
                        sent: total_bytes,
                    }
                }
            }
            OutputState::Connected { sent, .. } if total_bytes > sent => OutputState::Connected {
                last_progress: now,
                sent: total_bytes,
            },
            // No growth; or the sink failed, and its late bytes change nothing (the next build
            // replaces it).
            unchanged => unchanged,
        };
    }

    /// The current sink failed: a bus error, including a connect that timed out in rtmp2sink.
    pub fn on_error(&mut self, now: Ms, gen: u64, t: &Thresholds) {
        if gen != self.generation {
            return;
        }
        let failed_before = match self.state {
            OutputState::Connected { .. } => 0,
            // Not yet Connected: the failures before this sink still count (a refused publish).
            OutputState::Connecting { attempt, .. } | OutputState::Proving { attempt, .. } => attempt,
            // Already failed: a second error from the same sink changes nothing.
            OutputState::Backoff { .. } => return,
        };
        self.back_off(now, failed_before, t, SwapReason::Error);
    }

    /// `true` ⇒ effect `SwapSink(generation())`, its reason `swap_reason()`.
    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool {
        match self.state {
            OutputState::Backoff { attempt, until } if now >= until => self.build(now, attempt, self.ended),
            OutputState::Connecting { attempt, since }
                if elapsed(now, since) >= t.output_connect_timeout_ms =>
            {
                self.back_off(now, attempt, t, SwapReason::ConnectTimeout);
                false
            }
            // The bytes stopped before the sink earned Connected: a failed attempt, so it waits.
            OutputState::Proving {
                attempt,
                last_progress,
                ..
            } if elapsed(now, last_progress) >= t.output_stall_after_ms => {
                self.back_off(now, attempt, t, SwapReason::Stall);
                false
            }
            // Stalled: swap at once. The stall already spent 5 s of the 15 s budget.
            OutputState::Connected { last_progress, .. }
                if elapsed(now, last_progress) >= t.output_stall_after_ms =>
            {
                self.build(now, 0, SwapReason::Stall)
            }
            _ => false,
        }
    }

    /// The next sink generation, carrying the failures before it and the reason for it.
    fn build(&mut self, now: Ms, attempt: u32, reason: SwapReason) -> bool {
        self.generation += 1;
        self.state = OutputState::Connecting { attempt, since: now };
        self.built_for = reason;
        true
    }

    /// Failure number `failed_before + 1` waits `delay(failed_before)`. Both additions saturate: the
    /// clock is a u64 (time.rs), and "never gives up" has no last attempt.
    /// `ended` is what ended the sink: the next build's reason.
    fn back_off(&mut self, now: Ms, failed_before: u32, t: &Thresholds, ended: SwapReason) {
        let wait = delay(failed_before, t.output_backoff_base_ms, t.output_backoff_cap_ms);
        self.state = OutputState::Backoff {
            attempt: failed_before.saturating_add(1),
            until: now.saturating_add(wait),
        };
        self.ended = ended;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Thresholds {
        Thresholds::default()
    }

    /// Build at `built`, the first bytes at once, and growth to `bytes` (above 1) at the bound: a link
    /// Connected at generation 1 with `bytes` sent. Returns it and the instant it connected.
    fn connected(built: Ms, bytes: u64) -> (OutputLink, Ms) {
        let th = t();
        let mut o = OutputLink::new();
        assert!(o.on_tick(built, &th));
        o.on_progress(built, 1, 1, &th);
        let c = built + th.output_connected_after_ms;
        o.on_progress(c, 1, bytes, &th);
        assert_eq!(
            o.state(),
            OutputState::Connected {
                last_progress: c,
                sent: bytes
            }
        );
        (o, c)
    }

    /// Ticks forward from `now` in 10 ms steps until the next build; returns its time. With no bytes
    /// fed, the next build is due within the longer of the connect timeout (Connecting gives up) and
    /// the stall (Proving fails, Connected swaps), plus the capped back-off, so a link that never
    /// builds fails here instead of hanging the suite.
    fn next_build(o: &mut OutputLink, mut now: Ms, th: &Thresholds) -> Ms {
        const STEP: Ms = 10;
        let horizon = now
            + th.output_connect_timeout_ms.max(th.output_stall_after_ms)
            + th.output_backoff_cap_ms
            + STEP;
        while !o.on_tick(now, th) {
            now += STEP;
            assert!(now <= horizon, "no build by {horizon} ms: {:?}", o.state());
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
                        o.on_progress(now, gen, 4_096, &th);
                        assert!(
                            matches!(o.state(), OutputState::Proving { .. }),
                            "the first bytes start the proof: the output is back"
                        );
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
        o.on_progress(100, 1, 16_000, &th);
        let (mut now, mut total) = (100, 16_000);
        while now < 10_000 {
            total += 16_000;
            o.on_progress(now, 1, total, &th);
            now += 33;
        } // healthy until 10 s; then the bytes stop and every poll reports the same total
        assert!(o.is_connected(), "setup: 9.9 s of growth earns Connected");
        let last = now - 33;
        let mut swapped_at = None;
        while now < 20_000 {
            o.on_progress(now, 1, total, &th);
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
        o.on_progress(10, 1, 4_096, &th);
        o.on_error(20, 1, &th);
        assert!(o.on_tick(1_000, &th), "the 0.5 s back-off ended at 520");
        o.on_progress(1_100, 2, 4_096, &th);
        let c = 1_100 + th.output_connected_after_ms;
        o.on_progress(c, 2, 8_192, &th);
        assert!(o.is_connected(), "setup: gen 2 earned Connected");
        o.on_error(c + 100, 1, &th); // late error from the old sink
        assert!(o.is_connected());
    }

    #[test]
    fn progress_from_old_generation_does_not_keep_new_sink_alive() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        o.on_progress(0, 1, 4_096, &th);
        o.on_error(10, 1, &th);
        assert!(o.on_tick(600, &th), "the 0.5 s back-off ended at 510");
        o.on_progress(700, 2, 4_096, &th);
        let mut proving = o.clone(); // the same sink, still proving: its stall is a failure
        let c = 700 + th.output_connected_after_ms;
        o.on_progress(c, 2, 8_192, &th);
        assert!(o.is_connected(), "setup: gen 2 earned Connected");
        for (i, now) in (700..30_000).step_by(100).enumerate() {
            let total = 8_192 + i as u64 * 16_000; // gen 1's total keeps growing
            o.on_progress(now, 1, total, &th);
            proving.on_progress(now, 1, total, &th);
        }
        assert!(
            o.on_tick(c + th.output_stall_after_ms, &th),
            "gen-1 bytes must not mask a gen-2 stall"
        );
        assert!(!proving.on_tick(700 + th.output_stall_after_ms, &th));
        assert!(
            matches!(proving.state(), OutputState::Backoff { attempt: 2, .. }),
            "gen-1 bytes must not mask a proving gen-2 stall either: {:?}",
            proving.state()
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

    // A new sink starts at 0 bytes: a poll that reports 0 is no progress, and the first total above it
    // starts the proof (Owed 38: it never connects the sink by itself).
    #[test]
    fn the_first_progress_after_a_build_starts_the_proof() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        o.on_progress(1_000, 1, 0, &th);
        o.on_progress(2_000, 1, 0, &th);
        assert_eq!(
            o.state(),
            OutputState::Connecting { attempt: 0, since: 0 },
            "0 bytes sent is no progress"
        );
        o.on_progress(3_000, 1, 4_096, &th);
        assert_eq!(
            o.state(),
            OutputState::Proving {
                attempt: 0,
                first_progress: 3_000,
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
            o.on_progress(now, 1, 0, &th); // C09's poll, every second, reading 0
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
        let (mut o, c) = connected(1_000, 1_000);
        o.on_progress(c + 500, 1, 1_500, &th);
        let grown = OutputState::Connected {
            last_progress: c + 500,
            sent: 1_500,
        };
        assert_eq!(o.state(), grown, "a larger total is progress");
        o.on_progress(c + 1_000, 1, 1_500, &th); // a repeat
        o.on_progress(c + 2_000, 1, 1_200, &th); // backwards
        o.on_progress(c + 3_000, 1, 1_499, &th); // above the first total, below the highest
        assert_eq!(o.state(), grown);
        assert!(!o.on_tick(c + 500 + th.output_stall_after_ms - 1, &th));
        assert!(
            o.on_tick(c + 500 + th.output_stall_after_ms, &th),
            "stalled since the last growth"
        );
    }

    // ---- sequences: a second call, after an interruption, both generation directions ----

    #[test]
    fn a_second_error_while_backing_off_is_ignored() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        o.on_error(c + 100, 1, &th);
        let s = o.state();
        o.on_error(c + 150, 1, &th);
        assert_eq!(o.state(), s);
    }

    #[test]
    fn late_bytes_from_a_failed_sink_do_not_revive_it() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        o.on_error(c + 100, 1, &th);
        let s = o.state();
        o.on_progress(c + 200, 1, 2_000, &th);
        assert_eq!(o.state(), s, "the sink failed; its late bytes change nothing");
    }

    // A stall during back-off is ignored: the bytes stopped 4.9 s before the error, so the whole wait
    // lies past 5 s without bytes, and still nothing is built before the wait ends.
    #[test]
    fn a_stall_during_backoff_is_ignored() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        let e = c + th.output_stall_after_ms - 100;
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
        let (mut o, c) = connected(0, 1_000);
        let swap = c + th.output_stall_after_ms;
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
        let (mut o, c) = connected(0, 1_000);
        let swap = c + th.output_stall_after_ms;
        assert!(o.on_tick(swap, &th));
        o.on_progress(swap + 500, 1, 900_000, &th);
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

    // Owed 38 reversed C04's "a first progress resets the back-off": the first bytes after three
    // failures leave the count at 3, so an error 1 ms later is failure 4 and waits the cap.
    #[test]
    fn a_first_progress_no_longer_resets_the_backoff() {
        let th = t();
        let mut o = OutputLink::new();
        let mut now = 0;
        for _ in 0..3 {
            now = next_build(&mut o, now, &th);
            o.on_error(now, o.generation(), &th);
        }
        assert!(matches!(o.state(), OutputState::Backoff { attempt: 3, .. }));
        now = next_build(&mut o, now, &th);
        o.on_progress(now, o.generation(), 4_096, &th);
        o.on_error(now + 1, o.generation(), &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 4,
                until: now + 1 + th.output_backoff_cap_ms
            },
            "failure 4 waits 0.5 · 2^3, capped at 2 s"
        );
    }

    // stall → recover → stall: the second stall swaps again, at once.
    #[test]
    fn a_second_stall_after_recovery_swaps_again() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        let swap = c + th.output_stall_after_ms;
        assert!(o.on_tick(swap, &th));
        o.on_progress(swap + 300, 2, 4_096, &th);
        let c2 = swap + 300 + th.output_connected_after_ms;
        o.on_progress(c2, 2, 8_192, &th);
        assert!(o.is_connected());
        let second = c2 + th.output_stall_after_ms;
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
        o.on_progress(10, 2, 4_096, &th);
        assert_eq!(o.state(), OutputState::Connecting { attempt: 0, since: 0 });
        let (mut o, c) = connected(0, 1_000);
        o.on_error(c + 10, 2, &th);
        o.on_progress(c + 20, 2, 9_000, &th);
        assert_eq!(
            o.state(),
            OutputState::Connected {
                last_progress: c,
                sent: 1_000
            }
        );
    }

    // The clock is a u64 (time.rs): a back-off near its top saturates instead of overflowing.
    #[test]
    fn backoff_from_the_top_of_the_clock_range_saturates() {
        let th = t();
        let (mut o, top) = connected(Ms::MAX - 10 - th.output_connected_after_ms, 1_000);
        assert_eq!(top, Ms::MAX - 10);
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

    // ---- Owed 38 (F-011): Connected is earned by sustained growth, not by the first bytes ----

    /// The current sink's total, as C09's poll reports it.
    fn feed(o: &mut OutputLink, now: Ms, gen: u64, total: u64, th: &Thresholds) {
        o.on_progress(now, gen, total, th);
    }

    /// F-011, CF, a made-up key on the same host and app. rtmp2sink's first nonzero total is the 476 B
    /// `connect` command (22–69 ms, median 31, on the healthy legs); the refused leg then counted
    /// 52 769 B at 138 ms and 262 334 B at 538 ms, and "connection closed remotely" arrived at 522 ms.
    const CF_REFUSED: [(Ms, u64); 3] = [(31, 476), (138, 52_769), (538, 262_334)];
    const CF_REFUSED_ERROR_AT: Ms = 522;

    /// F-011, mediamtx, a publish-denied path: the adapter reported 9 growing totals, up to
    /// 1 008 779 B, for 2 s on its 250 ms poll, and `OutputError { gen: 1 }` came at 2 033 ms.
    const MTX_REFUSED_ERROR_AT: Ms = 2_033;
    fn mtx_refused() -> Vec<(Ms, u64)> {
        let mut v = vec![(31, 476)]; // the connect command, as on CF
        for k in 1..=8u64 {
            v.push((250 * k, 476 + (1_008_779 - 476) * k / 8));
        }
        assert_eq!(v.len(), 9, "F-011: 9 growing totals");
        assert_eq!(
            v.last(),
            Some(&(2_000, 1_008_779)),
            "F-011: up to 1 008 779 B in 2 s"
        );
        v
    }

    /// One refused publish on the sink just built at `built`: the totals at their offsets, the error
    /// at its offset, and any total counted after it. Asserts the link is never Connected.
    fn refuse(o: &mut OutputLink, built: Ms, totals: &[(Ms, u64)], error_at: Ms, th: &Thresholds) {
        let gen = o.generation();
        for &(at, total) in totals.iter().filter(|(at, _)| *at < error_at) {
            feed(o, built + at, gen, total, th);
            assert!(
                !o.is_connected(),
                "a refused publish is Connected at +{at} ms: {:?}",
                o.state()
            );
        }
        o.on_error(built + error_at, gen, th);
        for &(at, total) in totals.iter().filter(|(at, _)| *at >= error_at) {
            feed(o, built + at, gen, total, th); // counted after the refusal: the sink already failed
        }
        assert!(!o.is_connected());
    }

    #[test]
    fn a_refused_publish_on_cf_is_not_connected_and_backs_off_as_a_failure() {
        let th = t();
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        refuse(&mut o, 0, &CF_REFUSED, CF_REFUSED_ERROR_AT, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: CF_REFUSED_ERROR_AT + th.output_backoff_base_ms
            },
            "failure 1 waits the base"
        );
    }

    #[test]
    fn a_refused_publish_on_mediamtx_is_not_connected_and_backs_off_as_a_failure() {
        let th = t();
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        refuse(&mut o, 0, &mtx_refused(), MTX_REFUSED_ERROR_AT, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: MTX_REFUSED_ERROR_AT + th.output_backoff_base_ms
            }
        );
    }

    // F-011's consequence: a wrong stream key retried at the base back-off for the whole session.
    // Each refusal is a failure, so the waits follow the brief's 0.5, 1, 2, 2 s.
    #[test]
    fn refusals_in_a_row_keep_growing_the_back_off() {
        let th = t();
        let mut o = OutputLink::new();
        let mut now = 0;
        let mut waits = Vec::new();
        for (i, (totals, error_at)) in [
            (CF_REFUSED.to_vec(), CF_REFUSED_ERROR_AT),
            (mtx_refused(), MTX_REFUSED_ERROR_AT),
            (CF_REFUSED.to_vec(), CF_REFUSED_ERROR_AT),
            (mtx_refused(), MTX_REFUSED_ERROR_AT),
        ]
        .into_iter()
        .enumerate()
        {
            now = next_build(&mut o, now, &th);
            assert_eq!(
                o.state(),
                OutputState::Connecting {
                    attempt: i as u32,
                    since: now
                },
                "build {i} carries every refusal before it"
            );
            refuse(&mut o, now, &totals, error_at, &th);
            let OutputState::Backoff { attempt, until } = o.state() else {
                panic!("a refusal must back off, got {:?}", o.state());
            };
            assert_eq!(attempt, i as u32 + 1, "refusal {i}");
            waits.push(until - (now + error_at));
        }
        assert_eq!(
            waits,
            vec![500, 1_000, 2_000, 2_000],
            "the brief's 0.5, 1, 2, 2 s"
        );
    }

    /// F-011, CF, a healthy publish: 476 B at 31 ms (the median first nonzero total), then about
    /// 394 KB/s on C09's 250 ms poll, every sample larger than the last.
    fn cf_healthy_total(since_first: Ms) -> u64 {
        476 + 394 * since_first
    }

    #[test]
    fn sustained_growth_connects_at_the_bound_and_resets_the_back_off() {
        let th = t();
        let mut o = OutputLink::new();
        let mut now = 0;
        for _ in 0..2 {
            now = next_build(&mut o, now, &th);
            refuse(&mut o, now, &mtx_refused(), MTX_REFUSED_ERROR_AT, &th);
        }
        let built = next_build(&mut o, now, &th);
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 2,
                since: built
            }
        );
        let first = built + 31;
        let n = th.output_connected_after_ms;
        let mut at = first;
        while at < first + n {
            feed(&mut o, at, 3, cf_healthy_total(at - first), &th);
            assert!(!o.is_connected(), "Connected {} ms into the proof", at - first);
            at += 250;
        }
        assert_eq!(at, first + n, "setup: a poll lands on the bound");
        feed(&mut o, at, 3, cf_healthy_total(at - first), &th);
        assert_eq!(
            o.state(),
            OutputState::Connected {
                last_progress: first + n,
                sent: cf_healthy_total(n)
            }
        );
        // A later error is failure 1 again: Connected reset the two refusals.
        let e = first + n + 20_000;
        o.on_error(e, 3, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: e + th.output_backoff_base_ms
            }
        );
    }

    // The bound is inclusive: growth exactly `output_connected_after_ms` after the first connects,
    // 1 ms earlier does not. Under thresholds where the bound, the stall and the connect timeout all
    // differ, so a bound that read another field moves.
    #[test]
    fn the_bound_is_measured_from_the_first_growth_and_is_inclusive() {
        let distinct = Thresholds {
            output_connected_after_ms: 3_100,
            output_stall_after_ms: 4_300,
            output_connect_timeout_ms: 6_700,
            ..t()
        };
        for th in [t(), distinct] {
            let n = th.output_connected_after_ms;
            let mut o = OutputLink::new();
            assert!(o.on_tick(0, &th));
            feed(&mut o, 40, 1, 476, &th);
            let mut at = 40;
            while at + 1_000 < 40 + n - 1 {
                at += 1_000;
                feed(&mut o, at, 1, 476 + at, &th);
            }
            feed(&mut o, 40 + n - 1, 1, 10_000_000, &th);
            assert!(!o.is_connected(), "{n}: Connected 1 ms before the bound");
            assert!(
                !o.on_tick(40 + n - 1, &th),
                "{n}: no tick may end a growing proof"
            );
            feed(&mut o, 40 + n, 1, 10_000_001, &th);
            assert_eq!(
                o.state(),
                OutputState::Connected {
                    last_progress: 40 + n,
                    sent: 10_000_001
                },
                "{n}: Connected at the bound"
            );
        }
    }

    // Once bytes flow, the connect timeout no longer applies: a sink whose first bytes come late
    // proves itself for the whole bound, past `output_connect_timeout_ms` from its build.
    #[test]
    fn the_connect_timeout_ends_at_the_first_growth() {
        let th = t();
        let n = th.output_connected_after_ms;
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        let first = th.output_connect_timeout_ms - 1;
        feed(&mut o, first, 1, 476, &th);
        let mut at = first;
        while at < first + n {
            assert!(!o.on_tick(at, &th), "built at {at}");
            assert_eq!(o.generation(), 1);
            assert!(!o.is_connected(), "Connected at {at}");
            at += 250;
            feed(&mut o, at, 1, cf_healthy_total(at - first), &th);
        }
        assert!(o.is_connected(), "Connected at {at}: {:?}", o.state());
    }

    // A generation change mid-window: the old sink's bytes neither start nor finish the new sink's
    // proof, whether the change came from a refusal or from a stall swap.
    #[test]
    fn a_generation_change_mid_window_restarts_the_proof() {
        let th = t();
        let n = th.output_connected_after_ms;
        // (1) Gen 1 proves for 3 s, then its publish is refused; gen 1's bytes keep being polled.
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        for k in 0..=12 {
            feed(&mut o, 30 + 250 * k, 1, 476 + 100_000 * k, &th);
        }
        o.on_error(3_100, 1, &th);
        let built = next_build(&mut o, 3_100, &th);
        assert_eq!(o.generation(), 2);
        let timeout = th.output_connect_timeout_ms;
        let mut g1 = 2_000_000;
        for at in (built..built + timeout).step_by(250) {
            g1 += 100_000;
            feed(&mut o, at, 1, g1, &th);
            assert_eq!(
                o.state(),
                OutputState::Connecting {
                    attempt: 1,
                    since: built
                },
                "gen 1's bytes at {at} moved gen 2"
            );
            assert!(!o.on_tick(at, &th));
        }
        o.on_tick(built + timeout, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 2,
                until: built + timeout + 2 * th.output_backoff_base_ms
            },
            "gen 2 never sent a byte of its own, so its connect timed out"
        );
        // (2) Gen 2's own bytes start its proof, and gen 1's growing totals beside them change nothing.
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        o.on_error(10, 1, &th);
        let built = next_build(&mut o, 10, &th);
        let first = built + 31;
        let mut at = first;
        let (mut g1, mut g2) = (5_000_000, 476);
        while at < first + n {
            feed(&mut o, at, 2, g2, &th);
            g1 += 1_000_000;
            feed(&mut o, at + 1, 1, g1, &th);
            feed(&mut o, at + 2, 3, g1, &th); // a generation never built
            assert!(
                !o.is_connected(),
                "Connected {} ms into gen 2's proof",
                at - first
            );
            g2 += 98_500;
            at += 250;
        }
        feed(&mut o, at, 2, g2, &th);
        assert!(o.is_connected(), "gen 2 connects on its own bound");
        // (3) A stall swap from a Connected gen 1: its late, growing totals never start gen 2's proof.
        let (mut o, last) = connected(0, 1_000);
        let swap = last + th.output_stall_after_ms;
        assert!(o.on_tick(swap, &th));
        for k in 1..=40 {
            feed(&mut o, swap + 100 * k, 1, 2_000 * k, &th);
        }
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 0,
                since: swap
            },
            "gen 1's bytes after the swap"
        );
    }

    // Growth that stops before the bound: the stall rule ends the proof (5 s after the last growth,
    // not 1 ms sooner), and since the sink never earned Connected it is a failure: it backs off.
    #[test]
    fn growth_that_stops_before_the_bound_stalls_and_counts_as_a_failure() {
        let distinct = Thresholds {
            output_connected_after_ms: 6_100,
            output_stall_after_ms: 4_300,
            output_connect_timeout_ms: 7_700,
            ..t()
        };
        for th in [t(), distinct] {
            let stall = th.output_stall_after_ms;
            let mut o = OutputLink::new();
            let mut now = 0;
            now = next_build(&mut o, now, &th);
            refuse(&mut o, now, &CF_REFUSED, CF_REFUSED_ERROR_AT, &th);
            let built = next_build(&mut o, now, &th);
            let mut last = built;
            for k in 0..12 {
                last = built + 31 + 250 * k; // 2.75 s of growth, then the bytes stop
                feed(&mut o, last, 2, 476 + 50_000 * k, &th);
            }
            for at in (last..last + stall).step_by(33).chain([last + stall - 1]) {
                feed(&mut o, at, 2, 476 + 50_000 * 11, &th); // the poll repeats the frozen total
                assert!(!o.on_tick(at, &th), "{stall}: ended at {at}, before the stall");
                assert!(!o.is_connected());
            }
            assert!(!o.on_tick(last + stall, &th), "a failure waits; it is not a swap");
            assert_eq!(
                o.state(),
                OutputState::Backoff {
                    attempt: 2,
                    until: last + stall + 2 * th.output_backoff_base_ms
                },
                "{stall}: the second failure waits twice the base"
            );
            assert_eq!(o.generation(), 2);
        }
    }

    // A total that repeats or goes backwards while proving is no growth: it does not connect at the
    // bound, and the stall clock keeps running from the last real growth.
    #[test]
    fn a_repeated_total_while_proving_is_no_progress() {
        let th = t();
        let (n, stall) = (th.output_connected_after_ms, th.output_stall_after_ms);
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        feed(&mut o, 30, 1, 476, &th);
        feed(&mut o, 2_030, 1, 900_000, &th);
        feed(&mut o, 30 + n, 1, 900_000, &th); // a repeat, at the bound
        feed(&mut o, 30 + n + 1, 1, 899_999, &th); // backwards
        assert!(!o.is_connected(), "a repeat is no growth");
        assert!(!o.on_tick(2_030 + stall - 1, &th));
        o.on_tick(2_030 + stall, &th);
        assert!(
            matches!(o.state(), OutputState::Backoff { attempt: 1, .. }),
            "stalled since the last growth: {:?}",
            o.state()
        );
    }

    // ---- Owed 87: every build names what ended the sink before it ----

    /// Ticks to the next build (`next_build`) and checks it is generation `gen`, built for `want`;
    /// returns its time.
    fn swap_for(o: &mut OutputLink, now: Ms, th: &Thresholds, gen: u64, want: SwapReason) -> Ms {
        let at = next_build(o, now, th);
        assert_eq!(o.generation(), gen, "built at {at}");
        assert_eq!(o.swap_reason(), want, "generation {gen}, built at {at}");
        at
    }

    // One link through every way a sink ends, in the module doc's words: the first build, the sink's
    // own error, the connect timeout, the stall rule while proving and while Connected. An end from
    // another generation, or a second one from a sink that already failed, names nothing.
    #[test]
    fn each_build_names_what_ended_the_sink_before_it() {
        let th = t();
        let stall = th.output_stall_after_ms;
        let mut o = OutputLink::new();
        let mut checked = 0;
        assert_eq!(o.swap_reason(), SwapReason::First, "before any build");

        let mut now = swap_for(&mut o, 0, &th, 1, SwapReason::First);
        checked += 1;
        o.on_error(now + 100, 1, &th);
        assert_eq!(
            o.swap_reason(),
            SwapReason::First,
            "a back-off does not rename the generation still in place"
        );
        now = swap_for(&mut o, now + 100, &th, 2, SwapReason::Error);
        checked += 1;

        // Generation 2 sends nothing: the connect timeout.
        now = swap_for(&mut o, now, &th, 3, SwapReason::ConnectTimeout);
        checked += 1;

        // Generation 3's bytes stop while it proves: the stall rule, a failure that waits.
        feed(&mut o, now + 31, 3, 476, &th);
        let last = now + 1_000;
        feed(&mut o, last, 3, 52_000, &th);
        assert!(!o.on_tick(last + stall, &th), "a failure waits");
        now = swap_for(&mut o, last + stall, &th, 4, SwapReason::Stall);
        checked += 1;

        // Generation 4 earns Connected, then stalls: the stall rule, a swap at once.
        feed(&mut o, now + 31, 4, 476, &th);
        let c = now + 31 + th.output_connected_after_ms;
        feed(&mut o, c, 4, 900_000, &th);
        assert!(o.is_connected());
        assert!(!o.on_tick(c + stall - 1, &th));
        assert!(o.on_tick(c + stall, &th), "a Connected stall swaps at once");
        assert_eq!((o.generation(), o.swap_reason()), (5, SwapReason::Stall));
        now = c + stall;
        checked += 1;

        // Generation 5 errors after a stall swap: the latest end names the build.
        o.on_error(now + 200, 5, &th);
        now = swap_for(&mut o, now + 200, &th, 6, SwapReason::Error);
        checked += 1;

        // A late error from generation 5 changes nothing; generation 6 times out.
        o.on_error(now + 10, 5, &th);
        assert!(matches!(o.state(), OutputState::Connecting { .. }));
        now = swap_for(&mut o, now + 10, &th, 7, SwapReason::ConnectTimeout);
        checked += 1;

        // Generation 7 times out, and its error then arrives in the back-off: still the timeout.
        let timed_out = now + th.output_connect_timeout_ms;
        assert!(!o.on_tick(timed_out, &th));
        assert!(matches!(o.state(), OutputState::Backoff { .. }));
        o.on_error(timed_out + 1, 7, &th);
        swap_for(&mut o, timed_out + 1, &th, 8, SwapReason::ConnectTimeout);
        checked += 1;

        assert_eq!(checked, 8, "builds checked");
    }
}
