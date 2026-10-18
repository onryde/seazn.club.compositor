//! Programme sub-machine: what the encoder gets on every tick. Table-driven, from the PDF's
//! robustness state machine (spec §3.3, §4.2):
//!
//! ```text
//! Waiting --1 s continuous--> Live --0.5 s no frame--> Frozen --3 s no frame--> Reconnecting
//!                              ^ <------frames resume----- |                        |
//!                              +-----------------1 s continuous---------------------+
//! ```
//!
//! The PDF scheduler paragraph's "Repeat up to 1 s" is superseded by this machine (spec §3.3): the
//! last frame repeats from 0.5 s with no frame until the 3 s slate.
//!
//! A `FrameChoice` picks the VIDEO layer only; the overlay is composited over every choice. Audio is
//! not a programme choice: the track runs on in every phase with no PTS gap, real input audio or
//! low-level noise, and never digital silence (F-004; C09's Audio section). Spec §3.3's "audio
//! follows the same choice (… silence …)" is superseded by F-004.
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};
use FrameChoice::*;
use ProgrammePhase::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgrammePhase {
    Waiting,
    Live,
    Frozen,
    Reconnecting,
    /// Overlay templates spec 2026-10-15 §4.2 (D4): the final hold before EOS. Terminal:
    /// `PROGRAMME_TABLE` has no row from it, so no feed leaves it, and only `Programme::finish` enters
    /// it. It wins over every camera state.
    Final,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlateKind {
    Waiting,
    Reconnecting,
    Final,
}

impl SlateKind {
    /// Every kind once. C09 renders the one textless slate for each, and its test enumerates this.
    pub const ALL: [SlateKind; 3] = [SlateKind::Waiting, SlateKind::Reconnecting, SlateKind::Final];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameChoice {
    Emit,
    Repeat,
    Slate(SlateKind),
}

/// The input as seen at one tick, derived from frame arrival times. The age is the time since the
/// latest frame. The run is the unbroken run of frames (no gap between two of them longer than
/// `frozen_after_ms`), and its length is the span from its first frame to its latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feed {
    /// Age ≤ frozen_after AND the run spans ≥ resume_after (PDF: "1 s of continuous frames").
    Continuous,
    /// Age ≤ frozen_after, and the run spans less than resume_after.
    Fresh,
    /// frozen_after < age < slate_after.
    Gap,
    /// age ≥ slate_after, or no frame ever.
    LongGap,
}

/// (from, feed) → to. Any pair not listed keeps its phase, so a never-live input stays on the
/// waiting slate whatever it does.
pub const PROGRAMME_TABLE: &[(ProgrammePhase, Feed, ProgrammePhase)] = &[
    (Waiting, Feed::Continuous, Live),
    (Live, Feed::Gap, Frozen),
    (Live, Feed::LongGap, Reconnecting), // a stalled tick thread can skip Frozen entirely
    (Frozen, Feed::Fresh, Live),
    (Frozen, Feed::Continuous, Live),
    (Frozen, Feed::LongGap, Reconnecting),
    (Reconnecting, Feed::Continuous, Live),
];

pub fn next_phase(phase: ProgrammePhase, feed: Feed) -> ProgrammePhase {
    PROGRAMME_TABLE
        .iter()
        .find(|&&(f, s, _)| f == phase && s == feed)
        .map_or(phase, |&(_, _, to)| to)
}

pub fn choice_for(phase: ProgrammePhase) -> FrameChoice {
    match phase {
        Waiting => Slate(SlateKind::Waiting),
        Live => Emit,
        Frozen => Repeat,
        Reconnecting => Slate(SlateKind::Reconnecting),
        // Spec §4.2's Final backdrop is opaque, so the camera is not shown. With graphics up the
        // page's Final scene covers the slate; with graphics failed, C09's textless slate shows.
        Final => Slate(SlateKind::Final),
    }
}

/// The unbroken run of camera frames: its first frame and its latest. `since <= last` always: a run
/// starts with both at one frame, and `last` only grows. `feed` still reads the span through
/// `elapsed`, so a broken invariant would read as a short run (the slate holds), never a wrapped one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    since: Ms,
    last: Ms,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programme {
    phase: ProgrammePhase,
    /// `None` until the first frame. One value, so a latest frame without a run cannot be built.
    run: Option<Run>,
}

impl Default for Programme {
    fn default() -> Self {
        Self {
            phase: Waiting,
            run: None,
        }
    }
}

impl Programme {
    pub fn phase(&self) -> ProgrammePhase {
        self.phase
    }

    /// The latest camera frame. C06's input stall rule reads it.
    pub fn last_frame_at(&self) -> Option<Ms> {
        self.run.map(|r| r.last)
    }

    /// A camera frame at `now`. A gap of more than `frozen_after_ms` since the latest frame breaks
    /// the run, and this frame starts a new one. A frame stamped at or before the latest one changes
    /// nothing (its age from the latest is 0, and the latest stays the max), so the latest frame
    /// never moves backwards.
    pub fn on_frame(&mut self, now: Ms, t: &Thresholds) {
        self.run = Some(match self.run {
            Some(r) if elapsed(now, r.last) <= t.frozen_after_ms => Run {
                since: r.since,
                last: r.last.max(now),
            },
            _ => Run {
                since: now,
                last: now,
            },
        });
    }

    pub fn feed(&self, now: Ms, t: &Thresholds) -> Feed {
        let Some(r) = self.run else { return Feed::LongGap };
        let age = elapsed(now, r.last);
        if age >= t.slate_after_ms {
            return Feed::LongGap;
        }
        if age > t.frozen_after_ms {
            return Feed::Gap;
        }
        if elapsed(r.last, r.since) >= t.resume_after_ms {
            Feed::Continuous
        } else {
            Feed::Fresh
        }
    }

    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> FrameChoice {
        self.phase = next_phase(self.phase, self.feed(now, t));
        choice_for(self.phase)
    }

    /// Enter the final hold (C06). Idempotent. Later frames and ticks keep it Final: no table row
    /// leaves it.
    pub fn finish(&mut self) {
        self.phase = Final;
    }
}

// Table sweeps that print their counts, and the reference-model and budget sweeps, are in
// `tests/programme.rs`: the crate root forbids `clippy::print_stderr`, inline tests included.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutputProfile;

    fn t() -> Thresholds {
        Thresholds::default()
    }

    fn period() -> Ms {
        OutputProfile::P720.frame_period_ms().round() as Ms // 33
    }

    /// Frames arrive at every period inside any of `frames_in` ranges; a tick runs every period.
    /// Returns (time, choice) per tick from 0 to `until`.
    fn drive(frames_in: &[(Ms, Ms)], until: Ms) -> Vec<(Ms, FrameChoice)> {
        let (th, p) = (t(), period());
        let mut m = Programme::default();
        let mut out = Vec::new();
        let mut now = 0;
        while now <= until {
            if frames_in.iter().any(|&(a, b)| (a..b).contains(&now)) {
                m.on_frame(now, &th);
            }
            out.push((now, m.on_tick(now, &th)));
            now += p;
        }
        out
    }

    fn first(out: &[(Ms, FrameChoice)], from: Ms, want: FrameChoice) -> Ms {
        out.iter()
            .find(|&&(at, c)| at >= from && c == want)
            .map(|&(at, _)| at)
            .expect("choice never reached")
    }

    /// A frame and then a tick at every `step` from `from` to `to`, both included. Returns the choice
    /// of the last tick.
    fn frames(m: &mut Programme, from: Ms, to: Ms, step: Ms, th: &Thresholds) -> FrameChoice {
        let (mut now, mut choice) = (from, None);
        while now <= to {
            m.on_frame(now, th);
            choice = Some(m.on_tick(now, th));
            now += step;
        }
        choice.expect("frames: an empty range")
    }

    /// Live, with frames every 10 ms from 0 to exactly `resume_after_ms`, the latest at that instant.
    fn live(th: &Thresholds) -> Programme {
        let mut m = Programme::default();
        assert_eq!(frames(&mut m, 0, th.resume_after_ms, 10, th), Emit, "setup: live");
        m
    }

    /// Reconnecting: live as above, then `slate_after_ms` with no frame. The latest frame stays at
    /// `resume_after_ms`.
    fn reconnecting(th: &Thresholds) -> Programme {
        let mut m = live(th);
        let lost = th.resume_after_ms + th.slate_after_ms;
        assert_eq!(
            m.on_tick(lost, th),
            Slate(SlateKind::Reconnecting),
            "setup: slate"
        );
        m
    }

    // ---- the choice per phase (PDF robustness diagram; spec §3.3) ----
    #[test]
    fn choice_per_phase() {
        assert_eq!(choice_for(Waiting), Slate(SlateKind::Waiting));
        assert_eq!(choice_for(Live), Emit);
        assert_eq!(choice_for(Frozen), Repeat);
        assert_eq!(choice_for(Reconnecting), Slate(SlateKind::Reconnecting));
        assert_eq!(choice_for(Final), Slate(SlateKind::Final));
    }

    // ---- behaviour against the PDF timing budgets ----
    #[test]
    fn goes_live_after_one_second_of_frames_not_before() {
        let out = drive(&[(0, 10_000)], 2_000);
        let live = first(&out, 0, Emit);
        assert!(live >= t().resume_after_ms, "live at {live}");
        assert!(live < t().resume_after_ms + 2 * period());
    }

    #[test]
    fn no_frames_ever_shows_waiting_slate_forever() {
        let out = drive(&[], 10_000);
        assert_eq!(
            out.len() as Ms,
            10_000 / period() + 1,
            "one tick per period from 0"
        );
        assert!(
            out.iter().all(|&(_, c)| c == Slate(SlateKind::Waiting)),
            "{} ticks",
            out.len()
        );
    }

    #[test]
    fn freezes_after_half_second_then_slates_within_3_1_s_budget() {
        let out = drive(&[(0, 5_000)], 10_000);
        let last_frame = out
            .iter()
            .rev()
            .find(|&&(at, _)| at < 5_000)
            .map(|&(at, _)| at)
            .unwrap();
        let frozen = first(&out, 5_000, Repeat);
        let slate = first(&out, 5_000, Slate(SlateKind::Reconnecting));
        assert!(
            frozen - last_frame > t().frozen_after_ms,
            "froze too early at +{}",
            frozen - last_frame
        );
        assert!(
            slate - last_frame <= 3_100,
            "PDF budget: slate ≤ 3.1 s, got +{}",
            slate - last_frame
        );
    }

    #[test]
    fn frozen_returns_to_live_as_soon_as_frames_resume() {
        // 1 s outage: never reaches slate, resumes immediately
        let out = drive(&[(0, 3_000), (4_000, 9_000)], 6_000);
        let back = first(&out, 4_000, Emit);
        assert!(back - 4_000 <= period(), "resume took {}", back - 4_000);
        assert!(out.iter().all(|&(_, c)| c != Slate(SlateKind::Reconnecting)));
    }

    #[test]
    fn reconnecting_needs_one_second_continuous_and_lands_inside_1_2_s_budget() {
        let out = drive(&[(0, 2_000), (8_000, 12_000)], 12_000);
        let back = first(&out, 8_000, Emit);
        // The first returned frame is the first grid instant at or after 8 000.
        let returned = 8_000_u64.div_ceil(period()) * period();
        assert!(
            back - returned >= t().resume_after_ms,
            "live {} ms after the return",
            back - returned
        );
        assert!(
            back - 8_000 <= 1_200,
            "PDF budget: live ≤ 1.2 s after camera returns, got {}",
            back - 8_000
        );
    }

    #[test]
    fn hiccup_shorter_than_frozen_threshold_does_not_restart_the_run() {
        // 400 ms gap inside the first second: the run since t=0 is unbroken by the 0.5 s rule
        let out = drive(&[(0, 300), (700, 5_000)], 2_000);
        let live = first(&out, 0, Emit);
        assert!(
            live < 1_100,
            "went live at {live}; the 400 ms hiccup must not reset the run"
        );
    }

    // The differential pair of the test above: a 600 ms hiccup (> frozen_after) breaks the run, so
    // live comes a full resume_after after the first frame past the hiccup, not 1 s after t=0.
    #[test]
    fn hiccup_longer_than_frozen_threshold_restarts_the_run() {
        let out = drive(&[(0, 300), (900, 5_000)], 3_000);
        let resumed = 900_u64.div_ceil(period()) * period();
        let live = first(&out, 0, Emit);
        assert!(
            live >= resumed + t().resume_after_ms,
            "went live at {live}; the run must restart at {resumed}"
        );
        assert!(
            live < resumed + t().resume_after_ms + 2 * period(),
            "live at {live}"
        );
    }

    #[test]
    fn second_outage_behaves_like_the_first() {
        let out = drive(&[(0, 2_000), (8_000, 12_000), (18_000, 22_000)], 22_000);
        let a = first(&out, 8_000, Emit) - 8_000;
        let b = first(&out, 18_000, Emit) - 18_000;
        // `drive` walks one global 33 ms grid, so the two outages start at different grid phases
        // (8_019 vs 18_018): equal behaviour shows up as a difference under one frame period.
        assert!(
            a.abs_diff(b) < period(),
            "second outage recovered in {b} ms vs {a} ms"
        );
    }

    // A second loss after a recovery walks the same phases as the first, in the same order.
    #[test]
    fn two_outages_walk_the_same_phase_sequence() {
        let out = drive(&[(0, 2_000), (8_000, 12_000), (18_000, 22_000)], 22_000);
        let mut walk: Vec<FrameChoice> = Vec::new();
        for &(_, c) in &out {
            if walk.last() != Some(&c) {
                walk.push(c);
            }
        }
        let recon = Slate(SlateKind::Reconnecting);
        assert_eq!(
            walk,
            vec![
                Slate(SlateKind::Waiting),
                Emit,
                Repeat,
                recon,
                Emit,
                Repeat,
                recon,
                Emit
            ],
            "over {} ticks",
            out.len()
        );
    }

    #[test]
    fn stalled_tick_thread_skips_frozen_straight_to_slate() {
        let (th, mut m) = (t(), Programme::default());
        for now in (0..2_000).step_by(33) {
            m.on_frame(now, &th);
            m.on_tick(now, &th);
        }
        assert_eq!(m.phase(), Live);
        assert_eq!(m.on_tick(2_000 + 4_000, &th), Slate(SlateKind::Reconnecting));
    }

    // ---- the empty case and the initial state ----
    #[test]
    fn initial_state_is_waiting_with_no_frame() {
        let (th, mut m) = (t(), Programme::default());
        assert_eq!(m.phase(), Waiting);
        assert_eq!(m.last_frame_at(), None);
        assert_eq!(m.feed(0, &th), Feed::LongGap, "no frame ever, at t=0");
        assert_eq!(
            m.feed(Ms::MAX, &th),
            Feed::LongGap,
            "no frame ever, at the end of time"
        );
        assert_eq!(m.on_tick(0, &th), Slate(SlateKind::Waiting));
        assert_eq!(m.on_tick(Ms::MAX, &th), Slate(SlateKind::Waiting));
        assert_eq!(m.phase(), Waiting);
    }

    #[test]
    fn last_frame_at_reports_the_latest_frame() {
        let (th, mut m) = (t(), Programme::default());
        m.on_frame(40, &th);
        assert_eq!(m.last_frame_at(), Some(40));
        m.on_frame(73, &th);
        assert_eq!(m.last_frame_at(), Some(73));
        m.on_frame(9_000, &th);
        assert_eq!(m.last_frame_at(), Some(9_000), "after a gap that broke the run");
    }

    // A never-live input that stops is still on the WAITING slate, never "reconnecting".
    #[test]
    fn never_live_input_that_stops_stays_on_the_waiting_slate() {
        let out = drive(&[(0, 800)], 10_000);
        assert!(out.len() > 300, "{} ticks", out.len());
        assert!(out.iter().all(|&(_, c)| c == Slate(SlateKind::Waiting)));
    }

    // ---- exact threshold boundaries, ±1 ms (Thresholds::default(): 500 / 3 000 / 1 000) ----
    #[test]
    fn live_holds_at_exactly_frozen_after_and_freezes_one_ms_later() {
        let th = t();
        let last = th.resume_after_ms;
        let mut m = live(&th);
        assert_eq!(
            m.on_tick(last + th.frozen_after_ms, &th),
            Emit,
            "a frame exactly 0.5 s old"
        );
        assert_eq!(
            m.on_tick(last + th.frozen_after_ms + 1, &th),
            Repeat,
            "0.5 s + 1 ms"
        );
    }

    #[test]
    fn frozen_holds_one_ms_before_slate_after_and_slates_at_it() {
        let th = t();
        let last = th.resume_after_ms;
        let mut m = live(&th);
        assert_eq!(m.on_tick(last + th.frozen_after_ms + 1, &th), Repeat);
        assert_eq!(m.on_tick(last + th.slate_after_ms - 1, &th), Repeat, "3 s − 1 ms");
        assert_eq!(
            m.on_tick(last + th.slate_after_ms, &th),
            Slate(SlateKind::Reconnecting)
        );
    }

    #[test]
    fn a_stalled_live_tick_at_exactly_slate_after_slates_and_one_ms_before_freezes() {
        let th = t();
        let last = th.resume_after_ms;
        let mut early = live(&th);
        assert_eq!(early.on_tick(last + th.slate_after_ms - 1, &th), Repeat);
        let mut stalled = live(&th);
        assert_eq!(
            stalled.on_tick(last + th.slate_after_ms, &th),
            Slate(SlateKind::Reconnecting)
        );
    }

    #[test]
    fn waiting_goes_live_when_the_frames_span_exactly_resume_after() {
        let th = t();
        let mut m = Programme::default();
        let r = th.resume_after_ms;
        assert_eq!(frames(&mut m, 0, r - 10, 10, &th), Slate(SlateKind::Waiting));
        m.on_frame(r - 1, &th);
        assert_eq!(
            m.on_tick(r - 1, &th),
            Slate(SlateKind::Waiting),
            "frames span 1 s − 1 ms"
        );
        m.on_frame(r, &th);
        assert_eq!(m.on_tick(r, &th), Emit, "frames span exactly 1 s");
    }

    #[test]
    fn reconnecting_goes_live_when_the_returned_frames_span_exactly_resume_after() {
        let th = t();
        let mut m = reconnecting(&th);
        let back = 20_000;
        let r = th.resume_after_ms;
        assert_eq!(
            frames(&mut m, back, back + r - 10, 10, &th),
            Slate(SlateKind::Reconnecting)
        );
        m.on_frame(back + r - 1, &th);
        assert_eq!(
            m.on_tick(back + r - 1, &th),
            Slate(SlateKind::Reconnecting),
            "1 s − 1 ms"
        );
        m.on_frame(back + r, &th);
        assert_eq!(m.on_tick(back + r, &th), Emit, "exactly 1 s");
    }

    // Differential: "1 s of continuous frames" is the span of the frames received, not the time
    // since the run began. At `resume_after_ms` after the first frame, with the latest frame 1 ms
    // earlier, the frames span 999 ms: still waiting. Measuring from the first frame to `now` would
    // go live here.
    #[test]
    fn a_tick_one_second_after_the_first_frame_is_not_a_second_of_frames() {
        let th = t();
        let mut m = Programme::default();
        let r = th.resume_after_ms;
        assert_eq!(frames(&mut m, 0, r - 10, 10, &th), Slate(SlateKind::Waiting));
        m.on_frame(r - 1, &th);
        assert_eq!(m.feed(r, &th), Feed::Fresh, "frames span 999 ms");
        assert_eq!(m.on_tick(r, &th), Slate(SlateKind::Waiting));
    }

    // Differential, the visible case: the camera comes back for 0.6 s and stops again. 1 s after
    // its return the newest frame is 0.4 s old (inside frozen_after), but only 0.6 s of frames came.
    // The slate stays up; counting the time since the return would flash a stale frame as "live".
    #[test]
    fn a_short_return_then_silence_is_not_a_second_of_frames() {
        let th = t();
        let mut m = reconnecting(&th);
        let back = 20_000;
        let r = th.resume_after_ms;
        assert_eq!(
            frames(&mut m, back, back + 600, 10, &th),
            Slate(SlateKind::Reconnecting)
        );
        assert_eq!(m.feed(back + r, &th), Feed::Fresh, "the frames span 600 ms");
        assert_eq!(m.on_tick(back + r, &th), Slate(SlateKind::Reconnecting));
    }

    // Mutant 3 of the brief: a frame exactly `frozen_after_ms` after the previous one does NOT
    // break the run. Here the run since t=0 reaches 1 s at t=1 000 and goes live.
    #[test]
    fn a_frame_exactly_frozen_after_later_keeps_the_run() {
        let th = t();
        let mut m = Programme::default();
        assert_eq!(frames(&mut m, 0, 300, 10, &th), Slate(SlateKind::Waiting));
        let resumed = 300 + th.frozen_after_ms;
        assert_eq!(
            frames(&mut m, resumed, th.resume_after_ms, 10, &th),
            Emit,
            "run since 0"
        );
    }

    // ... and its pair: 1 ms more breaks it. The run restarts at 801, so at 1 000 it is still waiting
    // and it goes live at 1 801.
    #[test]
    fn a_frame_one_ms_past_frozen_after_restarts_the_run() {
        let th = t();
        let mut m = Programme::default();
        frames(&mut m, 0, 300, 10, &th);
        let resumed = 300 + th.frozen_after_ms + 1;
        let r = th.resume_after_ms;
        let w = Slate(SlateKind::Waiting);
        assert_eq!(
            frames(&mut m, resumed, r, 1, &th),
            w,
            "run restarted at {resumed}"
        );
        assert_eq!(
            frames(&mut m, r + 1, resumed + r - 1, 1, &th),
            w,
            "1 ms short of 1 s"
        );
        assert_eq!(frames(&mut m, resumed + r, resumed + r, 1, &th), Emit);
    }

    // ---- a second call: C08's `tick_due` runs every due tick at one `now` ----
    #[test]
    fn repeated_frames_and_ticks_at_one_instant_change_nothing() {
        let th = t();
        let mut m = live(&th);
        let at = th.resume_after_ms + th.frozen_after_ms + 1;
        assert_eq!(m.on_tick(at, &th), Repeat, "the transition tick: Live → Frozen");
        let once = m.clone();
        assert_eq!(m.on_tick(at, &th), Repeat, "a second tick at the same instant");
        assert_eq!(m, once, "the second tick changed the state");
        m.on_frame(at, &th);
        let framed = m.clone();
        m.on_frame(at, &th);
        assert_eq!(m, framed, "a second frame at the same instant changed the state");
        assert_eq!(m.on_tick(at, &th), Emit, "frames resumed: back to live");
    }

    // ---- frame stamps out of time order: the latest frame never moves backwards ----
    // C06 reads `last_frame_at` for the input stall rule, so a regressed latest frame would read as a
    // stall. Without the guard, a frame stamped at 400 after one at 1 000 makes the programme freeze
    // 0.5 s after 400 instead of 0.5 s after 1 000.
    #[test]
    fn a_frame_stamped_before_the_latest_changes_nothing() {
        let th = t();
        let mut m = live(&th);
        let before = m.clone();
        m.on_frame(400, &th);
        assert_eq!(m, before, "a late stamp moved the run");
        assert_eq!(m.last_frame_at(), Some(th.resume_after_ms));
        assert_eq!(m.on_tick(th.resume_after_ms + th.frozen_after_ms, &th), Emit);
    }

    // A frame delivered late (stamped before a tick that already ran) meets a Frozen programme with
    // its run unbroken: frames resumed, so it goes live. Row (Frozen, Continuous) is reachable only
    // this way, since a frame stamped after a tick that saw a Gap always restarts the run.
    #[test]
    fn a_late_delivered_frame_while_frozen_goes_live_on_an_unbroken_run() {
        let th = t();
        let mut m = live(&th);
        let last = th.resume_after_ms;
        let now = last + th.frozen_after_ms + 100;
        assert_eq!(m.on_tick(now, &th), Repeat);
        m.on_frame(last + 400, &th);
        assert_eq!(
            m.feed(now, &th),
            Feed::Continuous,
            "the run 0..=1 400 spans 1.4 s"
        );
        assert_eq!(m.on_tick(now, &th), Emit);
    }

    // A tick stamped BEFORE the latest frame (a clock that stepped back; C08/C12 owe a monotonic
    // `now`, docs/_INDEX.md): the frame's age reads 0, never a wrapped age, and the run is still the
    // span of the frames received (1 s here), not the 995 ms from its first frame to `now`.
    #[test]
    fn a_tick_before_the_latest_frame_reads_age_zero_and_the_whole_run() {
        let th = t();
        let mut m = Programme::default();
        let r = th.resume_after_ms;
        assert_eq!(frames(&mut m, 0, r - 10, 10, &th), Slate(SlateKind::Waiting));
        m.on_frame(r, &th);
        let early = r - 5;
        assert_eq!(m.feed(early, &th), Feed::Continuous, "age 0, frames span 1 s");
        assert_eq!(m.feed(0, &th), Feed::Continuous, "a tick before the run began");
        assert_eq!(m.on_tick(early, &th), Emit);
        assert_eq!(m.last_frame_at(), Some(r), "the tick moved the latest frame");
    }

    // ---- loss while already lost ----
    // Frozen, then one frame: back to Live at once (PDF "frames resume"), and the 3 s slate clock
    // restarts from that frame. A slate at "3 s after the first loss" would be wrong here.
    #[test]
    fn a_single_frame_while_frozen_goes_live_and_restarts_the_slate_clock() {
        let th = t();
        let mut m = live(&th);
        let flicker = th.resume_after_ms + 1_000;
        assert_eq!(m.on_tick(flicker, &th), Repeat);
        m.on_frame(flicker, &th);
        assert_eq!(m.on_tick(flicker, &th), Emit, "frames resumed");
        assert_eq!(
            m.on_tick(flicker + th.frozen_after_ms + 1, &th),
            Repeat,
            "lost again"
        );
        assert_eq!(
            m.on_tick(flicker + th.slate_after_ms - 1, &th),
            Repeat,
            "slate counts from the flicker"
        );
        assert_eq!(
            m.on_tick(flicker + th.slate_after_ms, &th),
            Slate(SlateKind::Reconnecting)
        );
    }

    // Reconnecting, and the camera comes back for 0.6 s, drops for 0.7 s (> frozen_after), and comes
    // back for good: the slate stays up throughout, and live needs 1 s from the SECOND return.
    #[test]
    fn a_short_return_while_reconnecting_keeps_the_slate_and_restarts_the_run() {
        let th = t();
        let mut m = reconnecting(&th);
        let recon = Slate(SlateKind::Reconnecting);
        let first = 20_000;
        assert_eq!(frames(&mut m, first, first + 600, 10, &th), recon);
        let second = first + 1_300;
        assert_eq!(m.on_tick(second - 1, &th), recon, "lost again while lost");
        assert_eq!(
            frames(&mut m, second, second + th.resume_after_ms - 10, 10, &th),
            recon
        );
        assert_eq!(
            frames(
                &mut m,
                second + th.resume_after_ms,
                second + th.resume_after_ms,
                10,
                &th
            ),
            Emit
        );
    }

    #[test]
    fn a_long_silence_while_reconnecting_stays_on_the_reconnecting_slate() {
        let th = t();
        let mut m = reconnecting(&th);
        for at in [10_000, 60_000, 3_600_000, Ms::MAX] {
            assert_eq!(m.on_tick(at, &th), Slate(SlateKind::Reconnecting), "at {at}");
        }
        assert_eq!(m.phase(), Reconnecting, "never back to Waiting once live");
    }
}
