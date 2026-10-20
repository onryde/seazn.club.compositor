//! Camera pull link (SRT from CF Input A): reconnect forever on a capped back-off, count peer kicks,
//! and ignore every event from a generation other than the current one.
//!
//! ```text
//! Backoff --due tick: build the next generation--> Connecting --first decoded frame--> Running
//!    ^                                                  |                                 |
//!    +------------- connect timeout, or lost -----------+------ lost (EOS, error, stall) -+
//! ```
//!
//! - **Back-off.** `attempt` counts consecutive failures, and failure number f waits `delay(f - 1)`:
//!   1, 2, 4, 8, 10, 10 s (PDF failure table), retried forever. A first decoded frame resets it.
//! - **Generations.** Every build starts a new generation, and every adapter event carries the
//!   generation of the pipeline that sent it. An event from any other generation is ignored, so a late
//!   `InputLost` from a torn-down pipeline cannot knock down the healthy new one.
//! - **Kicks.** An EOS while Running is a peer kick: CF closed the playback (maintenance; F-002 saw no
//!   second-reader kick). More than `kick_alert_count` kicks in `kick_window_ms` raise the alert. A
//!   stall or an error is not a kick, and neither is an EOS before the first frame.
//! - **Stall.** A Running input with no frame for `input_stall_after_ms` is lost as `Stalled`, because
//!   CF holds a silent playback socket open (F-002). The connect has its own budget,
//!   `input_connect_timeout_ms`, so the stall rule reads Running only.
//! - **Give-ups (C10b, Owed 50).** The connect timeout and the stall rule are the link's own
//!   decisions, so it reports each one as `InputAction::Abandon` for the generation it gave up, and the
//!   adapter shuts that generation out: the domain is the one authority on them, and the adapter
//!   keeps no copy of the rules. Each generation is given up at most once, since the link then backs
//!   off and only a rebuild moves it on. A loss the adapter reported (`on_lost`) owes no abandon: the
//!   adapter decided it, and its gate is shut already.
use crate::backoff::delay;
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};
use crate::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LossReason {
    Stalled,
    Eos,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputState {
    /// The current generation is being built; `attempt` consecutive failures came before it.
    Connecting { attempt: u32, since: Ms },
    /// Frames flow; `since` is this generation's first decoded frame (C10).
    Running { since: Ms },
    /// `attempt` consecutive failures so far; the next build is at the first tick at or after `until`.
    Backoff { attempt: u32, until: Ms },
}

/// What the link asks of the input adapter (C10b): the effect a tick or a stall check decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Build generation `gen` (`Effect::RebuildInput`).
    Rebuild { gen: u64 },
    /// The link gave generation `gen` up by its own rule, the connect timeout or the stall rule
    /// (`Effect::AbandonInput`).
    Abandon { gen: u64 },
}

/// Kick history bound: the window never holds more than this many stamps (AGENTS.md §4, nothing
/// unbounded). C04 brief default. It must exceed `kick_alert_count`, or a kick storm could never read
/// as more than the alert count (pinned by `the_kick_history_bound_leaves_room_for_the_alert`).
pub const KICK_HISTORY_CAP: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputLink {
    state: InputState,
    generation: u64,
    kicks: Window,
}

impl Default for InputLink {
    fn default() -> Self {
        Self::new()
    }
}

impl InputLink {
    /// `Backoff { attempt: 0, until: 0 }`: the first tick builds generation 1.
    pub fn new() -> Self {
        Self {
            state: InputState::Backoff { attempt: 0, until: 0 },
            generation: 0,
            kicks: Window::new(KICK_HISTORY_CAP),
        }
    }

    pub fn state(&self) -> InputState {
        self.state
    }

    /// The number of builds so far, and the generation an adapter event must carry to count.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a frame of generation `gen` counts (C10b): it is the current generation, and the link
    /// has not given it up or lost it (Backoff). A frame from a generation rebuilt past, or one queued
    /// before the give-up, changes nothing. While Connecting it counts, since the adapter's
    /// `InputConnected` and its first frames race on one channel.
    pub fn takes_frames_from(&self, gen: u64) -> bool {
        gen == self.generation && !matches!(self.state, InputState::Backoff { .. })
    }

    /// The current generation's first decoded frame (C10 times "connected" to it, F-002). It counts
    /// only while Connecting: a second one while Running must not restart the stall clock.
    pub fn on_connected(&mut self, now: Ms, gen: u64) {
        if gen == self.generation && matches!(self.state, InputState::Connecting { .. }) {
            self.state = InputState::Running { since: now };
        }
    }

    /// The current generation's pipeline ended: EOS, an error, or the stall rule.
    pub fn on_lost(&mut self, now: Ms, gen: u64, reason: LossReason, t: &Thresholds) {
        if gen != self.generation {
            return;
        }
        let failed_before = match self.state {
            InputState::Running { .. } => {
                if reason == LossReason::Eos {
                    self.kicks.record(now, t.kick_window_ms);
                }
                0
            }
            InputState::Connecting { attempt, .. } => attempt,
            // Already lost: a second EOS or an error from the same pipeline changes nothing.
            InputState::Backoff { .. } => return,
        };
        self.back_off(now, failed_before, t);
    }

    /// Running but no frame since max(running since, last frame) for `input_stall_after_ms`: lost as
    /// `Stalled`, and given up (`Abandon`). A frame from before this connection does not count.
    pub fn check_stall(&mut self, now: Ms, last_frame_at: Option<Ms>, t: &Thresholds) -> Option<InputAction> {
        let InputState::Running { since } = self.state else {
            return None;
        };
        let fresh = last_frame_at.map_or(since, |f| f.max(since));
        if elapsed(now, fresh) < t.input_stall_after_ms {
            return None;
        }
        let gen = self.generation;
        self.on_lost(now, gen, LossReason::Stalled, t);
        Some(InputAction::Abandon { gen })
    }

    /// A due back-off builds the next generation (`Rebuild`); a connect past
    /// `input_connect_timeout_ms` is given up (`Abandon`) and backs off.
    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<InputAction> {
        match self.state {
            InputState::Backoff { attempt, until } if now >= until => {
                self.generation += 1;
                self.state = InputState::Connecting { attempt, since: now };
                Some(InputAction::Rebuild { gen: self.generation })
            }
            InputState::Connecting { attempt, since }
                if elapsed(now, since) >= t.input_connect_timeout_ms =>
            {
                self.back_off(now, attempt, t);
                Some(InputAction::Abandon { gen: self.generation })
            }
            _ => None,
        }
    }

    pub fn kicks(&self, now: Ms, t: &Thresholds) -> usize {
        self.kicks.count(now, t.kick_window_ms)
    }

    /// PDF failure table: "alert if it repeats > 3 times in 5 min".
    pub fn kick_alert(&self, now: Ms, t: &Thresholds) -> bool {
        self.kicks(now, t) > t.kick_alert_count
    }

    /// Failure number `failed_before + 1` waits `delay(failed_before)`. Both additions saturate: the
    /// clock is a u64 (time.rs), and "retried forever" has no last attempt.
    fn back_off(&mut self, now: Ms, failed_before: u32, t: &Thresholds) {
        let wait = delay(failed_before, t.input_backoff_base_ms, t.input_backoff_cap_ms);
        self.state = InputState::Backoff {
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

    /// A tick that built (`Rebuild`), as against one that gave a generation up or did nothing.
    fn built(a: Option<InputAction>) -> bool {
        matches!(a, Some(InputAction::Rebuild { .. }))
    }

    /// Build, connect, run: returns a link in Running at generation 1.
    fn running(now: Ms) -> InputLink {
        let mut l = InputLink::new();
        assert!(built(l.on_tick(now, &t())));
        l.on_connected(now, 1);
        l
    }

    /// Ticks forward from `now` in 10 ms steps until the next build; returns its time.
    fn next_build(l: &mut InputLink, now: Ms, th: &Thresholds) -> Ms {
        next_build_every(l, now, 10, th)
    }

    /// Ticks forward from `now` in `step` ms steps until the next build; returns its time. From
    /// Backoff or Connecting the next build is due within one connect timeout (Connecting gives up)
    /// plus the capped back-off, so a link that never builds fails here instead of hanging the suite.
    fn next_build_every(l: &mut InputLink, mut now: Ms, step: Ms, th: &Thresholds) -> Ms {
        let horizon = now + th.input_connect_timeout_ms + th.input_backoff_cap_ms + step;
        while !built(l.on_tick(now, th)) {
            now += step;
            assert!(now <= horizon, "no build by {horizon} ms: {:?}", l.state());
        }
        now
    }

    /// An EOS from Running at `now`, then the rebuild and its first frame: one kick, recovered.
    fn kick_and_recover(l: &mut InputLink, now: Ms, th: &Thresholds) -> Ms {
        let gen = l.generation();
        l.on_lost(now, gen, LossReason::Eos, th);
        let built = next_build(l, now, th);
        l.on_connected(built, l.generation());
        assert!(matches!(l.state(), InputState::Running { .. }));
        built
    }

    // ---- the brief's 12 (C04 Step 1) ----

    #[test]
    fn first_tick_builds_generation_one() {
        let mut l = InputLink::new();
        assert_eq!(l.on_tick(0, &t()), Some(InputAction::Rebuild { gen: 1 }));
        assert_eq!(l.generation(), 1);
        assert!(matches!(l.state(), InputState::Connecting { attempt: 0, .. }));
        assert_eq!(l.on_tick(1, &t()), None, "no second build while connecting");
    }

    #[test]
    fn consecutive_failures_follow_the_pdf_backoff_1_2_4_8_10_10() {
        let th = t();
        let mut l = InputLink::new();
        let mut now = 0;
        let mut gaps = Vec::new();
        let mut last_build = None;
        for _ in 0..7 {
            // wait for the next build (bounded, so a link that never builds fails rather than hangs)
            now = next_build(&mut l, now, &th);
            if let Some(b) = last_build {
                gaps.push(now - b);
            }
            last_build = Some(now);
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Error, &th); // fails instantly
        }
        // The PDF's sequence, from the config's numbers: base, doubling, capped.
        let (base, cap) = (th.input_backoff_base_ms, th.input_backoff_cap_ms);
        let want: Vec<Ms> = (0..6).map(|a| (base << a).min(cap)).collect();
        assert_eq!(
            want,
            vec![1_000, 2_000, 4_000, 8_000, 10_000, 10_000],
            "PDF: 1 s → 10 s"
        );
        let rounded: Vec<Ms> = gaps.iter().map(|g| g / 10 * 10).collect();
        assert_eq!(rounded, want);
    }

    #[test]
    fn loss_from_running_restarts_backoff_at_one_second() {
        let th = t();
        let mut l = running(0);
        l.on_lost(5_000, 1, LossReason::Stalled, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: 5_000 + th.input_backoff_base_ms
            }
        );
    }

    #[test]
    fn connect_that_hangs_times_out_into_backoff() {
        let th = t();
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        assert_eq!(l.on_tick(th.input_connect_timeout_ms - 1, &th), None);
        assert!(matches!(l.state(), InputState::Connecting { .. }), "1 ms early");
        assert_eq!(
            l.on_tick(th.input_connect_timeout_ms, &th),
            Some(InputAction::Abandon { gen: 1 }),
            "the link gives up the generation it timed out"
        );
        assert!(matches!(l.state(), InputState::Backoff { attempt: 1, .. }));
    }

    #[test]
    fn stale_generation_loss_is_ignored() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Eos, &th);
        // A wait loop on a fixed `now` hangs when the back-off is wrong; the assertion fails instead.
        assert!(built(l.on_tick(2_000, &th)), "the 1 s back-off ends at 2 s");
        l.on_connected(2_000, 2);
        l.on_lost(2_100, 1, LossReason::Error, &th); // late event from the torn-down gen-1 pipeline
        assert_eq!(l.state(), InputState::Running { since: 2_000 });
    }

    #[test]
    fn stale_generation_connected_is_ignored() {
        let mut l = InputLink::new();
        l.on_tick(0, &t());
        l.on_connected(0, 0);
        assert!(matches!(l.state(), InputState::Connecting { .. }));
    }

    #[test]
    fn second_loss_while_backing_off_is_ignored() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Eos, &th);
        let s = l.state();
        l.on_lost(1_010, 1, LossReason::Error, &th);
        assert_eq!(l.state(), s);
    }

    #[test]
    fn kick_alert_needs_more_than_three_eos_in_five_minutes() {
        let th = t();
        let mut l = running(0);
        let mut now = 0;
        for k in 1..=th.kick_alert_count + 1 {
            now += 20_000; // PDF chaos: relay kicks the reader every 20 s
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Eos, &th);
            now = next_build_every(&mut l, now, 100, &th);
            l.on_connected(now, l.generation());
            assert_eq!(l.kick_alert(now, &th), k > th.kick_alert_count, "after {k} kicks");
        }
        assert!(
            !l.kick_alert(now + th.kick_window_ms + 1, &th),
            "alert clears once kicks age out"
        );
    }

    // The window's edge, from C02's `Window` rule rather than this module: a stamp exactly `span` old
    // still counts, and one 1 ms older does not (window::tests::stamp_exactly_span_old_still_counts,
    // counts_only_inside_span). So `kick_alert_count + 1` kicks whose first and last lie exactly
    // `kick_window_ms` apart raise the alert at the last kick's instant, and it clears 1 ms later.
    #[test]
    fn kicks_exactly_the_window_apart_raise_the_alert_and_clear_1_ms_later() {
        let th = t();
        let (n, w) = (th.kick_alert_count as Ms, th.kick_window_ms);
        let t0 = 10_000;
        let mut l = running(0);
        for i in 0..=n {
            let at = t0 + w * i / n;
            assert!(
                matches!(l.state(), InputState::Running { .. }),
                "kick {i} must come from Running"
            );
            if i == n {
                assert!(!l.kick_alert(at, &th), "{n} kicks are not yet more than {n}");
            }
            l.on_lost(at, l.generation(), LossReason::Eos, &th);
            if i < n {
                // The 1 s back-off after a kick, then the rebuild's first frame: long before the next.
                let rebuilt = at + th.input_backoff_base_ms;
                assert!(rebuilt < t0 + w * (i + 1) / n);
                assert!(
                    built(l.on_tick(rebuilt, &th)),
                    "the kick's back-off ends at {rebuilt}"
                );
                l.on_connected(rebuilt, l.generation());
            }
        }
        let last = t0 + w;
        assert_eq!(
            l.kicks(last, &th),
            th.kick_alert_count + 1,
            "the first kick is exactly the window old, so it still counts"
        );
        assert!(l.kick_alert(last, &th));
        assert_eq!(
            l.kicks(last + 1, &th),
            th.kick_alert_count,
            "1 ms later it is out"
        );
        assert!(!l.kick_alert(last + 1, &th));
    }

    #[test]
    fn camera_stall_is_not_a_kick() {
        let th = t();
        let mut l = running(0);
        let mut now = 0;
        for _ in 0..10 {
            now += 1_000;
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Stalled, &th);
            now = next_build_every(&mut l, now, 100, &th);
            l.on_connected(now, l.generation());
        }
        assert_eq!(l.generation(), 11, "ten rebuilds happened");
        assert_eq!(l.kicks(now, &th), 0, "a camera stall is not a peer kick");
    }

    #[test]
    fn running_without_frames_stalls_into_a_rebuild() {
        // CF can keep the playback socket open with nothing flowing (phone backgrounded)
        let th = t();
        let mut l = running(0);
        assert_eq!(l.check_stall(th.input_stall_after_ms - 1, None, &th), None);
        assert!(matches!(l.state(), InputState::Running { .. }));
        assert_eq!(
            l.check_stall(th.input_stall_after_ms, None, &th),
            Some(InputAction::Abandon { gen: 1 }),
            "the link gives up the generation that stalled"
        );
        assert!(matches!(l.state(), InputState::Backoff { attempt: 1, .. }));
    }

    #[test]
    fn stall_clock_runs_from_the_later_of_connect_and_last_frame() {
        let th = t();
        let mut l = running(0);
        l.check_stall(10_000, Some(9_000), &th); // a fresh frame 1 s ago: healthy
        assert!(matches!(l.state(), InputState::Running { .. }));
        l.check_stall(9_000 + th.input_stall_after_ms, Some(9_000), &th);
        assert!(matches!(l.state(), InputState::Backoff { .. }));
        // a frame from BEFORE this connection must not count: reconnect at 20 s, old frame at 9 s
        let mut l = running(20_000);
        l.check_stall(20_000 + th.input_stall_after_ms - 1, Some(9_000), &th);
        assert!(
            matches!(l.state(), InputState::Running { .. }),
            "an old frame must not stall a fresh connection"
        );
    }

    #[test]
    fn stall_is_not_a_kick_and_is_ignored_unless_running() {
        let th = t();
        let mut l = InputLink::new();
        l.check_stall(1_000_000, None, &th); // backing off: no effect
        assert!(matches!(l.state(), InputState::Backoff { attempt: 0, until: 0 }));
        let mut l = running(0);
        l.check_stall(th.input_stall_after_ms, None, &th);
        assert_eq!(l.kicks(th.input_stall_after_ms, &th), 0);
    }

    // ---- sequences: a second call, an interruption, both generation directions ----

    // A second call: the pipeline that was kicked can report EOS twice (EOS, then an error on teardown).
    // One kick is one kick.
    #[test]
    fn a_second_eos_from_the_kicked_pipeline_counts_once() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Eos, &th);
        l.on_lost(1_005, 1, LossReason::Eos, &th);
        l.on_lost(1_010, 1, LossReason::Error, &th);
        assert_eq!(l.kicks(1_010, &th), 1);
    }

    // kick → recover → kick: the second kick lands on generation 2 and counts as the second.
    #[test]
    fn kick_recover_kick_counts_two() {
        let th = t();
        let mut l = running(0);
        let back = kick_and_recover(&mut l, 1_000, &th);
        assert_eq!((l.generation(), l.kicks(back, &th)), (2, 1));
        let again = kick_and_recover(&mut l, back + 5_000, &th);
        assert_eq!((l.generation(), l.kicks(again, &th)), (3, 2));
        assert_eq!(
            l.state(),
            InputState::Running { since: again },
            "recovered after the second kick"
        );
    }

    #[test]
    fn eos_before_the_first_frame_is_a_failure_not_a_kick() {
        let th = t();
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        l.on_lost(500, 1, LossReason::Eos, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: 500 + th.input_backoff_base_ms
            }
        );
        assert_eq!(l.kicks(500, &th), 0);
    }

    // A second call: a duplicate "connected" while Running must not move `since`, or the stall clock
    // (which runs from it when no frame came) would restart on a link that never delivered a frame.
    #[test]
    fn a_second_connected_does_not_restart_the_run() {
        let th = t();
        let mut l = running(0);
        l.on_connected(4_000, 1);
        assert_eq!(l.state(), InputState::Running { since: 0 });
        l.check_stall(th.input_stall_after_ms, None, &th);
        assert!(
            matches!(l.state(), InputState::Backoff { .. }),
            "stall clock from the first frame"
        );
    }

    // Old-gen connect, the other direction from the brief's gen 0: generation 1's first frame,
    // delivered late after generation 2 was built, must not mark generation 2 Running.
    #[test]
    fn old_generation_connected_after_a_rebuild_is_ignored() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Error, &th);
        let built = next_build(&mut l, 1_000, &th);
        assert_eq!(l.generation(), 2);
        l.on_connected(built + 100, 1);
        assert!(
            matches!(l.state(), InputState::Connecting { .. }),
            "gen 1 is gone"
        );
        l.on_connected(built + 200, 2);
        assert_eq!(l.state(), InputState::Running { since: built + 200 });
    }

    // A generation the domain never built cannot be the current pipeline's.
    #[test]
    fn a_generation_not_yet_built_is_ignored() {
        let th = t();
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        l.on_connected(10, 2);
        assert!(matches!(l.state(), InputState::Connecting { .. }));
        let mut l = running(0);
        l.on_lost(1_000, 2, LossReason::Eos, &th);
        assert_eq!(l.state(), InputState::Running { since: 0 });
        assert_eq!(l.kicks(1_000, &th), 0);
    }

    // The stall rule reads Running only: backing off after a loss, and connecting (which has its own
    // timeout), it changes nothing, however old the last frame.
    #[test]
    fn stall_check_while_backing_off_or_connecting_changes_nothing() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Error, &th);
        let backing_off = l.state();
        l.check_stall(1_000 + th.input_stall_after_ms * 10, None, &th);
        assert_eq!(l.state(), backing_off);
        let built = next_build(&mut l, 1_000, &th);
        let connecting = l.state();
        l.check_stall(built + th.input_stall_after_ms, Some(0), &th);
        assert_eq!(l.state(), connecting, "the connect timeout, not the stall rule");
        assert_eq!(l.generation(), 2);
    }

    // After an interruption: three failed connects build up the back-off; a first frame resets it, so
    // the next loss waits the base again.
    #[test]
    fn a_first_frame_resets_the_backoff_to_its_base() {
        let th = t();
        let mut l = InputLink::new();
        let mut now = 0;
        for _ in 0..3 {
            now = next_build(&mut l, now, &th);
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Error, &th);
        }
        assert!(matches!(l.state(), InputState::Backoff { attempt: 3, .. }));
        now = next_build(&mut l, now, &th);
        l.on_connected(now, l.generation());
        l.on_lost(now + 1, l.generation(), LossReason::Error, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: now + 1 + th.input_backoff_base_ms
            }
        );
    }

    #[test]
    fn the_kick_history_bound_leaves_room_for_the_alert() {
        let th = t();
        assert!(
            KICK_HISTORY_CAP > th.kick_alert_count,
            "{KICK_HISTORY_CAP} ≤ {}",
            th.kick_alert_count
        );
        // a kick storm: more kicks inside one window than the history holds
        let mut l = running(0);
        let mut now = 0;
        for _ in 0..KICK_HISTORY_CAP + 8 {
            now = kick_and_recover(&mut l, now + 100, &th);
        }
        assert!(now < th.kick_window_ms, "the storm fits one window ({now} ms)");
        assert_eq!(l.kicks(now, &th), KICK_HISTORY_CAP, "bounded");
        assert!(l.kick_alert(now, &th), "the alert holds through the storm");
    }

    // The clock is a u64 (time.rs): a back-off near its top saturates instead of overflowing.
    #[test]
    fn backoff_from_the_top_of_the_clock_range_saturates() {
        let th = t();
        let top = Ms::MAX - 10;
        let mut l = running(top);
        l.on_lost(top + 5, 1, LossReason::Error, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: Ms::MAX
            }
        );
        assert!(
            built(l.on_tick(Ms::MAX, &th)),
            "the build still comes, at the top"
        );
        assert_eq!(l.on_tick(Ms::MAX, &th), None);
        assert!(matches!(l.state(), InputState::Connecting { attempt: 1, .. }));
    }

    // ---- C10b: the link's own give-ups (Owed 50) ----

    // A second call: once a generation is given up, the link backs off, and neither rule gives it up
    // again, however long nothing comes; the next tick that acts is the rebuild, for the next one.
    #[test]
    fn a_generation_is_given_up_once_by_either_rule() {
        let th = t();
        let mut checked = 0;
        // The connect timeout.
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        let at = th.input_connect_timeout_ms;
        assert_eq!(l.on_tick(at, &th), Some(InputAction::Abandon { gen: 1 }));
        for later in [at, at + 1, at + th.input_backoff_base_ms - 1] {
            assert_eq!(l.on_tick(later, &th), None, "connect: tick at {later}");
            assert_eq!(l.check_stall(later, None, &th), None, "connect: stall at {later}");
            checked += 2;
        }
        assert_eq!(
            l.on_tick(at + th.input_backoff_base_ms, &th),
            Some(InputAction::Rebuild { gen: 2 })
        );
        // The stall rule.
        let mut l = running(0);
        let at = th.input_stall_after_ms;
        assert_eq!(
            l.check_stall(at, None, &th),
            Some(InputAction::Abandon { gen: 1 })
        );
        for later in [at, at + 1, at + th.input_backoff_base_ms - 1] {
            assert_eq!(l.check_stall(later, None, &th), None, "stall: stall at {later}");
            assert_eq!(l.on_tick(later, &th), None, "stall: tick at {later}");
            checked += 2;
        }
        assert_eq!(
            l.on_tick(at + th.input_backoff_base_ms, &th),
            Some(InputAction::Rebuild { gen: 2 })
        );
        assert_eq!(checked, 12);
    }

    // A loss the adapter reported (EOS, error, or its own stall report) is the adapter's decision: no
    // abandon is owed, from Running or from Connecting, at the loss or on any tick of the back-off.
    #[test]
    fn a_loss_the_adapter_reported_is_never_abandoned() {
        let th = t();
        let mut checked = 0;
        for reason in [LossReason::Eos, LossReason::Error, LossReason::Stalled] {
            for from_running in [true, false] {
                let mut l = if from_running {
                    running(0)
                } else {
                    InputLink::new()
                };
                if !from_running {
                    l.on_tick(0, &th);
                }
                l.on_lost(100, 1, reason, &th);
                let mut now = 100;
                loop {
                    let a = l.on_tick(now, &th).or(l.check_stall(now, None, &th));
                    match a {
                        Some(InputAction::Rebuild { gen: 2 }) => break,
                        None => {}
                        other => panic!("({reason:?}, running {from_running}) at {now}: {other:?}"),
                    }
                    now += 10;
                    assert!(now < 100 + th.input_backoff_cap_ms + 20, "no rebuild");
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 6);
    }

    // The two rules name the generation they gave up, never a later one: after rebuilds, the
    // abandon carries the current generation, and a rebuild in the same tick (a back-off of 0)
    // follows it with the next.
    #[test]
    fn an_abandon_names_the_generation_it_gave_up() {
        let th = Thresholds {
            input_backoff_base_ms: 0,
            ..t()
        };
        let mut l = InputLink::new();
        assert_eq!(l.on_tick(0, &th), Some(InputAction::Rebuild { gen: 1 }));
        let at = th.input_connect_timeout_ms;
        assert_eq!(l.on_tick(at, &th), Some(InputAction::Abandon { gen: 1 }));
        assert_eq!(l.on_tick(at, &th), Some(InputAction::Rebuild { gen: 2 }));
        l.on_connected(at, 2);
        let stall = at + th.input_stall_after_ms;
        assert_eq!(
            l.check_stall(stall, None, &th),
            Some(InputAction::Abandon { gen: 2 })
        );
        assert_eq!(l.on_tick(stall, &th), Some(InputAction::Rebuild { gen: 3 }));
    }

    // C10b: frames count only from the current generation while it is not given up or lost.
    #[test]
    fn frames_count_only_from_the_current_live_generation() {
        let th = t();
        let mut checked = 0;
        let mut l = InputLink::new();
        assert!(!l.takes_frames_from(0), "nothing built");
        assert!(!l.takes_frames_from(1), "nothing built");
        l.on_tick(0, &th);
        for (gen, want) in [(0, false), (1, true), (2, false)] {
            assert_eq!(l.takes_frames_from(gen), want, "connecting, gen {gen}");
            checked += 1;
        }
        l.on_connected(10, 1);
        for (gen, want) in [(0, false), (1, true), (2, false)] {
            assert_eq!(l.takes_frames_from(gen), want, "running, gen {gen}");
            checked += 1;
        }
        // Given up by the stall rule: none, the given-up generation included.
        assert!(l.check_stall(10 + th.input_stall_after_ms, None, &th).is_some());
        for gen in 0..=2 {
            assert!(!l.takes_frames_from(gen), "given up, gen {gen}");
            checked += 1;
        }
        // Rebuilt: the next generation's, never the given-up one's.
        let at = next_build(&mut l, 10 + th.input_stall_after_ms, &th);
        assert_eq!(l.generation(), 2, "rebuilt at {at}");
        for (gen, want) in [(1, false), (2, true), (3, false)] {
            assert_eq!(l.takes_frames_from(gen), want, "rebuilt, gen {gen}");
            checked += 1;
        }
        // Lost as the adapter reported it: none.
        l.on_lost(at + 1, 2, LossReason::Error, &th);
        assert!(!l.takes_frames_from(2), "lost");
        checked += 1;
        assert_eq!(checked, 13);
    }
}
