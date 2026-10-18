//! Encoder-overrun degrade ladder (spec §4.3, §6 "Encoder overrun: drop frames before the encoder, then
//! step down the DegradeLadder"). It never stalls the clock: frames are dropped before the encoder,
//! and the adapter reports each drop as one overrun (C09, the pre-encoder queue's `overrun` signal).
//!
//! - **Two rungs, and the bottom holds** (owner ruling 2026-10-18, `docs/_INDEX.md` Owed 20: "no lite
//!   mode; if CPU impacts, measure and raise the CPU size"). `ladder_overruns_to_step` overruns within
//!   `ladder_window_ms` (an overrun exactly the window old still counts, as in `Window`) step down
//!   once, `Full → OverlayHalfRate`, with no wait. Every later burst holds on `OverlayHalfRate`.
//! - **The score is never removed.** The spec's second rung (x264 `veryfast` → `superfast`) cannot
//!   exist: F-001 found x264enc's `speed-preset` cannot change in PLAYING, and the programme pipeline
//!   is never rebuilt. C09's fallback for it, "drop the overlay entirely", is what the ruling refused.
//!   So `FastPreset` is gone, and so is the step gap that only a second step needed.
//! - **No way back up in R2** (C05 brief): the match finishes on the degraded rung, and the heartbeat
//!   reports it (C06 `HealthReport.rung`, `overlay_half_rate` on the wire). In production any step
//!   down is a sizing alarm: web raises it from the heartbeat, and ops raise the Machine size (C17
//!   runbook). C15's soak measures the real overlay cost against the size rule.
use crate::config::Thresholds;
use crate::time::Ms;
use crate::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rung {
    Full,
    /// The bottom: the overlay is refreshed on every other tick (C09), and the score stays on air.
    OverlayHalfRate,
}

impl Rung {
    /// The rung below this one. `OverlayHalfRate` is the bottom and has none (owner ruling
    /// 2026-10-18: no rung takes the score off air), so the ladder holds there.
    fn next(self) -> Option<Rung> {
        match self {
            Rung::Full => Some(Rung::OverlayHalfRate),
            Rung::OverlayHalfRate => None,
        }
    }
}

/// Overrun history bound: the window never holds more than this many stamps (AGENTS.md §4, nothing
/// unbounded). C05 brief default. It must be at least `ladder_overruns_to_step`, or the window could
/// never count enough overruns to step (pinned by `the_overrun_history_bound_leaves_room_for_a_step`).
pub const OVERRUN_HISTORY_CAP: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    rung: Rung,
    recent: Window,
    total: u64,
}

impl Default for Ladder {
    fn default() -> Self {
        Self::new()
    }
}

impl Ladder {
    pub fn new() -> Self {
        Self {
            rung: Rung::Full,
            recent: Window::new(OVERRUN_HISTORY_CAP),
            total: 0,
        }
    }

    pub fn rung(&self) -> Rung {
        self.rung
    }

    /// Every dropped-before-encoder frame this session (the heartbeat's `droppedFrames`, C06/C07).
    pub fn overruns(&self) -> u64 {
        self.total
    }

    /// One frame dropped before the encoder at `now`. `Some(rung)` ⇒ effect `StepDown(rung)`; on the
    /// bottom rung it is always `None`.
    pub fn on_overrun(&mut self, now: Ms, t: &Thresholds) -> Option<Rung> {
        self.total += 1;
        let n = self.recent.record(now, t.ladder_window_ms);
        if n < t.ladder_overruns_to_step {
            return None;
        }
        let next = self.rung.next()?;
        self.rung = next;
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Thresholds {
        Thresholds::default()
    }

    /// `count` overruns spread evenly from `from` to `from + span`, the first and last exactly
    /// `span` apart.
    fn spread(count: usize, from: Ms, span: Ms) -> Vec<Ms> {
        assert!(count >= 2, "a spread needs two ends");
        (0..count)
            .map(|k| from + k as Ms * span / (count as Ms - 1))
            .collect()
    }

    /// Feeds every instant of `at`; returns the action of the last.
    fn feed(l: &mut Ladder, at: &[Ms], th: &Thresholds) -> Option<Rung> {
        at.iter().fold(None, |_, &now| l.on_overrun(now, th))
    }

    /// Feeds every instant of `at`; returns every step it produced, in order.
    fn steps_of(l: &mut Ladder, at: &[Ms], th: &Thresholds) -> Vec<Rung> {
        at.iter().filter_map(|&now| l.on_overrun(now, th)).collect()
    }

    // ---- the brief's 3 (C05 Step 5), as the owner ruling of 2026-10-18 amends them ----

    #[test]
    fn isolated_overruns_never_step() {
        let th = t();
        let mut l = Ladder::new();
        for i in 0..100 {
            assert_eq!(l.on_overrun(i * (th.ladder_window_ms + 1), &th), None);
        }
        assert_eq!(l.rung(), Rung::Full);
        assert_eq!(
            l.overruns(),
            100,
            "every overrun is still counted as a dropped frame"
        );
    }

    // Owner ruling 2026-10-18: the score is never removed, so the one step is to the overlay at
    // half rate, at the `ladder_overruns_to_step`-th overrun, and ~66 s more of constant overrun
    // never steps again.
    #[test]
    fn sustained_overruns_step_once_to_overlay_half_rate_then_stop() {
        let th = t();
        let mut l = Ladder::new();
        let mut steps = Vec::new();
        for i in 0..2_000u64 {
            if let Some(r) = l.on_overrun(i * 33, &th) {
                steps.push((i, r));
            }
        }
        let first = th.ladder_overruns_to_step as u64 - 1;
        assert_eq!(steps, vec![(first, Rung::OverlayHalfRate)]);
        assert_eq!(l.rung(), Rung::OverlayHalfRate);
        assert_eq!(l.overruns(), 2_000);
    }

    #[test]
    fn bottom_rung_holds() {
        let th = t();
        let mut l = Ladder::new();
        let at: Vec<Ms> = (0..10_000u64).map(|i| i * 10).collect();
        assert_eq!(steps_of(&mut l, &at, &th), vec![Rung::OverlayHalfRate]);
        assert_eq!(l.rung(), Rung::OverlayHalfRate);
    }

    // ---- boundaries ----

    // `ladder_overruns_to_step` overruns with the first and last exactly `ladder_window_ms` apart
    // step; 1 ms wider, the first has left the window and nothing steps. One fewer never steps, even
    // in the same millisecond.
    #[test]
    fn the_step_count_inside_exactly_the_window_steps_and_one_ms_wider_does_not() {
        let th = t();
        let (count, span) = (th.ladder_overruns_to_step, th.ladder_window_ms);
        let mut l = Ladder::new();
        assert_eq!(
            feed(&mut l, &spread(count, 0, span), &th),
            Some(Rung::OverlayHalfRate)
        );

        let mut wider = spread(count, 0, span);
        *wider.last_mut().unwrap() += 1;
        let mut l = Ladder::new();
        assert_eq!(feed(&mut l, &wider, &th), None);
        assert_eq!(l.rung(), Rung::Full);

        let mut l = Ladder::new();
        assert_eq!(
            feed(&mut l, &vec![500; count - 1], &th),
            None,
            "{} at once",
            count - 1
        );
        assert_eq!(l.rung(), Rung::Full);
        assert_eq!(l.overruns(), (count - 1) as u64);
    }

    // The one step waits for nothing: the burst's last overrun steps in the same millisecond as
    // its first.
    #[test]
    fn the_step_needs_no_wait() {
        let th = t();
        let mut l = Ladder::new();
        let burst = vec![0; th.ladder_overruns_to_step];
        assert_eq!(feed(&mut l, &burst, &th), Some(Rung::OverlayHalfRate));
    }

    // ---- sequences ----

    // The bottom holds against every later burst, whatever the silence before it: right after the
    // step, one window later, ten windows, a minute's worth, a day. Each burst has the step count
    // and more, so each one alone would have stepped from `Full`.
    #[test]
    fn every_burst_after_the_step_holds_on_overlay_half_rate() {
        let th = t();
        let (need, window) = (th.ladder_overruns_to_step as Ms, th.ladder_window_ms);
        let mut l = Ladder::new();
        let first: Vec<Ms> = (0..need).collect();
        assert_eq!(steps_of(&mut l, &first, &th), vec![Rung::OverlayHalfRate]);
        let mut now = need - 1;
        let mut bursts = 0;
        for silence in [0, 1, window + 1, 10 * window, 60 * window, 86_400_000] {
            for len in [need, need + 1, 2 * need] {
                now += silence;
                let burst: Vec<Ms> = (0..len).map(|k| now + k).collect();
                now += len;
                assert_eq!(
                    steps_of(&mut l, &burst, &th),
                    Vec::<Rung>::new(),
                    "a burst of {len} after {silence} ms"
                );
                assert_eq!(l.rung(), Rung::OverlayHalfRate);
                bursts += 1;
            }
        }
        assert_eq!(bursts, 18, "every silence × burst length");
    }

    // R2 has no way back up (C05 brief): a day of silence, then isolated overruns, leave the degraded
    // rung where it is; a fresh sustained burst steps nowhere, and nothing wraps to `Full`.
    #[test]
    fn the_ladder_never_steps_back_up() {
        let th = t();
        let count = th.ladder_overruns_to_step as Ms;
        let mut l = Ladder::new();
        assert_eq!(
            feed(&mut l, &(0..count).collect::<Vec<_>>(), &th),
            Some(Rung::OverlayHalfRate)
        );
        let day = 86_400_000;
        for k in 0..10 {
            assert_eq!(l.on_overrun(day + k * (th.ladder_window_ms + 1), &th), None);
            assert_eq!(
                l.rung(),
                Rung::OverlayHalfRate,
                "isolated overrun {k} after a quiet day"
            );
        }
        let burst: Vec<Ms> = (0..count).map(|k| 2 * day + k).collect();
        assert_eq!(feed(&mut l, &burst, &th), None);
        assert_eq!(
            l.rung(),
            Rung::OverlayHalfRate,
            "the bottom holds; nothing wraps to Full"
        );
    }

    // Every overrun is a dropped frame, whether it steps, is below the count, or lands on the
    // bottom rung.
    #[test]
    fn every_overrun_counts_as_a_dropped_frame() {
        let th = t();
        let mut l = Ladder::new();
        let mut calls = 0u64;
        let mut steps = 0usize;
        for i in 0..1_000u64 {
            steps += usize::from(l.on_overrun(i * 5, &th).is_some());
            calls += 1;
        }
        assert_eq!(l.overruns(), calls);
        assert_eq!(steps, 1, "5 s of overruns: one step, then the bottom holds");
        for i in 0..1_000u64 {
            steps += usize::from(l.on_overrun(20_000 + i * 5, &th).is_some());
            calls += 1;
        }
        assert_eq!(steps, 1, "a second 5 s of overruns steps nowhere");
        assert_eq!(l.rung(), Rung::OverlayHalfRate);
        assert_eq!(l.overruns(), calls, "counted on the bottom rung too");
    }

    // Every field the ladder reads set apart from the defaults (5 overruns, a 250 ms window), so a
    // field mix-up cannot hide behind equal defaults; each expectation comes from its field.
    #[test]
    fn the_ladder_reads_its_own_thresholds() {
        let th = Thresholds {
            ladder_overruns_to_step: 5,
            ladder_window_ms: 250,
            ..t()
        };
        let mut l = Ladder::new();
        assert_eq!(feed(&mut l, &[0, 60, 120, 180], &th), None, "4 of 5");
        assert_eq!(
            l.on_overrun(250, &th),
            Some(Rung::OverlayHalfRate),
            "5 within 250 ms"
        );
        let mut l2 = Ladder::new();
        assert_eq!(
            feed(&mut l2, &[0, 60, 120, 180, 251], &th),
            None,
            "the first is 251 ms old"
        );
        assert_eq!(
            feed(&mut l, &[3_000, 3_060, 3_120, 3_180, 3_250], &th),
            None,
            "a second full burst, on the bottom"
        );
        assert_eq!(l.rung(), Rung::OverlayHalfRate);
    }

    #[test]
    fn the_overrun_history_bound_leaves_room_for_a_step() {
        let th = t();
        assert!(
            OVERRUN_HISTORY_CAP >= th.ladder_overruns_to_step,
            "a window of {OVERRUN_HISTORY_CAP} can never count {} overruns",
            th.ladder_overruns_to_step
        );
    }
}
