//! Programme push link (RTMPS to CF Input B): a sink is Connected at its first growth at least
//! `output_connected_after_ms` after its first growth, with no error between (a silence between is
//! no failure since Owed 87 while the encoders starve or are unread; with both delivering, a silence
//! of `output_egress_flat_ms` is the backstop's, Owed 94); swap it on its error, or when its egress
//! stays flat while the encoders deliver; back off 0.5 s → 2 s between failed attempts; ignore every
//! other generation.
//!
//! ```text
//! Backoff --due tick: build--> Connecting --first growth--> Proving --growth >= the bound--> Connected
//!  ^  ^  ^                         |                           |                                |
//!  |  |  +-- timeout, or error ----+                           |                                |
//!  |  +-- error, or egress flat: one more failure -------------+                                |
//!  +-- error, or egress flat: failure 1 (Connected reset the count) ----------------------------+
//! ```
//!
//! - **Liveness is bytes SENT, never bytes acked.** F-005: CF Input B sends no RTMP acknowledgements
//!   (rtmp2sink `out-bytes-acked` stayed 0 for 290 s while `out-bytes-total` grew), so an ack rule
//!   would never connect a healthy push. The adapter reports the sink's own monotonic bytes-sent
//!   total, counted from 0 for each new sink (C09 feeds `out-bytes-total`).
//! - **Progress** (growth) is a total above the highest this sink has reported. A total that repeats or
//!   goes backwards is no progress: it neither starts nor proves a sink.
//! - **Connected is earned, not given** (F-011, Owed 38). The first nonzero total is the 476 B RTMP
//!   `connect` command, and a refused publish counts 262 KB (CF) to 1 MB (mediamtx) before its error
//!   arrives, so the first growth only starts a **Proving** window. The sink is Connected at the first
//!   growth at least `output_connected_after_ms` after its first growth. An error ends the window
//!   first, and then the sink never connected.
//! - **The connect timeout** (`output_connect_timeout_ms`; rtmp2sink's "Socket I/O timed out", F-004
//!   rerun 1) runs from the build to the first growth; the egress backstop runs after it.
//! - **No stall rule** (Owed 87; the owner, 2026-10-24, "A"). A destination that stops taking bytes
//!   fails the sink's socket wait after `OUTPUT_SINK_TIMEOUT_MS` (rtmp2sink's `timeout`, which C09
//!   sets), and that error is the swap's cause. The retired rule ("no growth for 5 s") timed the same
//!   gap from the same write: rtmp2 counts a message only once all of it is written, and the socket's
//!   wait won 17 of 17 CI stalls. Except the multi-wake trickle (the PR #73 review's P-3): a
//!   destination that drains each message across several waits, each under the timeout, never errors,
//!   and there the backstop below is the only detector.
//! - **The egress backstop** (Owed 94; the owner, 2026-10-24, "B"). A Proving or Connected sink whose
//!   total has not grown for `output_egress_flat_ms` while both encoders kept delivering is swapped
//!   (`SwapReason::EgressStalled`): a hang inside the output pipeline that no sink error reports. The
//!   clock runs from the later of the sink's last growth and the start of the encoders' current run
//!   of delivery, so feed starvation (encoders that deliver nothing) never counts, and it is time, not
//!   a count of readings: readings that stop altogether leave it running. A reading the full channel
//!   dropped that carried no growth changes nothing; one that carried growth leaves the last growth
//!   older, so the backstop can fire up to a poll early for each (the PR #76 review's m-6). It starts
//!   again with each sink. It fires after the sink's own error would when one socket wait outlasts the
//!   timeout (`config`'s ordering test), so a stall rtmp2sink reports is still that error's; a
//!   multi-wake trickle, which never errors, is the backstop's alone.
//! - **Back-off.** Every end before Connected is one more consecutive failure: a connect that times
//!   out, or an error. Failure number f waits `delay(f - 1)`: 0.5, 1, 2, 2 s. It never gives up.
//!   **Only Connected resets it**: an error from Connected is failure 1 again.
//! - **Generations.** Every build is a new generation, and an event from any other generation is
//!   ignored in both directions: a late error from a torn-down sink cannot knock down its replacement,
//!   and late bytes from it can neither start nor prove the replacement.
//! - **The 15 s budget** (`OUTPUT_GAP_BUDGET_MS`, F-004) holds from an error through one failed
//!   connect, not two: `tests/links.rs` measures every recovery path, and docs/_INDEX.md records the
//!   paths that exceed it. The gap ends when the new sink's bytes flow, not when it is Connected.
//! - **Every build carries its reason** (`SwapReason`, Owed 87): what ended the sink before it. An
//!   error is the sink's own (rtmp2sink's socket timeout among them); the connect timeout and the
//!   egress backstop are this module's rules. A build after a back-off carries the failure that
//!   started it, and the first build carries `First`.
use crate::backoff::delay;
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputState {
    /// The current sink is being built and has sent no byte; `attempt` consecutive failures came
    /// before it.
    Connecting { attempt: u32, since: Ms },
    /// The sink's total has grown, but not yet for `output_connected_after_ms`. `first_progress` is its
    /// first growth, `sent` the highest total; `attempt` failures came before it and still count until
    /// it is Connected.
    Proving {
        attempt: u32,
        first_progress: Ms,
        sent: u64,
    },
    /// The sink earned Connected. Its error ends it, or the egress backstop (Owed 94).
    Connected,
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
    /// timeout (a destination stall, `OUTPUT_SINK_TIMEOUT_MS`) and a refused publish among them.
    Error,
    /// The sink before it sent no byte within `output_connect_timeout_ms` of its build.
    ConnectTimeout,
    /// The sink before it sent nothing new for `output_egress_flat_ms` while both encoders delivered
    /// (Owed 94): a hang inside the output pipeline that no sink error reported.
    EgressStalled,
}

/// What the encoders have delivered: per track, the encoded samples pushed into the ring so far (C09
/// counts them for the whole programme, so the totals outlive a sink). Only growth is read: a reading
/// where both grew since the one before shows the encoders delivering (Owed 94).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Encoded {
    pub video: u64,
    pub audio: u64,
}

/// The current sink's highest total and when it last grew: the egress backstop's clock (Owed 94).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Egress {
    high: u64,
    grew_at: Ms,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLink {
    state: OutputState,
    generation: u64,
    /// Why the current generation was built.
    built_for: SwapReason,
    /// What ended the last sink: the reason the next build carries.
    ended: SwapReason,
    /// The current sink's egress since its first growth; `None` until then, and after each build.
    egress: Option<Egress>,
    /// The encoders' totals at the last reading accepted (one from the generation current then). A
    /// build resets neither this nor `delivering_since`: the totals are the programme's and outlive a
    /// sink, so a new sink's first reading compares with its predecessor's last.
    encoded: Option<Encoded>,
    /// Since when every reading has shown both encoders delivering; `None` after one that did not.
    delivering_since: Option<Ms>,
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
            egress: None,
            encoded: None,
            delivering_since: None,
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
        matches!(self.state, OutputState::Connected)
    }

    /// Why the current generation was built (`First` before any build).
    pub fn swap_reason(&self) -> SwapReason {
        self.built_for
    }

    /// The current sink's bytes-sent total, counted from 0 when the sink was built (C09: rtmp2sink
    /// `out-bytes-total`). Only a total above the highest one reported counts as progress: it starts
    /// and proves the sink, and restarts the egress backstop's clock (Owed 94).
    pub fn on_progress(&mut self, now: Ms, gen: u64, total_bytes: u64, t: &Thresholds) {
        if gen != self.generation {
            return;
        }
        let grew = match self.state {
            OutputState::Connecting { .. } | OutputState::Proving { .. } | OutputState::Connected => {
                total_bytes > self.egress.map_or(0, |e| e.high)
            }
            // The sink failed: its late bytes change nothing.
            OutputState::Backoff { .. } => false,
        };
        if grew {
            self.egress = Some(Egress {
                high: total_bytes,
                grew_at: now,
            });
        }
        self.state = match self.state {
            // A new sink counts from 0: its first growth starts the proof, and never connects it.
            OutputState::Connecting { attempt, .. } if total_bytes > 0 => OutputState::Proving {
                attempt,
                first_progress: now,
                sent: total_bytes,
            },
            OutputState::Proving {
                attempt,
                first_progress,
                sent,
            } if total_bytes > sent => {
                if elapsed(now, first_progress) >= t.output_connected_after_ms {
                    OutputState::Connected
                } else {
                    OutputState::Proving {
                        attempt,
                        first_progress,
                        sent: total_bytes,
                    }
                }
            }
            // No growth; a Connected sink; or the sink failed, and its late bytes change nothing (the
            // next build replaces it).
            unchanged => unchanged,
        };
    }

    /// The current sink failed: a bus error, including rtmp2sink's own socket timeout (a connect
    /// that timed out, or a destination that stopped taking bytes).
    pub fn on_error(&mut self, now: Ms, gen: u64, t: &Thresholds) {
        if gen != self.generation {
            return;
        }
        let failed_before = match self.state {
            OutputState::Connected => 0,
            // Not yet Connected: the failures before this sink still count (a refused publish).
            OutputState::Connecting { attempt, .. } | OutputState::Proving { attempt, .. } => attempt,
            // Already failed: a second error from the same sink changes nothing.
            OutputState::Backoff { .. } => return,
        };
        self.back_off(now, failed_before, t, SwapReason::Error);
    }

    /// The encoders' totals, read with the current sink's total (C09's poll). A reading where both
    /// tracks grew since the last one continues (or starts) the encoders' run of delivery; any other
    /// reading ends it, so feed starvation never counts toward the egress backstop (Owed 94).
    pub fn on_delivery(&mut self, now: Ms, gen: u64, encoded: Encoded) {
        if gen != self.generation {
            return;
        }
        let grew = self
            .encoded
            .is_some_and(|e| encoded.video > e.video && encoded.audio > e.audio);
        self.delivering_since = if grew {
            Some(self.delivering_since.unwrap_or(now))
        } else {
            None
        };
        self.encoded = Some(encoded);
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
            // Owed 94: the egress backstop. Not yet Connected, the failures before the sink count.
            OutputState::Proving { attempt, .. } if self.egress_flat(now, t) => {
                self.back_off(now, attempt, t, SwapReason::EgressStalled);
                false
            }
            OutputState::Connected if self.egress_flat(now, t) => {
                self.back_off(now, 0, t, SwapReason::EgressStalled);
                false
            }
            _ => false,
        }
    }

    /// The sink's total has not grown for `output_egress_flat_ms` while the encoders delivered: timed
    /// from the later of its last growth and the start of the current run of delivery.
    fn egress_flat(&self, now: Ms, t: &Thresholds) -> bool {
        match (self.egress, self.delivering_since) {
            (Some(e), Some(d)) => elapsed(now, e.grew_at.max(d)) >= t.output_egress_flat_ms,
            _ => false,
        }
    }

    /// The next sink generation, carrying the failures before it and the reason for it. Its egress
    /// clock starts at its own first growth.
    fn build(&mut self, now: Ms, attempt: u32, reason: SwapReason) -> bool {
        self.generation += 1;
        self.state = OutputState::Connecting { attempt, since: now };
        self.built_for = reason;
        self.egress = None;
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
    use crate::config::{OUTPUT_PROGRESS_POLL_MS, OUTPUT_SINK_TIMEOUT_MS};

    fn t() -> Thresholds {
        Thresholds::default()
    }

    /// Build at `built`, the first bytes at once, and growth to `bytes` (above 1) at the bound: a link
    /// Connected at generation 1. Returns it and the instant it connected.
    fn connected(built: Ms, bytes: u64) -> (OutputLink, Ms) {
        let th = t();
        let mut o = OutputLink::new();
        assert!(o.on_tick(built, &th));
        o.on_progress(built, 1, 1, &th);
        let c = built + th.output_connected_after_ms;
        o.on_progress(c, 1, bytes, &th);
        assert_eq!(o.state(), OutputState::Connected);
        (o, c)
    }

    /// Ticks forward from `now` in 10 ms steps until the next build; returns its time. With no bytes
    /// fed, the next build is due within the connect timeout (Connecting gives up) plus the capped
    /// back-off, so a link that never builds fails here instead of hanging the suite. Proving and
    /// Connected build nothing without an error (Owed 87), so this fails from them too.
    fn next_build(o: &mut OutputLink, mut now: Ms, th: &Thresholds) -> Ms {
        const STEP: Ms = 10;
        let horizon = now + th.output_connect_timeout_ms + th.output_backoff_cap_ms + STEP;
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

    // PDF chaos: the destination accepts but stalls → detected within 5 s. Since Owed 87 the detector
    // is rtmp2sink's socket timeout (C09 sets `OUTPUT_SINK_TIMEOUT_MS`): its error arrives that long
    // after the last write, and the link swaps one back-off base later (failure 1: Connected reset the
    // count). No tick before the error swaps anything.
    #[test]
    fn a_destination_stall_swaps_on_the_sinks_timeout_error() {
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
        let error_at = last + OUTPUT_SINK_TIMEOUT_MS; // rtmp2sink's socket wait gives up
        while now < error_at {
            o.on_progress(now, 1, total, &th);
            assert!(
                !o.on_tick(now, &th),
                "swapped {} ms after the last bytes",
                now - last
            );
            now += 33;
        }
        o.on_error(error_at, 1, &th);
        let at = next_build(&mut o, error_at, &th);
        assert!(
            error_at - last <= 5_000,
            "PDF: detected within 5 s of the last bytes"
        );
        assert_eq!(
            at - last,
            OUTPUT_SINK_TIMEOUT_MS + th.output_backoff_base_ms,
            "swapped one base back-off after the sink's error"
        );
        assert_eq!((o.generation(), o.swap_reason()), (2, SwapReason::Error));
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 1,
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

    // Gen 2 sends its first bytes and goes quiet; gen 1's total keeps growing, past gen 2's bound. Gen
    // 1's bytes are not gen 2's: they neither grow nor connect it.
    #[test]
    fn progress_from_old_generation_does_not_prove_the_new_sink() {
        let th = t();
        let mut o = OutputLink::new();
        o.on_tick(0, &th);
        o.on_progress(0, 1, 4_096, &th);
        o.on_error(10, 1, &th);
        assert!(o.on_tick(600, &th), "the 0.5 s back-off ended at 510");
        o.on_progress(700, 2, 4_096, &th);
        let mut fed = 0;
        for (i, now) in (700..30_000).step_by(100).enumerate() {
            let total = 8_192 + i as u64 * 16_000; // gen 1's total keeps growing
            o.on_progress(now, 1, total, &th);
            fed += 1;
        }
        assert!(
            fed * 100 > th.output_connected_after_ms,
            "{fed} polls reach past the bound"
        );
        assert_eq!(
            o.state(),
            OutputState::Proving {
                attempt: 1,
                first_progress: 700,
                sent: 4_096
            },
            "gen-1 bytes must not prove gen 2"
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

    // An error from Connected: nothing is built before its back-off ends, and the build at its end
    // carries failure 1.
    #[test]
    fn nothing_is_built_before_the_back_off_ends() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        let e = c + 100;
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

    // A second error from the sink a swap replaced: it errors again as it is torn down (old
    // generation, ignored); the new sink's own error backs off as failure 2, since the new sink never
    // earned Connected.
    #[test]
    fn an_error_from_the_torn_down_sink_counts_only_from_the_new_one() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        o.on_error(c + 100, 1, &th);
        let swap = next_build(&mut o, c + 100, &th);
        assert_eq!(o.generation(), 2);
        o.on_error(swap + 10, 1, &th);
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 1,
                since: swap
            }
        );
        o.on_error(swap + 20, 2, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 2,
                until: swap + 20 + 2 * th.output_backoff_base_ms
            }
        );
    }

    // Progress after a swap, from the old generation: the old sink's last poll must not connect the
    // new one, which then times out because nothing did.
    #[test]
    fn old_sink_bytes_after_a_swap_never_connect_the_new_sink() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        o.on_error(c + 100, 1, &th);
        let swap = next_build(&mut o, c + 100, &th);
        o.on_progress(swap + 500, 1, 900_000, &th);
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 1,
                since: swap
            }
        );
        assert!(!o.on_tick(swap + th.output_connect_timeout_ms - 1, &th));
        o.on_tick(swap + th.output_connect_timeout_ms, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 2,
                until: swap + th.output_connect_timeout_ms + 2 * th.output_backoff_base_ms
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

    // stall → recover → stall, each stall ended by the sink's timeout error: the second waits the base
    // again, because the recovery earned Connected (it reset the count). No tick swaps either.
    #[test]
    fn a_second_stall_after_recovery_swaps_again() {
        let th = t();
        let base = th.output_backoff_base_ms;
        let (mut o, c) = connected(0, 1_000);
        let first = c + OUTPUT_SINK_TIMEOUT_MS;
        assert!(!o.on_tick(first, &th), "no swap without the sink's error");
        o.on_error(first, 1, &th);
        let swap = swap_for(&mut o, first, &th, 2, SwapReason::Error);
        assert_eq!(swap, first + base);
        o.on_progress(swap + 300, 2, 4_096, &th);
        let c2 = swap + 300 + th.output_connected_after_ms;
        o.on_progress(c2, 2, 8_192, &th);
        assert!(o.is_connected());
        let second = c2 + OUTPUT_SINK_TIMEOUT_MS;
        assert!(!o.on_tick(second, &th), "no swap without the sink's error");
        o.on_error(second, 2, &th);
        let at = swap_for(&mut o, second, &th, 3, SwapReason::Error);
        assert_eq!(at, second + base, "failure 1 again: the recovery reset the count");
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 1,
                since: at
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
            OutputState::Connected,
            "generation 2's error must not end gen 1"
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
        assert_eq!(o.state(), OutputState::Connected);
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
    // 1 ms earlier does not. Under thresholds where the bound and the connect timeout differ, so a
    // bound that read the other field moves.
    #[test]
    fn the_bound_is_measured_from_the_first_growth_and_is_inclusive() {
        let distinct = Thresholds {
            output_connected_after_ms: 3_100,
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
            assert_eq!(o.state(), OutputState::Connected, "{n}: Connected at the bound");
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
    // proof, whether the change came from a refusal or from a Connected sink's error.
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
        // (3) A Connected gen 1 errors: its late, growing totals never start gen 2's proof.
        let (mut o, last) = connected(0, 1_000);
        o.on_error(last + OUTPUT_SINK_TIMEOUT_MS, 1, &th);
        let swap = next_build(&mut o, last + OUTPUT_SINK_TIMEOUT_MS, &th);
        for k in 1..=40 {
            feed(&mut o, swap + 100 * k, 1, 2_000 * k, &th);
        }
        assert_eq!(
            o.state(),
            OutputState::Connecting {
                attempt: 1,
                since: swap
            },
            "gen 1's bytes after the swap"
        );
    }

    // The guard on the total, while proving: a repeat, a backwards total, and one above the first
    // but below the highest are all no growth. None connects the sink at or past the bound, none moves
    // the highest total, and the next real growth connects it.
    #[test]
    fn a_repeated_or_backwards_total_is_no_progress() {
        let th = t();
        let n = th.output_connected_after_ms;
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        feed(&mut o, 30, 1, 476, &th);
        feed(&mut o, 2_030, 1, 900_000, &th);
        feed(&mut o, 30 + n, 1, 900_000, &th); // a repeat, at the bound
        feed(&mut o, 30 + n + 1, 1, 899_999, &th); // backwards
        feed(&mut o, 30 + n + 2, 1, 500_000, &th); // above the first total, below the highest
        assert_eq!(
            o.state(),
            OutputState::Proving {
                attempt: 0,
                first_progress: 30,
                sent: 900_000
            },
            "no growth past the bound"
        );
        feed(&mut o, 30 + n + 3, 1, 900_001, &th);
        assert_eq!(o.state(), OutputState::Connected, "the next real growth connects");
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
    // own error (while connecting, while proving and once Connected) and the connect timeout. An end
    // from another generation, or a second one from a sink that already failed, names nothing.
    #[test]
    fn each_build_names_what_ended_the_sink_before_it() {
        let th = t();
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

        // Generation 3's bytes stop while it proves, and it errors: the latest end names the build.
        feed(&mut o, now + 31, 3, 476, &th);
        let last = now + 1_000;
        feed(&mut o, last, 3, 52_000, &th);
        o.on_error(last + OUTPUT_SINK_TIMEOUT_MS, 3, &th);
        now = swap_for(&mut o, last + OUTPUT_SINK_TIMEOUT_MS, &th, 4, SwapReason::Error);
        checked += 1;

        // Generation 4 earns Connected, then errors.
        feed(&mut o, now + 31, 4, 476, &th);
        let c = now + 31 + th.output_connected_after_ms;
        feed(&mut o, c, 4, 900_000, &th);
        assert!(o.is_connected());
        o.on_error(c + OUTPUT_SINK_TIMEOUT_MS, 4, &th);
        now = swap_for(&mut o, c + OUTPUT_SINK_TIMEOUT_MS, &th, 5, SwapReason::Error);
        checked += 1;

        // A late error from generation 4 changes nothing; generation 5 times out.
        o.on_error(now + 10, 4, &th);
        assert!(matches!(o.state(), OutputState::Connecting { .. }));
        now = swap_for(&mut o, now + 10, &th, 6, SwapReason::ConnectTimeout);
        checked += 1;

        // Generation 6 times out, and its error then arrives in the back-off: still the timeout.
        let timed_out = now + th.output_connect_timeout_ms;
        assert!(!o.on_tick(timed_out, &th));
        assert!(matches!(o.state(), OutputState::Backoff { .. }));
        o.on_error(timed_out + 1, 6, &th);
        swap_for(&mut o, timed_out + 1, &th, 7, SwapReason::ConnectTimeout);
        checked += 1;

        assert_eq!(checked, 7, "builds checked");
    }

    // ---- Owed 87 (the owner, 2026-10-24, "A"): the link has no stall rule while the encoders starve.
    // With both delivering, a flat sink is Owed 94's backstop's (below). ----

    /// Far past every timer the link has: the connect timeout, the bound and the capped back-off,
    /// ten times over. A link that swaps or fails a sink for silence inside it while the encoders
    /// starve has a stall rule.
    fn silence(th: &Thresholds) -> Ms {
        10 * (th.output_connect_timeout_ms + th.output_connected_after_ms + th.output_backoff_cap_ms)
    }

    // The empty case: a Connected sink whose total never grows again while the encoders starve (the
    // poll's readings repeat their totals too: an encoder hang, Owed 86's shape) is never swapped by
    // the link, on any tick, however long it is silent. The sink's error is what swaps it: failure 1,
    // since Connected reset the count, then the back-off's build, named `Error`. The readings are the
    // production shape (C09's poll carries both totals); with the encoders delivering instead, the
    // backstop ends the sink (`a_hung_output_is_swapped_by_the_egress_backstop_while_the_encoders_deliver`).
    #[test]
    fn a_connected_sink_that_sends_nothing_while_the_encoders_starve_is_swapped_only_by_its_error() {
        let th = t();
        let (mut o, c) = connected(0, 1_000);
        o.on_delivery(c, 1, delivered(1));
        o.on_delivery(c, 1, delivered(2)); // the encoders delivered up to the silence
        let quiet = silence(&th);
        let mut ticks = 0;
        for now in (c..=c + quiet).step_by(33) {
            o.on_progress(now, 1, 1_000, &th); // the poll repeats the frozen total
            o.on_delivery(now, 1, delivered(2)); // and the encoders' totals: starved
            assert!(!o.on_tick(now, &th), "swapped {} ms into the silence", now - c);
            ticks += 1;
        }
        assert!(ticks > 1_000, "{ticks} ticks");
        assert!(o.is_connected(), "{:?}", o.state());
        assert_eq!(o.generation(), 1);
        let e = c + quiet + 1;
        o.on_error(e, 1, &th);
        assert!(
            matches!(
                o.state(),
                OutputState::Backoff { attempt: 1, until } if until == e + th.output_backoff_base_ms
            ),
            "{:?}",
            o.state()
        );
        assert!(!o.on_tick(e + th.output_backoff_base_ms - 1, &th));
        assert!(o.on_tick(e + th.output_backoff_base_ms, &th));
        assert_eq!((o.generation(), o.swap_reason()), (2, SwapReason::Error));
    }

    // A sink that goes silent while it proves, the encoders starved, is not failed by the link
    // either: it stays Proving, carrying the failures before it, until its own error ends it as one
    // more failure. (With them delivering, `a_proving_sink_that_stops_growing_is_swapped_too_and_carries_its_failures`.)
    #[test]
    fn a_proving_sink_that_goes_silent_while_the_encoders_starve_waits_for_its_error() {
        let th = t();
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        o.on_error(10, 1, &th);
        let built = next_build(&mut o, 10, &th);
        o.on_progress(built + 31, 2, 476, &th);
        o.on_delivery(built + 31, 2, delivered(1));
        let last = built + 1_000;
        o.on_progress(last, 2, 52_000, &th); // then nothing, short of the bound
        o.on_delivery(last, 2, delivered(2));
        let quiet = silence(&th);
        let mut ticks = 0;
        for now in (last..=last + quiet).step_by(33) {
            o.on_progress(now, 2, 52_000, &th);
            o.on_delivery(now, 2, delivered(2)); // starved: the totals repeat
            assert!(!o.on_tick(now, &th), "built {} ms into the silence", now - last);
            assert!(
                matches!(o.state(), OutputState::Proving { attempt: 1, .. }),
                "{} ms into the silence: {:?}",
                now - last,
                o.state()
            );
            ticks += 1;
        }
        assert!(ticks > 1_000, "{ticks} ticks");
        let e = last + quiet + 1;
        o.on_error(e, 2, &th);
        let wait = 2 * th.output_backoff_base_ms; // failure 2 waits twice the base
        assert!(
            matches!(o.state(), OutputState::Backoff { attempt: 2, until } if until == e + wait),
            "{:?}",
            o.state()
        );
        assert_eq!(swap_for(&mut o, e, &th, 3, SwapReason::Error), e + wait);
    }

    // The PR #73 review's m-2: with no stall rule, a silence inside the proof is no failure and does
    // not restart it. A proving sink silent for longer than the old stall threshold (one sink timeout
    // and more) that grows again connects at its first growth at or past the bound, counted from its
    // FIRST growth; a growth that resumes short of the bound keeps it proving.
    #[test]
    fn a_proving_sink_that_grows_again_after_a_silence_connects_at_the_bound() {
        let th = t();
        let mut checked = 0;
        for quiet in [OUTPUT_SINK_TIMEOUT_MS + 1, 2 * th.output_connected_after_ms] {
            let mut o = OutputLink::new();
            assert!(o.on_tick(0, &th));
            let first = 31;
            o.on_progress(first, 1, 476, &th);
            let last = first + 100;
            o.on_progress(last, 1, 8_192, &th);
            for now in (last..=last + quiet).step_by(33) {
                o.on_progress(now, 1, 8_192, &th);
                assert!(!o.on_tick(now, &th), "built {} ms into the silence", now - last);
            }
            let back = last + quiet + 1;
            assert!(
                back - first >= th.output_connected_after_ms,
                "setup: past the bound"
            );
            o.on_progress(back, 1, 16_384, &th);
            assert_eq!(o.state(), OutputState::Connected, "quiet {quiet} ms");
            assert_eq!(o.generation(), 1, "no build: a silence is not a failure");
            checked += 1;
        }
        // Short of the bound: the growth that resumes keeps the proof running from its first growth.
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        o.on_progress(31, 1, 476, &th);
        let back = 31 + th.output_connected_after_ms - 1;
        o.on_progress(back, 1, 8_192, &th);
        assert_eq!(
            o.state(),
            OutputState::Proving {
                attempt: 0,
                first_progress: 31,
                sent: 8_192
            }
        );
        o.on_progress(back + 1, 1, 9_000, &th);
        assert_eq!(
            o.state(),
            OutputState::Connected,
            "at the bound from the first growth"
        );
        checked += 1;
        assert_eq!(checked, 3);
    }

    // ---- Owed 94 (the owner, 2026-10-24, "B"): the egress backstop ----

    /// The encoders' totals at reading `k`: both tracks one sample further each reading.
    fn delivered(k: u64) -> Encoded {
        Encoded { video: k, audio: k }
    }

    /// C09's poll from `from` (exclusive) to `to` (inclusive), every `OUTPUT_PROGRESS_POLL_MS`: the
    /// sink's total stays at `total`, the encoders deliver when `delivering` says so for that instant
    /// (else their totals repeat), and every tick in between is offered. Returns the first instant a
    /// tick built, if one did.
    fn poll_flat(
        o: &mut OutputLink,
        (from, to): (Ms, Ms),
        total: u64,
        k: &mut u64,
        delivering: impl Fn(Ms) -> bool,
        th: &Thresholds,
    ) -> Option<Ms> {
        let gen = o.generation();
        let mut now = from;
        while now < to {
            now += 1;
            if (now - from).is_multiple_of(OUTPUT_PROGRESS_POLL_MS) {
                o.on_progress(now, gen, total, th);
                if delivering(now) {
                    *k += 1;
                }
                o.on_delivery(now, gen, delivered(*k));
            }
            if o.on_tick(now, th) {
                return Some(now);
            }
        }
        None
    }

    /// Connected at generation 1 with the encoders delivering on every poll; the total stops growing
    /// at the returned instant (its last growth, on a poll).
    fn connected_delivering(th: &Thresholds) -> (OutputLink, Ms, u64, u64) {
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, th));
        let (mut k, mut total, mut now) = (0u64, 0u64, 0);
        while now < 2 * th.output_connected_after_ms {
            now += OUTPUT_PROGRESS_POLL_MS;
            total += 16_000;
            k += 1;
            o.on_progress(now, 1, total, th);
            o.on_delivery(now, 1, delivered(k));
            assert!(!o.on_tick(now, th), "built while growing at {now}");
        }
        assert!(o.is_connected(), "setup: growth earned Connected");
        (o, now, total, k)
    }

    // Owed 94 (ii): a hang inside the output pipeline. The encoders keep delivering, no sink error
    // comes, and the total stops growing. Nothing swaps before the threshold; the first tick at it
    // backs off as failure 1 (Connected reset the count) and the build names the backstop.
    #[test]
    fn a_hung_output_is_swapped_by_the_egress_backstop_while_the_encoders_deliver() {
        let th = t();
        let flat = th.output_egress_flat_ms;
        let (mut o, last, total, mut k) = connected_delivering(&th);
        let none = poll_flat(&mut o, (last, last + flat - 1), total, &mut k, |_| true, &th);
        assert_eq!(none, None, "swapped before the threshold");
        assert!(o.is_connected(), "still Connected 1 ms before it");
        assert!(
            !o.on_tick(last + flat, &th),
            "the backstop backs off, it does not build at once"
        );
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 1,
                until: last + flat + th.output_backoff_base_ms
            }
        );
        let at = swap_for(&mut o, last + flat, &th, 2, SwapReason::EgressStalled);
        assert_eq!(at, last + flat + th.output_backoff_base_ms);
    }

    // P-2 (the PR #73 review's confirm pass): the backstop's clock runs in Proving too, from the last
    // growth, and the failures before the sink still count (it never earned Connected).
    #[test]
    fn a_proving_sink_that_stops_growing_is_swapped_too_and_carries_its_failures() {
        let th = t();
        let flat = th.output_egress_flat_ms;
        let mut o = OutputLink::new();
        assert!(o.on_tick(0, &th));
        o.on_error(10, 1, &th); // one failure before the sink under test
        let built = next_build(&mut o, 10, &th);
        // The encoders were delivering before the sink's first bytes (two readings: the second grew).
        let mut k = 1;
        o.on_delivery(built, 2, delivered(0));
        o.on_delivery(built + 20, 2, delivered(k));
        o.on_progress(built + 31, 2, 476, &th);
        let last = built + 31;
        assert!(matches!(o.state(), OutputState::Proving { attempt: 1, .. }));
        let none = poll_flat(&mut o, (last, last + flat - 1), 476, &mut k, |_| true, &th);
        assert_eq!(none, None);
        assert!(
            matches!(o.state(), OutputState::Proving { .. }),
            "{:?}",
            o.state()
        );
        o.on_tick(last + flat, &th);
        assert_eq!(
            o.state(),
            OutputState::Backoff {
                attempt: 2,
                until: last + flat + 2 * th.output_backoff_base_ms
            },
            "failure 2 waits the second back-off"
        );
        swap_for(&mut o, last + flat, &th, 3, SwapReason::EgressStalled);
    }

    // Owed 94 (i), the owner's reasoning for "A": a flat total while the encoders deliver nothing is
    // feed starvation, which no swap cures. It never counts: 60 s of it swaps nothing, and once the
    // encoders deliver again the whole threshold runs from then.
    #[test]
    fn the_backstop_never_counts_while_the_encoders_starve() {
        let th = t();
        let flat = th.output_egress_flat_ms;
        let (mut o, last, total, mut k) = connected_delivering(&th);
        let resume = last + 60_000;
        let none = poll_flat(&mut o, (last, resume), total, &mut k, |_| false, &th);
        assert_eq!(none, None, "swapped during starvation");
        assert!(o.is_connected());
        let at = poll_flat(&mut o, (resume, resume + 2 * flat), total, &mut k, |_| true, &th);
        // The first reading after `resume` that shows both encoders grown starts the run, one poll
        // after it; the swap comes a threshold and a base back-off later.
        let run = resume + OUTPUT_PROGRESS_POLL_MS;
        assert_eq!(at, Some(run + flat + th.output_backoff_base_ms));
        assert_eq!((o.generation(), o.swap_reason()), (2, SwapReason::EgressStalled));
    }

    // Starvation part-way through the flat interval restarts the clock: the threshold is counted over
    // one unbroken run of delivery, so no swap lands at `last + flat`.
    #[test]
    fn a_starved_reading_restarts_the_backstops_clock() {
        let th = t();
        let flat = th.output_egress_flat_ms;
        let (mut o, last, total, mut k) = connected_delivering(&th);
        let gap = last + flat / 2;
        let starving = |now: Ms| now != gap;
        let none = poll_flat(&mut o, (last, last + flat), total, &mut k, starving, &th);
        assert_eq!(none, None);
        assert!(
            o.is_connected(),
            "swapped at {} though a reading starved",
            last + flat
        );
        let restart = gap + OUTPUT_PROGRESS_POLL_MS; // the next delivering reading
        let none = poll_flat(
            &mut o,
            (last + flat, restart + flat - 1),
            total,
            &mut k,
            |_| true,
            &th,
        );
        assert_eq!(none, None);
        assert!(o.is_connected(), "swapped before a whole run");
        o.on_tick(restart + flat, &th);
        assert!(
            matches!(o.state(), OutputState::Backoff { attempt: 1, .. }),
            "{:?}",
            o.state()
        );
    }

    // The order the coordinator's question asked for, as a run: a destination stall with the encoders
    // delivering ends by the sink's own error (ruling A's detector), never by the backstop.
    #[test]
    fn a_destination_stall_still_swaps_on_the_sinks_error_with_the_encoders_delivering() {
        let th = t();
        let (mut o, last, total, mut k) = connected_delivering(&th);
        let error_at = last + OUTPUT_SINK_TIMEOUT_MS;
        let none = poll_flat(&mut o, (last, error_at), total, &mut k, |_| true, &th);
        assert_eq!(none, None);
        o.on_error(error_at, 1, &th);
        swap_for(&mut o, error_at, &th, 2, SwapReason::Error);
    }

    // P-6: the clock is time, not a count of readings (a full channel drops them, gst_output.rs). The
    // readings stop entirely after the last delivering one, and the backstop still fires on time.
    #[test]
    fn the_backstop_fires_on_time_when_the_readings_stop() {
        let th = t();
        let flat = th.output_egress_flat_ms;
        let (mut o, last, _, _) = connected_delivering(&th);
        for now in last + 1..last + flat {
            assert!(!o.on_tick(now, &th), "built at {now}");
        }
        assert!(o.is_connected());
        o.on_tick(last + flat, &th);
        assert!(matches!(o.state(), OutputState::Backoff { attempt: 1, .. }));
    }

    // The clock resets at each build: the new sink is judged from its own first growth, and while it
    // connects only the connect timeout ends it. A reading of another generation changes nothing.
    #[test]
    fn the_backstop_restarts_with_each_sink_and_ignores_other_generations() {
        let th = t();
        let flat = th.output_egress_flat_ms;
        let (mut o, last, total, mut k) = connected_delivering(&th);
        poll_flat(&mut o, (last, last + flat), total, &mut k, |_| true, &th);
        let built = swap_for(&mut o, last + flat, &th, 2, SwapReason::EgressStalled);
        // Connecting: the old sink's flat clock does not carry over; the connect timeout runs.
        let none = poll_flat(
            &mut o,
            (built, built + th.output_connect_timeout_ms - 1),
            0,
            &mut k,
            |_| true,
            &th,
        );
        assert_eq!(none, None);
        assert!(
            matches!(o.state(), OutputState::Connecting { .. }),
            "{:?}",
            o.state()
        );
        // Its first bytes start its own clock.
        let first = built + 100;
        let mut o2 = o.clone();
        o2.on_progress(first, 2, 476, &th);
        let none = poll_flat(&mut o2, (first, first + flat - 1), 476, &mut k, |_| true, &th);
        assert_eq!(none, None);
        assert!(
            matches!(o2.state(), OutputState::Proving { .. }),
            "{:?}",
            o2.state()
        );
        // Gen 1's readings (its encoders still counting) neither start nor feed gen 2's clock.
        let mut o3 = o.clone();
        let before = o3.clone();
        o3.on_delivery(built + 50, 1, delivered(k + 100));
        assert_eq!(o3, before, "a stale generation's reading changed the link");
    }
}
