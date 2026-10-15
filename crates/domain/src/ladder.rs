//! Encoder-overrun degrade ladder (spec §4.3, §6 "Encoder overrun: drop frames before the encoder, then
//! step down the DegradeLadder"). It never stalls the clock: frames are dropped before the encoder,
//! and the adapter reports each drop as one overrun (C09, the pre-encoder queue's `overrun` signal).
//!
//! - **Step.** `ladder_overruns_to_step` overruns within `ladder_window_ms` (an overrun exactly the
//!   window old still counts, as in `Window`) step down one rung, `Full → OverlayHalfRate →
//!   FastPreset`, and the bottom rung holds.
//! - **Gap.** At most one step per `ladder_step_min_gap_ms` (PDF failure table: real time restored
//!   within 10 s): the next step may come once that long has passed since the last. The first step
//!   waits for nothing.
//! - **No way back up in R2** (C05 brief): the match finishes on the degraded rung, and the heartbeat
//!   reports it (C06 `HealthReport.rung`). YAGNI until the soak shows a need.
//! - **What `FastPreset` does is C09's.** The spec names an x264 preset step (`veryfast` →
//!   `superfast`), but F-001 found x264enc's `speed-preset` cannot change in PLAYING, and the
//!   programme pipeline is never rebuilt. C09's decision rule then makes this rung "drop the overlay
//!   entirely" (`docs/_INDEX.md`, the C05 false-premise rows). The name stays, because the heartbeat
//!   contract carries it (`fast_preset`, R2002).
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};
use crate::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rung {
    Full,
    OverlayHalfRate,
    FastPreset,
}

impl Rung {
    /// The rung below this one; the bottom has none.
    fn next(self) -> Option<Rung> {
        match self {
            Rung::Full => Some(Rung::OverlayHalfRate),
            Rung::OverlayHalfRate => Some(Rung::FastPreset),
            Rung::FastPreset => None,
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
    last_step_at: Option<Ms>,
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
            last_step_at: None,
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

    /// One frame dropped before the encoder at `now`. `Some(rung)` ⇒ effect `StepDown(rung)`.
    pub fn on_overrun(&mut self, now: Ms, t: &Thresholds) -> Option<Rung> {
        self.total += 1;
        let n = self.recent.record(now, t.ladder_window_ms);
        if n < t.ladder_overruns_to_step {
            return None;
        }
        if self
            .last_step_at
            .is_some_and(|last| elapsed(now, last) < t.ladder_step_min_gap_ms)
        {
            return None;
        }
        let next = self.rung.next()?;
        self.rung = next;
        self.last_step_at = Some(now);
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

    // ---- the brief's 3 (C05 Step 5) ----

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

    #[test]
    fn sustained_overruns_step_overlay_first_then_preset_then_stop() {
        let th = t();
        let mut l = Ladder::new();
        let mut steps = Vec::new();
        for i in 0..2_000u64 {
            // ~66 s of constant overrun
            if let Some(r) = l.on_overrun(i * 33, &th) {
                steps.push((i * 33, r));
            }
        }
        assert_eq!(
            steps.iter().map(|s| s.1).collect::<Vec<_>>(),
            vec![Rung::OverlayHalfRate, Rung::FastPreset]
        );
        assert!(steps[1].0 - steps[0].0 >= th.ladder_step_min_gap_ms);
    }

    #[test]
    fn bottom_rung_holds() {
        let th = t();
        let mut l = Ladder::new();
        for i in 0..10_000u64 {
            l.on_overrun(i * 10, &th);
        }
        assert_eq!(l.rung(), Rung::FastPreset);
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

    // The first step needs no gap (at 2 ms, long before `ladder_step_min_gap_ms`). The next waits for
    // exactly the gap: a full burst ending 1 ms short of it does not step, and the overrun at exactly
    // the gap does.
    #[test]
    fn the_next_step_waits_exactly_the_min_gap() {
        let th = t();
        let count = th.ladder_overruns_to_step as Ms;
        let gap = th.ladder_step_min_gap_ms;
        let mut l = Ladder::new();
        let first: Vec<Ms> = (0..count).collect();
        assert_eq!(feed(&mut l, &first, &th), Some(Rung::OverlayHalfRate));
        let stepped = count - 1;
        let burst: Vec<Ms> = (0..count).map(|k| stepped + gap - count + k).collect();
        assert_eq!(*burst.last().unwrap(), stepped + gap - 1);
        for &now in &burst {
            assert_eq!(
                l.on_overrun(now, &th),
                None,
                "at {now}, {} ms after the step",
                now - stepped
            );
        }
        assert_eq!(l.on_overrun(stepped + gap, &th), Some(Rung::FastPreset));
    }

    // ---- sequences ----

    // R2 has no way back up (C05 brief): a day of silence, then isolated overruns, leave the degraded
    // rung where it is; a fresh sustained burst only steps further down.
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
        assert_eq!(feed(&mut l, &burst, &th), Some(Rung::FastPreset));
        assert_eq!(feed(&mut l, &[3 * day, 3 * day, 3 * day, 3 * day + 1], &th), None);
        assert_eq!(
            l.rung(),
            Rung::FastPreset,
            "the bottom holds; nothing wraps to Full"
        );
    }

    // Every overrun is a dropped frame, whether it steps, is held back by the gap, or lands on the
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
        assert_eq!(steps, 1, "5 s of overruns: one step, then the gap holds the next");
        for i in 0..1_000u64 {
            l.on_overrun(20_000 + i * 5, &th);
            calls += 1;
        }
        assert_eq!(l.rung(), Rung::FastPreset);
        assert_eq!(l.overruns(), calls, "counted on the bottom rung too");
    }

    // Every field the ladder reads set apart from the defaults (5 overruns, a 250 ms window, a 3 s
    // gap), so a field mix-up cannot hide behind equal defaults; each expectation comes from its field.
    #[test]
    fn the_ladder_reads_its_own_thresholds() {
        let th = Thresholds {
            ladder_overruns_to_step: 5,
            ladder_window_ms: 250,
            ladder_step_min_gap_ms: 3_000,
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
            feed(&mut l, &[3_000, 3_060, 3_120, 3_180, 3_249], &th),
            None,
            "2_999 ms after the step"
        );
        assert_eq!(
            l.on_overrun(3_250, &th),
            Some(Rung::FastPreset),
            "3_000 ms after the step"
        );
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

    // The next step's evidence comes after the last step: the window is shorter than the gap, so an
    // overrun from before a step has left the window by the time the next step is allowed.
    #[test]
    fn overruns_before_a_step_never_count_toward_the_next() {
        let th = t();
        assert!(
            th.ladder_window_ms < th.ladder_step_min_gap_ms,
            "window {} ms vs gap {} ms",
            th.ladder_window_ms,
            th.ladder_step_min_gap_ms
        );
    }
}
