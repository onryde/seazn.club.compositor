//! CEF overlay lifecycle. It never blocks video: a failed page restarts on a back-off while the adapter
//! holds the last overlay frame (C11); the planned recycle is a restart, never a failure; repeated
//! failures hide the overlay and retry after a cooldown. Whatever happens, one bound keeps a stale
//! score off air: a frame stays on air at most `cef_hang_after_ms` after the last proof of life of the
//! page that produced it.
//!
//! ```text
//! Backoff --due tick: restart, next generation--> Starting --ready signal--> Ready
//!    ^                                               |                         |
//!    +---------- failure: load timeout or crash -----+--------- crash ---------+
//!         (the cef_fail_count-th in the window enters Failed + Hide(Failed))   |
//!                                                                              |
//! Ready --beat--> Ready (the bound moves; the recycle does not)                |
//! Ready --the bound: failure (a hang) + Hide(Hang)--> Backoff | Failed         |
//! Ready --recycle due--> Starting (Restart; not a failure)                     |
//! Starting | Backoff --the bound of a crashed or recycled page's frame:        |
//!                      Hide(Crash | Recycle), nothing else moves               |
//! Failed --retry_at--> Starting (Restart)                                      v
//! ```
//!
//! - **Ready.** The page's readiness signal for the current generation, while Starting: F-001 pinned it
//!   to `console.info("seazn:ready")`, read by C11 from the `cefconsole` log category. Frames flow
//!   before it (F-001), so a first frame is never ready (C11). A ready from another generation, a
//!   second ready, or one that comes after the load timed out changes nothing: the recycle clock runs
//!   from the first ready, and a timed-out load stays one failure.
//! - **Beat.** While Ready, the page logs `seazn:alive` every `cef_alive_every_ms` (R2005; C11 relays
//!   it, C06 routes it). The ready signal counts as the first beat. A beat from any other generation,
//!   or in any other state, changes nothing: the page beats only after ready, and a beat never makes a
//!   page Ready. A beat moves only the bound, never the recycle, and never earlier: a beat stamped
//!   before the last one changes nothing (PR #22 review M3).
//! - **The bound** (coordinator decision 2026-10-18, PR #22 review G1, generalising the
//!   owner-approved 10 s rule). The frame on air may stay on air at most `cef_hang_after_ms` after its
//!   page's last proof of life (its ready, or a beat). C11 writes the overlay slot only from a page
//!   after its own ready, so the frame on air is the Ready page's, or the last frame of a Ready page
//!   whose life a crash or a recycle ended, held through the reload. The first tick at or after the
//!   bound hides it, naming its page; nothing extends the bound but a beat of that page while Ready,
//!   and only a newer page's Ready replaces the frame first. The bound is checked before every other
//!   timer: a restart, a load timeout or a recycle due on the same tick follows on the next one.
//! - **Failure.** A crash of the current generation while Starting or Ready (C11: a bus error, the
//!   renderer exiting, a CEF child gone, RSS over its cap), a load with no ready signal
//!   `cef_load_timeout_ms` after its restart, or a hang: the bound of the Ready page's own frame, a
//!   page with no beat for `cef_hang_after_ms` (`docs/_INDEX.md` Owed 18). A JS hang leaves the
//!   renderer alive, and cefsrc emits only on paint (F-001), so the beat is the only signal. A crash
//!   reported while backing off or Failed is that same failure reported again, and is not counted
//!   twice; a crash reported past the bound but before the tick is that crash, and not a hang too.
//!   When the hang and the recycle fall due on the same tick, the hang wins: it is counted, never
//!   laundered as a recycle.
//! - **Back-off.** The n-th failure within `cef_fail_window_ms` waits `delay(n - 1)` on the CEF
//!   back-off (1 s → 10 s), so a page that keeps failing waits longer.
//! - **Failed.** The `cef_fail_count`-th failure within `cef_fail_window_ms` (spec §4.2: 3 in 5 min; a
//!   failure exactly the window old still counts, as in `Window`) enters Failed and returns
//!   `Hide { reason: Failed }`, once. Failed is not terminal (C05 brief): a flaky deploy of the page early in a match must not
//!   lose the score bug for the whole match, so `cef_fail_window_ms` after it the page restarts. By then
//!   every failure that led to Failed is at least the window old, so the retried page gets its full
//!   `cef_fail_count` attempts again.
//! - **Hide.** Every `Hide` names its reason and a generation (PR #22 review I2; C12 logs
//!   `overlay hidden reason=<r> gen=<g>`). A bound hide names the path that started it and the page
//!   whose frame comes off air; a load timeout never starts one, because a page that timed out never
//!   showed a frame of its own, so its reloads' load timeouts neither end nor move a bound:
//!   - `Hang`: the Ready page itself went silent. A hung page's last frame may be a stale score, so it
//!     comes off at once, below the count and at it (the reason names the cause, not the state left;
//!     owner-approved design, 2026-10-18).
//!   - `Crash`: a crash of a Ready page below the count, and no newer page Ready within the bound.
//!   - `Recycle`: a recycle, and no newer page Ready within the bound.
//!   - `Failed`: a crash or a load timeout that enters Failed, naming the generation that failed.
//!
//!   The overlay stays hidden from a `Hide` until the next generation's Ready; nothing else lifts it.
//!   After a `Hang` or `Failed` hide the next action is a `Restart` (at `retry_at` from Failed, at the
//!   back-off's end from a hang); a `Crash` or `Recycle` hide leaves the reload loading, so the next is
//!   a `Restart` or that reload's `Failed`. Nothing of the hidden generation may reach the programme
//!   before it; under F-001's console readiness a page that never signals ready keeps painting, so the
//!   adapters must hold the hide against its frames, and lift it once a later generation signals ready
//!   (C08 maps a `Hang` or `Failed` hide to `hide_overlay()` and `graphics.stop()`, and a `Crash` or
//!   `Recycle` hide to `hide_overlay()` alone, so the reload survives; C11 writes the slot only from a
//!   generation that signalled its own ready, and closes a generation's slot at its stop, its restart
//!   or its reported crash; `docs/_INDEX.md` Owed 17).
//! - **Recycle.** `cef_recycle_every_ms` after the ready signal the browser is restarted (spec §6, every
//!   2 h). It is planned, so it records no failure.
//! - **Generations.** Every restart is a new generation, and every adapter event carries the
//!   generation of the page that sent it. An event from any other generation is ignored.
use crate::backoff::delay;
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};
use crate::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsState {
    /// The current generation's page is loading; `since` is its restart.
    Starting { since: Ms },
    /// The current generation signalled ready at `since` (the recycle clock: its recycle is due
    /// `cef_recycle_every_ms` after it). `last_alive` is its last beat, the ready signal included: its
    /// last proof of life, from which its frame's bound runs.
    Ready { since: Ms, last_alive: Ms },
    /// A failure is backing off, and the next restart is at the first tick at or after `until`.
    /// `attempt` is the back-off exponent: the failures within `cef_fail_window_ms`, this one
    /// included, minus one.
    Backoff { attempt: u32, until: Ms },
    /// The overlay is hidden, and the next restart is at the first tick at or after `retry_at`.
    Failed { retry_at: Ms },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsAction {
    /// Build the page of `generation()` (C06 `RestartGraphics { gen }`). The adapter holds the last
    /// overlay frame while it loads (C11).
    Restart,
    /// Take the overlay off air (C06 `HideOverlay { reason, gen }`) until the next generation's Ready.
    /// `gen` is the page whose frame came off air at its bound (`Hang`, `Crash`, `Recycle`), or the
    /// generation whose failure entered Failed (`Failed`). C12 logs the one line
    /// `overlay hidden reason=<r> gen=<g>` for it (PR #22 review I2).
    Hide { reason: HideReason, gen: u64 },
}

/// What took the overlay off air: the path that started it, not the state the machine is left in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideReason {
    /// The Ready page stopped beating for `cef_hang_after_ms` (Owed 18): below the count, and at it.
    Hang,
    /// A crash or a load timeout entered Failed (the `cef_fail_count`-th failure in the window).
    Failed,
    /// A Ready page crashed, and its last frame reached its bound with no newer generation Ready.
    Crash,
    /// A Ready page was recycled, and its last frame reached its bound with no newer generation Ready.
    Recycle,
}

/// What ended a Ready page's life while its last frame stayed on air. A hang ends it at the bound, so
/// it hides there and is not one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Crash,
    Recycle,
}

/// The last frame of a page whose life a crash or a recycle ended while it was Ready: it stays on
/// air through the reload until its bound or the next Ready (C11 writes the slot only from a page
/// after its own ready, and `hide_overlay` clears it, so the machine tracks it without seeing a frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    /// The generation that produced the frame.
    gen: u64,
    /// That page's last proof of life: its ready or its last beat.
    proof: Ms,
    ended: Ended,
}

/// Failure history bound: the window never holds more than this many stamps (AGENTS.md §4, nothing
/// unbounded). C05 brief default. It must be at least `cef_fail_count`, or the window could never
/// count enough failures to enter Failed (pinned by `the_failure_history_bound_leaves_room_for_failed`).
pub const FAILURE_HISTORY_CAP: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graphics {
    state: GraphicsState,
    generation: u64,
    failures: Window,
    /// Read only while not Ready: a Ready page's own frames replace any held frame, and each way out
    /// of Ready (a crash, a recycle, the bound) writes it.
    held: Option<Held>,
}

impl Default for Graphics {
    fn default() -> Self {
        Self::new()
    }
}

impl Graphics {
    /// `Backoff { attempt: 0, until: 0 }`: the first tick builds generation 1.
    pub fn new() -> Self {
        Self {
            state: GraphicsState::Backoff { attempt: 0, until: 0 },
            generation: 0,
            failures: Window::new(FAILURE_HISTORY_CAP),
            held: None,
        }
    }

    pub fn state(&self) -> GraphicsState {
        self.state
    }

    /// The number of restarts so far, and the generation an adapter event must carry to count.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The page of generation `gen` signalled ready (F-001's console line, relayed by C11). It counts
    /// only while that page is loading. Its frames now reach air: a hide lifts, and a held frame is
    /// replaced.
    pub fn on_ready(&mut self, now: Ms, gen: u64) {
        if gen == self.generation && matches!(self.state, GraphicsState::Starting { .. }) {
            self.state = GraphicsState::Ready {
                since: now,
                last_alive: now,
            };
        }
    }

    /// The page of generation `gen` beat (R2005's `seazn:alive`, relayed by C11 as `CefAlive`). It
    /// counts only while that page is Ready.
    pub fn on_alive(&mut self, now: Ms, gen: u64) {
        if gen != self.generation {
            return;
        }
        if let GraphicsState::Ready { since, last_alive } = self.state {
            self.state = GraphicsState::Ready {
                since,
                last_alive: now.max(last_alive),
            };
        }
    }

    /// The page of generation `gen` crashed. `Hide { reason: Failed, .. }` when this failure enters
    /// Failed.
    pub fn on_crashed(&mut self, now: Ms, gen: u64, t: &Thresholds) -> Option<GraphicsAction> {
        if gen != self.generation {
            return None;
        }
        match self.state {
            GraphicsState::Starting { .. } | GraphicsState::Ready { .. } => {
                // A Ready page's last frame stays on air to its bound; at the count `fail` hides it.
                self.end_life(Ended::Crash);
                self.fail(now, t)
            }
            // Already failed: the same page reporting again changes nothing.
            GraphicsState::Backoff { .. } | GraphicsState::Failed { .. } => None,
        }
    }

    /// `Some(Restart)` when a restart is due (the back-off is over, the recycle is due, or the
    /// cooldown is over); `Hide { reason: Failed, .. }` when a load timeout enters Failed; and a hide
    /// naming its path (`Hang`, `Crash`, `Recycle`) when the frame on air reaches its bound.
    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> {
        // The bound, checked first: the frame on air comes off on the first tick at or after it, and
        // a restart, a load timeout or a recycle due on the same tick follows on the next one.
        if let Some(hide) = self.on_bound_tick(now, t) {
            return Some(hide);
        }
        match self.state {
            GraphicsState::Backoff { until, .. } if now >= until => Some(self.restart(now)),
            GraphicsState::Starting { since } if elapsed(now, since) >= t.cef_load_timeout_ms => {
                self.fail(now, t)
            }
            // The planned recycle: a restart, and no failure is recorded. Its page's last frame stays
            // on air while the reload loads, to its bound.
            GraphicsState::Ready { since, .. } if elapsed(now, since) >= t.cef_recycle_every_ms => {
                self.end_life(Ended::Recycle);
                Some(self.restart(now))
            }
            GraphicsState::Failed { retry_at } if now >= retry_at => Some(self.restart(now)),
            _ => None,
        }
    }

    /// The bound alone: `on_tick`'s first check, and nothing else. A `Hide` naming its path when the
    /// frame on air reaches its bound (a hang is still a counted failure); no restart, load timeout,
    /// recycle or cooldown's end is run. C06's final hold ticks graphics with this, because nothing is
    /// rebuilt during the hold: a restart the hold dropped after the generation had moved would leave
    /// the healthy page's beats ignored, and its Final card would come off air at a bound that never
    /// should have started (coordinator decision on the C05b PR #22 review, carried into C06).
    pub fn on_bound_tick(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> {
        let (gen, proof, reason) = self.on_air()?;
        if elapsed(now, proof) < t.cef_hang_after_ms {
            return None;
        }
        if reason == HideReason::Hang {
            // The Ready page itself is silent: a hang (Owed 18) is a counted failure, and is hidden as
            // a hang whatever `fail` answers, because its last frame may be a stale score. Being
            // first, it is never laundered as a recycle due on the same tick.
            self.fail(now, t);
        }
        Some(self.hide(reason, gen))
    }

    /// The frame on air, as (the generation that produced it, that page's last proof of life, the
    /// reason a hide at its bound names): the Ready page's own frames, or a held frame.
    fn on_air(&self) -> Option<(u64, Ms, HideReason)> {
        match self.state {
            GraphicsState::Ready { last_alive, .. } => Some((self.generation, last_alive, HideReason::Hang)),
            _ => self.held.map(|h| {
                let reason = match h.ended {
                    Ended::Crash => HideReason::Crash,
                    Ended::Recycle => HideReason::Recycle,
                };
                (h.gen, h.proof, reason)
            }),
        }
    }

    /// A crash or a recycle ends the Ready page's life: its last frame stays on air, held, and its
    /// bound still runs from its last proof of life. A page that is not Ready never showed a frame of
    /// its own, so a frame already held keeps its page, its bound and its cause.
    fn end_life(&mut self, ended: Ended) {
        if let GraphicsState::Ready { last_alive, .. } = self.state {
            self.held = Some(Held {
                gen: self.generation,
                proof: last_alive,
                ended,
            });
        }
    }

    /// Takes the overlay off air: nothing of this machine's pages is shown until the next Ready.
    fn hide(&mut self, reason: HideReason, gen: u64) -> GraphicsAction {
        self.held = None;
        GraphicsAction::Hide { reason, gen }
    }

    fn restart(&mut self, now: Ms) -> GraphicsAction {
        self.generation += 1;
        self.state = GraphicsState::Starting { since: now };
        GraphicsAction::Restart
    }

    /// Records a failure at `now`. The `cef_fail_count`-th within the window enters Failed; any other
    /// backs off. Both deadlines saturate: the clock is a u64 (time.rs).
    fn fail(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> {
        let n = self.failures.record(now, t.cef_fail_window_ms);
        if n >= t.cef_fail_count {
            self.state = GraphicsState::Failed {
                retry_at: now.saturating_add(t.cef_fail_window_ms),
            };
            return Some(self.hide(HideReason::Failed, self.generation));
        }
        // n ≤ FAILURE_HISTORY_CAP (32), so the cast is exact; n ≥ 1, as this failure is recorded.
        let attempt = (n as u32).saturating_sub(1);
        let wait = delay(attempt, t.cef_backoff_base_ms, t.cef_backoff_cap_ms);
        self.state = GraphicsState::Backoff {
            attempt,
            until: now.saturating_add(wait),
        };
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GraphicsAction::{Hide, Restart};
    use HideReason::{Crash, Failed, Hang, Recycle};

    fn t() -> Thresholds {
        Thresholds::default()
    }

    /// The hide C06 maps to `HideOverlay { reason, gen }`, and C12 logs as
    /// `overlay hidden reason=<r> gen=<g>`.
    fn hide(reason: HideReason, gen: u64) -> Option<GraphicsAction> {
        Some(Hide { reason, gen })
    }

    /// The CEF back-off, written independently of `backoff::delay`: the base, doubled once per
    /// failure before this one in the window, never above the cap.
    fn wait(th: &Thresholds, failed_before: usize) -> Ms {
        (0..failed_before).fold(th.cef_backoff_base_ms.min(th.cef_backoff_cap_ms), |w, _| {
            w.saturating_mul(2).min(th.cef_backoff_cap_ms)
        })
    }

    /// Run ticks every 100 ms; the page signals ready `ready_after` ms after each restart (None = never).
    fn run(until: Ms, ready_after: Option<Ms>) -> (Graphics, Vec<(Ms, GraphicsAction)>) {
        let th = t();
        let mut g = Graphics::new();
        let mut acts = Vec::new();
        let mut started: Option<(Ms, u64)> = None;
        let mut now = 0;
        while now <= until {
            if let Some(a) = g.on_tick(now, &th) {
                if a == Restart {
                    started = Some((now, g.generation()));
                }
                acts.push((now, a));
            }
            if let (Some((at, gen)), Some(r)) = (started, ready_after) {
                if now >= at + r {
                    g.on_ready(now, gen);
                    started = None;
                }
            }
            now += 100;
        }
        (g, acts)
    }

    /// Ticks forward from `now` in `step` ms until the next action, and returns its time and kind.
    /// Every state acts within its own timer (the load timeout plus the capped back-off, the recycle,
    /// the cooldown), so a machine that never acts fails here instead of hanging the suite (C04
    /// review: bound every wait).
    fn next_action(g: &mut Graphics, mut now: Ms, step: Ms, th: &Thresholds) -> (Ms, GraphicsAction) {
        let longest = th
            .cef_recycle_every_ms
            .max(th.cef_fail_window_ms)
            .max(th.cef_load_timeout_ms + th.cef_backoff_cap_ms);
        let horizon = now + longest + step;
        loop {
            if let Some(a) = g.on_tick(now, th) {
                return (now, a);
            }
            now += step;
            assert!(now <= horizon, "no action by {horizon} ms: {:?}", g.state());
        }
    }

    /// The next action must be a restart; returns its time.
    fn next_restart(g: &mut Graphics, now: Ms, step: Ms, th: &Thresholds) -> Ms {
        let (at, a) = next_action(g, now, step, th);
        assert_eq!(a, Restart, "at {at}: {:?}", g.state());
        at
    }

    /// Crashes the current page at each instant of `at`, making it ready at the restart before each
    /// crash and beating at the crash itself (a page silent for `cef_hang_after_ms` would have hung
    /// first, and a frame past its bound comes off air at the next tick). Each instant must lie at or
    /// after the restart before it. Returns the machine and the last crash's action.
    fn crashes(at: &[Ms], th: &Thresholds) -> (Graphics, Option<GraphicsAction>) {
        let mut g = Graphics::new();
        let mut last = None;
        for &c in at {
            let restart = match g.state() {
                GraphicsState::Backoff { until, .. } => until,
                other => panic!("crash at {c}: expected Backoff before the restart, got {other:?}"),
            };
            assert!(c >= restart, "crash at {c} before its restart at {restart}");
            assert_eq!(g.on_tick(restart, th), Some(Restart));
            g.on_ready(restart, g.generation());
            assert!(matches!(g.state(), GraphicsState::Ready { .. }));
            g.on_alive(c, g.generation());
            last = g.on_crashed(c, g.generation(), th);
        }
        (g, last)
    }

    /// `count` failures spread evenly from 0 to `last`, the first and last exactly `last` apart.
    fn spread(count: usize, last: Ms) -> Vec<Ms> {
        assert!(count >= 2, "a spread needs two ends");
        (0..count).map(|k| k as Ms * last / (count as Ms - 1)).collect()
    }

    // ---- the brief's 6 (C05 Step 1), with every wait bounded ----

    #[test]
    fn healthy_page_is_ready_after_one_restart() {
        let (g, acts) = run(5_000, Some(1_500));
        assert!(matches!(g.state(), GraphicsState::Ready { .. }));
        assert_eq!(acts, vec![(0, Restart)]);
    }

    #[test]
    fn page_that_never_signals_ready_fails_after_three_timeouts_and_hides() {
        let th = t();
        let (g, acts) = run(th.cef_fail_window_ms - 1, None);
        let restarts = acts.iter().filter(|(_, a)| *a == Restart).count();
        let hides: Vec<GraphicsAction> = acts
            .iter()
            .filter(|(_, a)| matches!(a, Hide { .. }))
            .map(|&(_, a)| a)
            .collect();
        assert_eq!(
            restarts, th.cef_fail_count,
            "one restart per attempt before Failed"
        );
        let count = th.cef_fail_count as u64;
        assert_eq!(
            hides,
            vec![hide(Failed, count).unwrap()],
            "hide exactly once on entering Failed, naming the count-th generation"
        );
        assert!(matches!(g.state(), GraphicsState::Failed { .. }));
    }

    // Brief form: `on_tick(retry_at)` twice, the restart on the second. Here the cooldown's end IS the
    // restart (one rule, one tick): `retry_at - 1` does nothing and `retry_at` restarts.
    #[test]
    fn failed_retries_after_the_cooldown() {
        let th = t();
        let mut g = Graphics::new();
        let mut now = 0;
        for _ in 0..th.cef_fail_count {
            now = next_restart(&mut g, now, 100, &th);
            g.on_crashed(now + 10, g.generation(), &th);
            now += 10;
        }
        let GraphicsState::Failed { retry_at } = g.state() else {
            panic!("expected Failed, got {:?}", g.state())
        };
        assert_eq!(retry_at - now, th.cef_fail_window_ms);
        assert_eq!(g.on_tick(retry_at - 1, &th), None);
        assert_eq!(
            g.on_tick(retry_at, &th),
            Some(Restart),
            "the cooldown is over: restart"
        );
        assert_eq!(g.state(), GraphicsState::Starting { since: retry_at });
    }

    #[test]
    fn scheduled_recycle_is_a_restart_not_a_failure() {
        let th = t();
        let mut g = Graphics::new();
        assert_eq!(g.on_tick(0, &th), Some(Restart));
        g.on_ready(1_000, 1);
        // C05b: a silent Ready page hangs, so every page here beats; the domain keeps only the last
        // beat, so one 1 ms before the recycle stands for a page that beat throughout.
        g.on_alive(1_000 + th.cef_recycle_every_ms - 1, 1);
        assert_eq!(g.on_tick(1_000 + th.cef_recycle_every_ms, &th), Some(Restart));
        // three recycles never accumulate into Failed
        for k in 2..=4u64 {
            g.on_ready(1_000 + th.cef_recycle_every_ms * (k - 1) + 500, k);
            g.on_alive(1_000 + th.cef_recycle_every_ms * k + 499, k);
            let a = g.on_tick(1_000 + th.cef_recycle_every_ms * k + 500, &th);
            assert_eq!(a, Some(Restart));
        }
        assert!(!matches!(g.state(), GraphicsState::Failed { .. }));
    }

    #[test]
    fn stale_generation_crash_is_ignored() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        g.on_ready(500, 1);
        g.on_alive(500 + th.cef_recycle_every_ms - 1, 1); // C05b: it beats, so it reaches its recycle
        g.on_tick(500 + th.cef_recycle_every_ms, &th); // gen 2 starting
        assert_eq!(g.on_crashed(500 + th.cef_recycle_every_ms + 1, 1, &th), None);
        assert!(matches!(g.state(), GraphicsState::Starting { .. }));
    }

    #[test]
    fn crashes_spread_beyond_the_window_never_fail() {
        let th = t();
        let mut g = Graphics::new();
        let mut now = 0;
        for _ in 0..10 {
            now = next_restart(&mut g, now, 100, &th);
            g.on_ready(now + 100, g.generation());
            now += th.cef_fail_window_ms / 2 + 1; // fewer than 3 inside any 5-min window
            g.on_alive(now, g.generation()); // it beat up to its crash: its frame is fresh
            assert_eq!(g.on_crashed(now, g.generation(), &th), None);
        }
        assert!(!matches!(g.state(), GraphicsState::Failed { .. }));
    }

    // ---- boundaries: each timer at exactly its threshold, and 1 ms before ----

    // C06's first running tick sends `RestartGraphics { gen: 1 }`.
    #[test]
    fn the_first_tick_restarts_generation_one() {
        let th = t();
        let mut g = Graphics::new();
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Backoff { attempt: 0, until: 0 }, 0)
        );
        assert_eq!(g.on_tick(0, &th), Some(Restart));
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since: 0 }, 1)
        );
    }

    // A load is a failure at exactly `cef_load_timeout_ms` (1 ms earlier it is still loading), and the
    // first failure waits the back-off base and restarts at exactly its end.
    #[test]
    fn a_load_times_out_at_exactly_the_timeout_and_restarts_at_exactly_the_back_off_end() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        let timeout = th.cef_load_timeout_ms;
        assert_eq!(g.on_tick(timeout - 1, &th), None);
        assert_eq!(
            g.state(),
            GraphicsState::Starting { since: 0 },
            "1 ms before the timeout"
        );
        assert_eq!(
            g.on_tick(timeout, &th),
            None,
            "a first failure backs off; it does not hide"
        );
        let until = timeout + th.cef_backoff_base_ms;
        assert_eq!(g.state(), GraphicsState::Backoff { attempt: 0, until });
        assert_eq!(g.on_tick(until - 1, &th), None);
        assert_eq!(g.generation(), 1, "no restart 1 ms before the back-off ends");
        assert_eq!(g.on_tick(until, &th), Some(Restart));
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since: until }, 2)
        );
    }

    // F-001 measured cefsrc start → first buffer at 2.781–2.804 s; a page ready then is Ready, and
    // it is recycled at exactly `cef_recycle_every_ms` after its ready signal, not 1 ms before.
    #[test]
    fn a_ready_page_is_recycled_at_exactly_the_recycle_interval() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        g.on_ready(2_804, 1);
        assert_eq!(
            g.state(),
            GraphicsState::Ready {
                since: 2_804,
                last_alive: 2_804
            }
        );
        let due = 2_804 + th.cef_recycle_every_ms;
        // C05b: the page beats (a silent one hangs first); a beat moves the hang, never the recycle.
        g.on_alive(due - 1, 1);
        assert_eq!(g.on_tick(due - 1, &th), None);
        assert_eq!(
            g.state(),
            GraphicsState::Ready {
                since: 2_804,
                last_alive: due - 1
            }
        );
        assert_eq!(g.on_tick(due, &th), Some(Restart));
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since: due }, 2)
        );
    }

    // The brief's recycle test cannot see a recycle that records a failure: its recycles are 2 h
    // apart, so even counted they never meet inside a 5-min window. Here the recycled page fails
    // `cef_fail_count - 1` times right after the recycle: counted, the recycle would make the last of
    // them the `cef_fail_count`-th failure in the window and hide the overlay.
    #[test]
    fn a_recycle_records_no_failure_even_beside_fresh_failures() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        g.on_ready(2_804, 1);
        let beat = 2_804 + th.cef_recycle_every_ms - 1;
        g.on_alive(beat, 1); // C05b: a beating page reaches its recycle
        let mut now = next_restart(&mut g, beat, 1, &th); // the recycle
        for k in 0..th.cef_fail_count - 1 {
            let a = g.on_crashed(now + 1, g.generation(), &th);
            assert_eq!(a, None, "failure {} after the recycle", k + 1);
            assert_eq!(
                g.state(),
                GraphicsState::Backoff {
                    attempt: k as u32,
                    until: now + 1 + wait(&th, k)
                },
                "failure {} after the recycle is the {}-th in the window",
                k + 1,
                k + 1
            );
            now = next_restart(&mut g, now + 1, 1, &th);
        }
    }

    // Spec §4.2 "3 CEF failures in 5 min": the `cef_fail_count`-th failure exactly
    // `cef_fail_window_ms` after the first still counts (`Window`: a stamp exactly span old is inside)
    // and hides; 1 ms later the first has left the window, and it backs off instead.
    #[test]
    fn the_fail_count_inside_exactly_the_window_fails_and_one_ms_wider_does_not() {
        let th = t();
        let (count, span) = (th.cef_fail_count, th.cef_fail_window_ms);
        let (g, last) = crashes(&spread(count, span), &th);
        assert_eq!(
            last,
            hide(Failed, count as u64),
            "{count} failures exactly {span} ms apart"
        );
        assert_eq!(
            g.state(),
            GraphicsState::Failed {
                retry_at: span + span
            }
        );

        let mut wider = spread(count, span);
        *wider.last_mut().unwrap() += 1;
        let (g, last) = crashes(&wider, &th);
        assert_eq!(last, None, "{count} failures {} ms apart", span + 1);
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: (count - 2) as u32,
                until: span + 1 + wait(&th, count - 2)
            },
            "the first failure has left the window, so the last is number {}",
            count - 1
        );
    }

    #[test]
    fn one_failure_fewer_than_the_count_never_fails_however_close() {
        let th = t();
        let mut g = Graphics::new();
        let mut now = 0;
        for k in 0..th.cef_fail_count - 1 {
            now = next_restart(&mut g, now, 1, &th);
            assert_eq!(g.on_crashed(now, g.generation(), &th), None, "failure {}", k + 1);
        }
        assert!(
            matches!(g.state(), GraphicsState::Backoff { .. }),
            "{:?}",
            g.state()
        );
    }

    // The cooldown ends at exactly `retry_at`; until then Failed ignores everything: ticks, and the
    // hidden generation's own crash or late ready signal. One hide per entry, never a second.
    #[test]
    fn failed_hides_once_and_restarts_at_exactly_retry_at() {
        let th = t();
        let span = th.cef_fail_window_ms;
        let (mut g, last) = crashes(&spread(th.cef_fail_count, span), &th);
        let hidden = g.generation();
        assert_eq!(last, hide(Failed, hidden));
        let retry_at = span + span;
        for now in [span, span + 1, span + 33, retry_at - 1_000, retry_at - 1] {
            assert_eq!(g.on_tick(now, &th), None, "tick at {now}");
            assert_eq!(g.on_crashed(now, hidden, &th), None, "crash at {now}");
            g.on_ready(now, hidden);
            assert_eq!(g.state(), GraphicsState::Failed { retry_at }, "at {now}");
        }
        assert_eq!(g.on_tick(retry_at, &th), Some(Restart));
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since: retry_at }, hidden + 1)
        );
    }

    // ---- sequences ----

    // Failed is recoverable (C05 brief), and the recovery is real: after the cooldown the page loads,
    // goes Ready, and then needs the full `cef_fail_count` fresh failures to fail again. The second
    // Failed hides again, with its own cooldown.
    #[test]
    fn fail_then_recover_then_fail_again() {
        let th = t();
        let span = th.cef_fail_window_ms;
        let (mut g, last) = crashes(&spread(th.cef_fail_count, span), &th);
        assert_eq!(last, hide(Failed, th.cef_fail_count as u64), "first Failed");
        let retry_at = span + span;
        assert_eq!(g.on_tick(retry_at, &th), Some(Restart));
        g.on_ready(retry_at + 2_804, g.generation());
        assert!(matches!(g.state(), GraphicsState::Ready { .. }), "recovered");
        let mut now = retry_at + 60_000;
        for k in 0..th.cef_fail_count {
            g.on_alive(now, g.generation()); // each page beat up to its crash: its frame is fresh
            let a = g.on_crashed(now, g.generation(), &th);
            if k + 1 < th.cef_fail_count {
                assert_eq!(a, None, "failure {} after the recovery", k + 1);
                assert_eq!(
                    g.state(),
                    GraphicsState::Backoff {
                        attempt: k as u32,
                        until: now + wait(&th, k)
                    }
                );
                now = next_restart(&mut g, now, 1, &th);
                g.on_ready(now, g.generation());
                now += 1;
            } else {
                assert_eq!(a, hide(Failed, g.generation()), "the second Failed hides again");
                assert_eq!(g.state(), GraphicsState::Failed { retry_at: now + span });
            }
        }
    }

    // A retried page that never signals ready runs the same `cef_fail_count` timeouts again: every
    // cooldown is followed by a full set of attempts and one more hide. From a restart, the hide comes
    // `cef_fail_count` load timeouts plus the back-offs between them later (all multiples of the
    // 100 ms tick here, so exact), and the next restart a cooldown after the hide.
    #[test]
    fn a_page_that_never_recovers_hides_once_per_cooldown() {
        let th = t();
        let count = th.cef_fail_count;
        let to_hide = count as Ms * th.cef_load_timeout_ms + (0..count - 1).map(|k| wait(&th, k)).sum::<Ms>();
        let second = to_hide + th.cef_fail_window_ms + to_hide;
        let (_, acts) = run(second, None);
        let hides: Vec<(Ms, GraphicsAction)> = acts
            .iter()
            .filter(|(_, a)| matches!(a, Hide { .. }))
            .copied()
            .collect();
        let restarts = acts.iter().filter(|(_, a)| *a == Restart).count();
        let n = count as u64;
        let want = [(to_hide, n), (second, 2 * n)].map(|(at, gen)| (at, hide(Failed, gen).unwrap()));
        assert_eq!(hides, want, "{acts:?}");
        assert_eq!(restarts, 2 * count, "{acts:?}");
    }

    // A failure reported during the back-off (the timed-out or crashed page then crashes, or crashes
    // twice) is the same failure: not counted again, and the back-off does not move.
    #[test]
    fn a_failure_reported_during_the_back_off_is_not_counted_again() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        let timeout = th.cef_load_timeout_ms;
        assert_eq!(g.on_tick(timeout, &th), None, "the load times out");
        let backing_off = g.state();
        for now in [timeout, timeout + 1, timeout + th.cef_backoff_base_ms - 1] {
            assert_eq!(
                g.on_crashed(now, 1, &th),
                None,
                "the timed-out page crashes at {now}"
            );
            assert_eq!(g.state(), backing_off, "at {now}");
        }
        let restart = next_restart(&mut g, timeout, 1, &th);
        assert_eq!(g.on_crashed(restart, 2, &th), None);
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: 1,
                until: restart + wait(&th, 1)
            },
            "the second failure, not the fifth"
        );
    }

    // A ready from the recycled page (generation 1) or from one never built (3) leaves generation 2
    // loading: the recycled frame still comes off at its bound, and the load timeout still fires.
    #[test]
    fn a_ready_from_another_generation_does_not_make_the_page_ready() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        g.on_ready(100, 1);
        let since = 100 + th.cef_recycle_every_ms;
        g.on_alive(since - 1, 1); // C05b: it beats, so it reaches its recycle
        assert_eq!(g.on_tick(since, &th), Some(Restart));
        g.on_ready(since + 50, 1);
        g.on_ready(since + 60, 3);
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since }, 2)
        );
        assert_eq!(
            g.on_tick(since - 1 + th.cef_hang_after_ms, &th),
            hide(Recycle, 1),
            "no other generation's ready took over the air"
        );
        assert_eq!(g.on_tick(since + th.cef_load_timeout_ms, &th), None);
        assert!(
            matches!(g.state(), GraphicsState::Backoff { .. }),
            "the load timed out: {:?}",
            g.state()
        );
    }

    // A ready that arrives after the load timed out does not undo the failure: the restart goes ahead.
    #[test]
    fn a_ready_after_the_load_timed_out_is_ignored() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        g.on_tick(th.cef_load_timeout_ms, &th);
        let backing_off = g.state();
        g.on_ready(th.cef_load_timeout_ms + 1, 1);
        assert_eq!(g.state(), backing_off);
        let until = th.cef_load_timeout_ms + th.cef_backoff_base_ms;
        assert_eq!(g.on_tick(until, &th), Some(Restart));
    }

    // A page that logs `seazn:ready` twice must not push its recycle out: the clock runs from the first.
    #[test]
    fn a_second_ready_does_not_postpone_the_recycle() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        g.on_ready(100, 1);
        g.on_ready(60_000, 1);
        // Nor is a second ready a beat (C05b): only the first ready counts as one.
        assert_eq!(
            g.state(),
            GraphicsState::Ready {
                since: 100,
                last_alive: 100
            }
        );
        g.on_alive(100 + th.cef_recycle_every_ms - 1, 1); // a silent page would hang first
        assert_eq!(g.on_tick(100 + th.cef_recycle_every_ms, &th), Some(Restart));
    }

    // time.rs: the clock is a u64 and nothing wraps. Every deadline saturates at the top of the range.
    #[test]
    fn deadlines_saturate_at_the_top_of_the_clock() {
        let th = t();
        let top = Ms::MAX;
        let mut g = Graphics::new();
        assert_eq!(g.on_tick(top - 5, &th), Some(Restart));
        assert_eq!(g.on_crashed(top - 5, 1, &th), None);
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: 0,
                until: top
            }
        );
        for gen in 2..=th.cef_fail_count as u64 {
            assert_eq!(g.on_tick(top, &th), Some(Restart));
            let a = g.on_crashed(top, gen, &th);
            let at_count = gen == th.cef_fail_count as u64;
            assert_eq!(a, hide(Failed, gen).filter(|_| at_count), "generation {gen}");
        }
        assert_eq!(g.state(), GraphicsState::Failed { retry_at: top });
        assert_eq!(g.on_tick(top, &th), Some(Restart));
    }

    // Under `Thresholds::default()` the CEF numbers equal other links' (back-off 1 s → 10 s like the
    // input; 3 in 5 min like the kick alert), so reading the wrong field is invisible there. Here every
    // field the machine reads is distinct, and each expected value comes from that field.
    #[test]
    fn the_machine_reads_the_cef_thresholds() {
        let th = Thresholds {
            cef_fail_count: 4,
            cef_fail_window_ms: 77_000,
            cef_load_timeout_ms: 4_321,
            cef_recycle_every_ms: 600_000,
            cef_backoff_base_ms: 250,
            cef_backoff_cap_ms: 400,
            ..t()
        };
        let mut g = Graphics::new();
        assert_eq!(g.on_tick(0, &th), Some(Restart));
        assert_eq!(g.on_tick(4_320, &th), None);
        assert_eq!(g.on_tick(4_321, &th), None, "the load timeout");
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: 0,
                until: 4_321 + 250
            }
        );
        let mut now = 4_321 + 250;
        let mut waits = Vec::new();
        for gen in 2..=4 {
            assert_eq!(g.on_tick(now, &th), Some(Restart));
            let a = g.on_crashed(now, gen, &th);
            match g.state() {
                GraphicsState::Backoff { until, .. } => {
                    waits.push(until - now);
                    now = until;
                }
                GraphicsState::Failed { retry_at } => {
                    assert_eq!(
                        (a, gen, retry_at),
                        (hide(Failed, 4), 4, now + 77_000),
                        "the 4th failure"
                    );
                    now = retry_at;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(waits, vec![400, 400], "250 doubled is 500, capped at 400");
        assert_eq!(g.on_tick(now - 1, &th), None);
        assert_eq!(g.on_tick(now, &th), Some(Restart), "the cooldown is the window");
        g.on_ready(now, g.generation());
        let recycle = now + 600_000;
        g.on_alive(recycle - 1, g.generation()); // C05b: a beating page reaches its recycle
        assert_eq!(g.on_tick(recycle - 1, &th), None);
        assert_eq!(g.on_tick(recycle, &th), Some(Restart), "the recycle interval");
        // Two crashes 77_001 ms apart: the first has left a 77_000 ms window, so each waits the base.
        g.on_crashed(recycle, g.generation(), &th);
        let again = recycle + 77_001;
        assert_eq!(g.on_tick(recycle + 250, &th), Some(Restart));
        g.on_crashed(again, g.generation(), &th);
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: 0,
                until: again + 250
            }
        );
    }

    // ---- C05b: hang detection from the page's alive beat (docs/_INDEX.md Owed 18) ----

    /// Generation 1, Ready at `r = cef_load_timeout_ms / 2` (C05b Step 2: never at 0, so a `last_alive`
    /// left at 0 would hang at once).
    fn ready_at(th: &Thresholds) -> (Graphics, Ms) {
        let r = th.cef_load_timeout_ms / 2;
        assert!(r > 0, "a Ready at 0 cannot tell last_alive from 0");
        let mut g = Graphics::new();
        assert_eq!(g.on_tick(0, th), Some(Restart));
        g.on_ready(r, 1);
        assert_eq!(
            g.state(),
            GraphicsState::Ready {
                since: r,
                last_alive: r
            },
            "the ready counts as the first beat"
        );
        (g, r)
    }

    // A page that beats every `cef_alive_every_ms` never hangs: over three recycles nothing fails and
    // the only actions are the planned recycles, each exactly `cef_recycle_every_ms` after its page's
    // ready, so the beats never move the recycle either. Each recycled page loads in `r` and beats.
    #[test]
    fn a_beating_page_never_hangs() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let (beat, step) = (th.cef_alive_every_ms, 500);
        assert!(
            beat.is_multiple_of(step) && r.is_multiple_of(step),
            "the grid hits every beat and ready"
        );
        let mut ready = Some(r);
        let mut want = vec![r + th.cef_recycle_every_ms];
        let mut acts = Vec::new();
        let mut now = r;
        while acts.len() < 3 {
            now += step;
            assert!(
                now <= r + 4 * th.cef_recycle_every_ms,
                "no third recycle by {now}"
            );
            match ready {
                Some(at) if (now - at).is_multiple_of(beat) => g.on_alive(now, g.generation()),
                Some(_) => {}
                None => {
                    // The recycled page loads in `r`, then beats from its own ready.
                    if now >= acts.last().map_or(0, |&(at, _)| at) + r {
                        g.on_ready(now, g.generation());
                        ready = Some(now);
                        want.push(now + th.cef_recycle_every_ms);
                    }
                }
            }
            if let Some(a) = g.on_tick(now, &th) {
                acts.push((now, a));
                ready = None;
            }
            assert!(
                !matches!(
                    g.state(),
                    GraphicsState::Backoff { .. } | GraphicsState::Failed { .. }
                ),
                "a beating page failed at {now}: {:?}",
                g.state()
            );
        }
        want.truncate(3);
        let restarts: Vec<(Ms, GraphicsAction)> = want.iter().map(|&at| (at, Restart)).collect();
        assert_eq!(acts, restarts, "only the planned recycles, each on time");
        assert_eq!(g.generation(), 4);
    }

    // Owed 18: a Ready page silent for `cef_hang_after_ms` has hung: at exactly the threshold the tick
    // hides at once and backs off as a first failure (generation unchanged); 1 ms earlier it is quiet.
    // The restart comes one `cef_backoff_base_ms` later, not 1 ms before (C06 PR #21 review I2).
    #[test]
    fn a_silent_ready_page_hangs_and_hides_at_exactly_the_threshold() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let hang = r + th.cef_hang_after_ms;
        assert_eq!(g.on_tick(hang - 1, &th), None, "1 ms before the threshold");
        assert_eq!(
            g.state(),
            GraphicsState::Ready {
                since: r,
                last_alive: r
            }
        );
        assert_eq!(g.on_tick(hang, &th), hide(Hang, 1), "hidden at once");
        let until = hang + wait(&th, 0);
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Backoff { attempt: 0, until }, 1)
        );
        assert_eq!(g.on_tick(until - 1, &th), None);
        assert_eq!(g.generation(), 1, "no restart 1 ms before the back-off ends");
        assert_eq!(g.on_tick(until, &th), Some(Restart));
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since: until }, 2)
        );
    }

    // The threshold is "at or after", not "exactly": a first tick 1 ms past it, or a minute past it (a
    // stalled loop), still hangs, and the back-off runs from that tick.
    #[test]
    fn a_first_tick_past_the_threshold_still_hangs() {
        let th = t();
        for late in [1, 60_000] {
            let (mut g, r) = ready_at(&th);
            let at = r + th.cef_hang_after_ms + late;
            assert_eq!(g.on_tick(at, &th), hide(Hang, 1), "{late} ms past the threshold");
            assert_eq!(
                g.state(),
                GraphicsState::Backoff {
                    attempt: 0,
                    until: at + wait(&th, 0)
                }
            );
        }
    }

    // Each beat moves the deadline to `cef_hang_after_ms` after it; the ready's own deadline passes
    // quietly once a beat has come, and the beat changes nothing else.
    #[test]
    fn the_last_beat_moves_the_deadline() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let beat = th.cef_alive_every_ms;
        for k in 1..=2 {
            g.on_alive(r + k * beat, 1);
            assert_eq!(
                g.state(),
                GraphicsState::Ready {
                    since: r,
                    last_alive: r + k * beat
                },
                "beat {k}"
            );
        }
        assert_eq!(
            g.on_tick(r + th.cef_hang_after_ms, &th),
            None,
            "the ready's deadline: moved"
        );
        let hang = r + 2 * beat + th.cef_hang_after_ms;
        assert_eq!(
            g.on_tick(hang - 1, &th),
            None,
            "1 ms before the last beat's deadline"
        );
        assert_eq!(g.on_tick(hang, &th), hide(Hang, 1), "the last beat's deadline");
    }

    // Spec §4.2 "3 in 5 min", through hangs alone: every hang hides at once, below the count as well
    // as at it, and the `cef_fail_count`-th enters Failed. Each retried page loads in `r`.
    #[test]
    fn three_hangs_in_the_window_fail_and_hide() {
        let th = t();
        let count = th.cef_fail_count;
        let (mut g, r) = ready_at(&th);
        let (mut ready, mut hangs) = (r, Vec::new());
        for k in 0..count {
            let hang = ready + th.cef_hang_after_ms;
            assert_eq!(g.on_tick(hang - 1, &th), None, "hang {}: 1 ms early", k + 1);
            // The reason names what took the frame off air: a hang, at the count too.
            let gen = k as u64 + 1;
            assert_eq!(g.on_tick(hang, &th), hide(Hang, gen), "hang {} hides", k + 1);
            hangs.push(hang);
            if k + 1 < count {
                let until = hang + wait(&th, k);
                let attempt = k as u32;
                assert_eq!(g.state(), GraphicsState::Backoff { attempt, until });
                assert_eq!(g.on_tick(until, &th), Some(Restart));
                ready = until + r;
                g.on_ready(ready, g.generation());
            } else {
                let retry_at = hang + th.cef_fail_window_ms;
                assert_eq!(g.state(), GraphicsState::Failed { retry_at }, "the count-th hang");
            }
        }
        assert_eq!(hangs.len(), count);
        assert!(
            hangs[count - 1] - hangs[0] <= th.cef_fail_window_ms,
            "all inside one window: {hangs:?}"
        );
    }

    // A hang and a crash are the same kind of failure in the same window. Every placement of one hang
    // among `cef_fail_count` failures (crashes 1 ms after their ready): a crash below the count holds
    // the frame (`None`), the hang hides wherever it falls, and the count-th enters Failed.
    #[test]
    fn a_hang_and_a_crash_share_the_window() {
        let th = t();
        let count = th.cef_fail_count;
        let mut checked = 0;
        for hang_at in 0..count {
            let (mut g, r) = ready_at(&th);
            let mut ready = r;
            for k in 0..count {
                let (at, a) = if k == hang_at {
                    let at = ready + th.cef_hang_after_ms;
                    (at, g.on_tick(at, &th))
                } else {
                    (ready + 1, g.on_crashed(ready + 1, g.generation(), &th))
                };
                let last = k + 1 == count;
                let gen = k as u64 + 1;
                let want = match (k == hang_at, last) {
                    (true, _) => hide(Hang, gen),
                    (false, true) => hide(Failed, gen),
                    (false, false) => None,
                };
                assert_eq!(a, want, "hang at {hang_at}: failure {}", k + 1);
                if last {
                    let retry_at = at + th.cef_fail_window_ms;
                    assert_eq!(g.state(), GraphicsState::Failed { retry_at }, "hang at {hang_at}");
                } else {
                    let until = at + wait(&th, k);
                    let attempt = k as u32;
                    assert_eq!(g.state(), GraphicsState::Backoff { attempt, until });
                    assert_eq!(g.on_tick(until, &th), Some(Restart));
                    ready = until + r;
                    g.on_ready(ready, g.generation());
                }
            }
            checked += 1;
        }
        assert_eq!(checked, count, "every placement of the hang");
    }

    // The hang and the recycle fall due on the same tick: the hang wins. It is a counted failure plus
    // a hide, never laundered as a planned restart, or `cef_fail_count` hangs at recycles would never
    // reach Failed. Counted: the next `cef_fail_count - 1` crashes enter Failed at the count.
    #[test]
    fn the_hang_wins_over_a_due_recycle() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let recycle = r + th.cef_recycle_every_ms;
        g.on_alive(recycle - th.cef_hang_after_ms, 1);
        assert_eq!(g.on_tick(recycle - 1, &th), None, "neither is due 1 ms early");
        assert_eq!(
            g.on_tick(recycle, &th),
            hide(Hang, 1),
            "the hang, not the recycle"
        );
        let until = recycle + wait(&th, 0);
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Backoff { attempt: 0, until }, 1)
        );
        let mut now = until;
        for k in 1..th.cef_fail_count {
            assert_eq!(g.on_tick(now, &th), Some(Restart));
            let a = g.on_crashed(now, g.generation(), &th);
            if k + 1 < th.cef_fail_count {
                assert_eq!(a, None, "failure {}", k + 1);
                now += wait(&th, k);
            } else {
                assert_eq!(
                    a,
                    hide(Failed, g.generation()),
                    "the hang counted: this crash is the count-th"
                );
                assert!(matches!(g.state(), GraphicsState::Failed { .. }));
            }
        }
    }

    // A beat counts only from the current generation's Ready page. From the previous and the next
    // generation while Ready, and from any of the three while Starting, backing off or Failed, it
    // changes nothing: not the state, the generation or the failure window (the whole machine is
    // compared). The page beats only after ready, and a beat never makes a page Ready.
    #[test]
    fn beats_from_other_generations_and_states_change_nothing() {
        let th = t();
        // Generation 2 in each state: generation 1 crashed once, 10 ms after its restart.
        let at_gen_2 = || {
            let mut g = Graphics::new();
            g.on_tick(0, &th);
            g.on_crashed(10, 1, &th);
            let until = 10 + wait(&th, 0);
            assert_eq!(g.on_tick(until, &th), Some(Restart));
            (g, until)
        };
        let mut machines = Vec::new();
        let (starting, s_at) = at_gen_2();
        machines.push(("Starting", starting.clone(), s_at, vec![1, 2, 3]));
        let mut ready = starting.clone();
        ready.on_ready(s_at + 100, 2);
        machines.push(("Ready", ready.clone(), s_at + 100, vec![1, 3]));
        let mut backoff = ready.clone();
        assert_eq!(backoff.on_crashed(s_at + 200, 2, &th), None);
        machines.push(("Backoff", backoff, s_at + 200, vec![1, 2, 3]));
        let span = th.cef_fail_window_ms;
        let (failed, _) = crashes(&spread(th.cef_fail_count, span), &th);
        let hidden = failed.generation();
        machines.push(("Failed", failed, span, vec![hidden - 1, hidden, hidden + 1]));
        let mut checked = 0;
        for (name, before, entered, gens) in machines {
            for gen in gens {
                for at in [entered, entered + 1, entered + th.cef_hang_after_ms - 1] {
                    let mut g = before.clone();
                    g.on_alive(at, gen);
                    assert_eq!(g, before, "{name}: a beat from generation {gen} at {at}");
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked,
            (3 + 2 + 3 + 3) * 3,
            "every case, and 0 checked is a failure"
        );
    }

    // A page that hangs while loading never signals ready, so it never beats either (R2005 beats
    // only after ready): it is a load timeout, at exactly `cef_load_timeout_ms` after its restart
    // whatever it logged, and below the count it holds the frame like any load timeout, no hide.
    #[test]
    fn a_page_that_hangs_while_loading_is_a_load_timeout_not_a_hide() {
        let th = t();
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        let timeout = th.cef_load_timeout_ms;
        let mut now = 0;
        while now < timeout {
            g.on_alive(now, 1); // a stray beat while loading is no ready and moves nothing
            assert_eq!(g.on_tick(now, &th), None, "at {now}");
            assert_eq!(g.state(), GraphicsState::Starting { since: 0 }, "at {now}");
            now += th.cef_alive_every_ms / 4;
        }
        assert_eq!(g.on_tick(timeout - 1, &th), None);
        assert_eq!(
            g.on_tick(timeout, &th),
            None,
            "a load timeout below the count holds the frame"
        );
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: 0,
                until: timeout + wait(&th, 0)
            }
        );
    }

    // Hang → hide → restart → a reload that hangs while loading → restart → Ready, beating. The hide
    // came once, at the hang: the reload's load timeout below the count adds no second hide (the
    // first still covers it), and the next generation's Ready lifts it (C11 writes the slot only
    // from a generation that signalled ready). Then the page beats for a minute and nothing acts.
    #[test]
    fn a_hang_hides_until_the_next_generation_is_ready() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let hang = r + th.cef_hang_after_ms;
        let mut acts = vec![(hang, g.on_tick(hang, &th))];
        let first = hang + wait(&th, 0);
        acts.push((first, g.on_tick(first, &th)));
        let timeout = first + th.cef_load_timeout_ms;
        acts.push((timeout, g.on_tick(timeout, &th)));
        let second = timeout + wait(&th, 1);
        acts.push((second, g.on_tick(second, &th)));
        assert_eq!(
            acts,
            vec![
                (hang, hide(Hang, 1)),
                (first, Some(Restart)),
                (timeout, None),
                (second, Some(Restart))
            ]
        );
        let shown = second + r;
        g.on_ready(shown, 3);
        assert_eq!(
            (g.state(), g.generation()),
            (
                GraphicsState::Ready {
                    since: shown,
                    last_alive: shown
                },
                3
            ),
            "the next generation is Ready: the hide lifts"
        );
        let mut now = shown;
        while now < shown + 60_000 {
            now += th.cef_alive_every_ms;
            g.on_alive(now, 3);
            assert_eq!(g.on_tick(now, &th), None, "a beating page at {now}");
        }
    }

    // A beat that comes after the hang was declared (the hung page wakes, or a beat still queued in the
    // log) changes nothing: the back-off and its restart stand; in the reload's Starting it is a stale
    // generation's; and in Failed the cooldown stands until `retry_at`.
    #[test]
    fn a_beat_after_the_hang_is_declared_changes_nothing() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let hang = r + th.cef_hang_after_ms;
        assert_eq!(g.on_tick(hang, &th), hide(Hang, 1));
        let backing_off = g.state();
        let until = hang + wait(&th, 0);
        for at in [hang, hang + 1, until - 1] {
            g.on_alive(at, 1);
            assert_eq!(g.state(), backing_off, "a late beat at {at}");
        }
        assert_eq!(g.on_tick(until, &th), Some(Restart), "the restart stands");
        g.on_alive(until + 1, 1);
        assert_eq!(
            g.state(),
            GraphicsState::Starting { since: until },
            "the hung generation's beat"
        );
        // Two more hangs: Failed, then late beats from the hidden page through the cooldown.
        let mut ready = until + r;
        g.on_ready(ready, 2);
        for k in 1..th.cef_fail_count {
            let hang = ready + th.cef_hang_after_ms;
            assert_eq!(g.on_tick(hang, &th), hide(Hang, g.generation()), "hang {}", k + 1);
            if k + 1 < th.cef_fail_count {
                let until = hang + wait(&th, k);
                assert_eq!(g.on_tick(until, &th), Some(Restart));
                ready = until + r;
                g.on_ready(ready, g.generation());
            }
        }
        let GraphicsState::Failed { retry_at } = g.state() else {
            panic!("expected Failed, got {:?}", g.state())
        };
        let hidden = g.generation();
        for at in [retry_at - th.cef_fail_window_ms, retry_at - 1] {
            g.on_alive(at, hidden);
            assert_eq!(g.on_tick(at, &th), None, "the cooldown at {at}");
            assert_eq!(g.state(), GraphicsState::Failed { retry_at });
        }
        assert_eq!(g.on_tick(retry_at, &th), Some(Restart));
    }

    // Under `Thresholds::default()` the hang threshold equals the load timeout (both 10 s), so reading
    // the wrong field is invisible there. Here they differ, and so does the beat.
    #[test]
    fn the_hang_reads_cef_hang_after_ms() {
        let th = Thresholds {
            cef_hang_after_ms: 7_777,
            cef_load_timeout_ms: 4_321,
            cef_alive_every_ms: 1_111,
            cef_backoff_base_ms: 250,
            cef_backoff_cap_ms: 400,
            ..t()
        };
        // Every expected instant reads the field it tests, so editing an override keeps it honest;
        // the guard keeps the load timeout a distinct, earlier instant the hang must not fire at.
        assert!(
            th.cef_load_timeout_ms < th.cef_hang_after_ms,
            "the overrides must differ"
        );
        let (mut g, r) = ready_at(&th);
        let hang = r + th.cef_hang_after_ms;
        assert_eq!(
            g.on_tick(r + th.cef_load_timeout_ms, &th),
            None,
            "not the load timeout"
        );
        assert_eq!(g.on_tick(hang - 1, &th), None);
        assert_eq!(g.on_tick(hang, &th), hide(Hang, 1), "the hang threshold");
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: 0,
                until: hang + th.cef_backoff_base_ms
            }
        );
    }

    // ---- the one bound: the frame on air stays at most `cef_hang_after_ms` after the last proof of
    // life (the ready, or a beat) of the page that produced it (coordinator decision 2026-10-18,
    // PR #22 review G1, generalising the owner-approved 10 s rule). A hang, a crash and a recycle all
    // fall under it, and the load timeouts of their reloads neither end it nor move it ----

    /// The ways a Ready page's life ends with its last frame still on air while a reload loads.
    const ENDINGS: [HideReason; 2] = [Crash, Recycle];

    /// Generation 1 Ready at `r` (the ready is its last proof of life), crashed at
    /// `c = r + cef_alive_every_ms - 1`, just before its next beat would have come: a first failure,
    /// so it backs off and its frame stays on air. The crash is late, so a bound timed from the crash
    /// would differ from one timed from the proof by almost a beat. Returns the machine, `r` and `c`.
    fn crashed_ready(th: &Thresholds) -> (Graphics, Ms, Ms) {
        let (mut g, r) = ready_at(th);
        let c = r + th.cef_alive_every_ms - 1;
        assert!(
            c > r && c < r + th.cef_hang_after_ms,
            "the crash lands after the proof and inside the bound"
        );
        assert_eq!(
            g.on_crashed(c, 1, th),
            None,
            "a first crash backs off; its frame stays on air"
        );
        assert_eq!(
            g.state(),
            GraphicsState::Backoff {
                attempt: 0,
                until: c + wait(th, 0)
            }
        );
        (g, r, c)
    }

    /// Generation 1 Ready at `r`, its last beat `gap` ms before its recycle: the recycle restarts
    /// generation 2, and the recycled page's last frame stays on air while that loads. Returns the
    /// machine, the last beat and the recycle's instant (the reload's restart).
    fn recycled(th: &Thresholds, gap: Ms) -> (Graphics, Ms, Ms) {
        let (mut g, r) = ready_at(th);
        let recycle = r + th.cef_recycle_every_ms;
        let beat = recycle - gap;
        assert!(
            beat > r && gap < th.cef_hang_after_ms,
            "a beating page reaches its recycle"
        );
        g.on_alive(beat, 1);
        assert_eq!(g.on_tick(recycle, th), Some(Restart), "the recycle");
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since: recycle }, 2)
        );
        (g, beat, recycle)
    }

    /// Generation 1's life ended by `ending` with its frame on air, and generation 2 loading: the
    /// crash of `crashed_ready` and its reload's restart, or `recycled` a beat after the last beat.
    /// Returns the machine, generation 1's last proof of life and the reload's restart.
    fn reloading_after(ending: HideReason, th: &Thresholds) -> (Graphics, Ms, Ms) {
        match ending {
            Crash => {
                let (mut g, r, c) = crashed_ready(th);
                let restart = c + wait(th, 0);
                assert_eq!(g.on_tick(restart, th), Some(Restart), "the reload");
                (g, r, restart)
            }
            Recycle => recycled(th, th.cef_alive_every_ms),
            other => panic!("a {other:?} hide does not leave a frame on air through a reload"),
        }
    }

    // The crashed page's frame stays on air through the reload for exactly `cef_hang_after_ms` after
    // its last proof of life (here its ready), not after the crash: 1 ms before, nothing; at it, a
    // `Crash` hide naming the crashed generation, while the reload (generation 2) goes on loading;
    // then nothing more.
    #[test]
    fn a_crash_holds_the_frame_until_exactly_the_bound() {
        let th = t();
        let (mut g, r, c) = crashed_ready(&th);
        let restart = c + wait(&th, 0);
        assert_eq!(g.on_tick(restart, &th), Some(Restart), "the reload");
        let off = r + th.cef_hang_after_ms;
        assert!(
            off < c + th.cef_hang_after_ms,
            "a bound from the crash would be later"
        );
        assert!(
            off < restart + th.cef_load_timeout_ms,
            "the bound ends before the reload's own load timeout"
        );
        assert_eq!(g.on_tick(off - 1, &th), None, "1 ms before the bound");
        assert_eq!(
            g.on_tick(off, &th),
            hide(Crash, 1),
            "the held frame comes off air"
        );
        assert_eq!(
            (g.state(), g.generation()),
            (GraphicsState::Starting { since: restart }, 2),
            "a crash hide does not touch the reload"
        );
        assert_eq!(g.on_tick(off + 1, &th), None, "one hide, not one per tick");
    }

    // PR #22 review G1: a recycle's reload that never signals ready held the recycled page's last
    // frame ~33 s, through every load timeout to Failed. Now it stays on air for exactly
    // `cef_hang_after_ms` after that page's last beat, and a `Recycle` hide names it while the reload
    // loads on. With the last beat on the recycle's own tick the bound and the reload's load timeout
    // fall due together (both 10 s by default): the hide wins that tick, and the timeout, a failure
    // with nothing left to hide, follows on the next.
    #[test]
    fn a_recycle_whose_reload_never_readies_hides_at_exactly_the_bound() {
        let th = t();
        let mut checked = 0;
        for gap in [th.cef_alive_every_ms, 1, 0] {
            let (mut g, beat, recycle) = recycled(&th, gap);
            let (off, timeout) = (beat + th.cef_hang_after_ms, recycle + th.cef_load_timeout_ms);
            assert!(off <= timeout, "gap {gap}: the bound comes first or with it");
            assert!(
                off < recycle + th.cef_hang_after_ms || gap == 0,
                "gap {gap}: a bound from the recycle would be later"
            );
            assert_eq!(g.on_tick(off - 1, &th), None, "gap {gap}: 1 ms before the bound");
            assert_eq!(
                g.on_tick(off, &th),
                hide(Recycle, 1),
                "gap {gap}: the recycled frame comes off air"
            );
            assert_eq!(
                (g.state(), g.generation()),
                (GraphicsState::Starting { since: recycle }, 2),
                "gap {gap}: the reload loads on"
            );
            assert_eq!(g.on_tick(timeout, &th), None, "gap {gap}: the load timeout");
            assert_eq!(
                g.state(),
                GraphicsState::Backoff {
                    attempt: 0,
                    until: timeout + wait(&th, 0)
                },
                "gap {gap}: one failure, and nothing left on air to hide"
            );
            checked += 1;
        }
        assert_eq!(checked, 3);
    }

    // The load timeouts of a crash's or a recycle's reloads neither end the bound nor move it: with a
    // load timeout well under `cef_hang_after_ms`, two reloads time out and restart while the frame
    // stays on air, and it comes off at exactly its page's last proof of life plus the bound, naming
    // the path that started it and that page, never a load timeout and never the reload.
    #[test]
    fn reloads_that_time_out_never_end_or_move_the_bound() {
        let th = Thresholds {
            cef_load_timeout_ms: 3_000,
            cef_backoff_base_ms: 250,
            cef_backoff_cap_ms: 400,
            cef_fail_count: 5,
            ..t()
        };
        assert!(
            2 * (th.cef_load_timeout_ms + th.cef_backoff_cap_ms)
                < th.cef_hang_after_ms - th.cef_alive_every_ms,
            "two reloads time out inside the bound"
        );
        let mut checked = 0;
        for ending in ENDINGS {
            let (mut g, proof, restart) = reloading_after(ending, &th);
            let off = proof + th.cef_hang_after_ms;
            let mut timeouts = 0;
            for now in restart + 1..off {
                let loading = matches!(g.state(), GraphicsState::Starting { .. });
                let a = g.on_tick(now, &th);
                assert!(
                    a.is_none() || a == Some(Restart),
                    "{ending:?}: {a:?} at {now}, before the bound"
                );
                timeouts += usize::from(loading && matches!(g.state(), GraphicsState::Backoff { .. }));
            }
            assert!(
                timeouts >= 2,
                "{ending:?}: {timeouts} load timeouts inside the bound"
            );
            assert!(g.generation() > 2, "{ending:?}: the reloads restarted");
            assert_eq!(
                g.on_tick(off, &th),
                hide(ending, 1),
                "{ending:?}: off at the page's proof plus the bound"
            );
            checked += 1;
        }
        assert_eq!(checked, ENDINGS.len());
    }

    // A first tick past the bound still hides, 1 ms past it and when the reload's own load timeout
    // falls due on the same tick. The hide wins that tick, and the load timeout is recorded on the
    // next one.
    #[test]
    fn a_first_tick_past_the_bound_still_hides() {
        let th = t();
        let mut checked = 0;
        for ending in ENDINGS {
            let (_, proof, restart) = reloading_after(ending, &th);
            let both = restart + th.cef_load_timeout_ms - (proof + th.cef_hang_after_ms);
            for late in [1, both] {
                let (mut g, _, _) = reloading_after(ending, &th);
                let at = proof + th.cef_hang_after_ms + late;
                assert_eq!(
                    g.on_tick(at, &th),
                    hide(ending, 1),
                    "{ending:?}: {late} ms past the bound"
                );
                assert_eq!(g.state(), GraphicsState::Starting { since: restart });
                if at >= restart + th.cef_load_timeout_ms {
                    assert_eq!(g.on_tick(at, &th), None, "the load timeout, on the next tick");
                    assert!(matches!(g.state(), GraphicsState::Backoff { .. }));
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 2 * ENDINGS.len());
    }

    // The reload goes Ready inside the bound, on the tick of its restart, 1 ms before the bound, or
    // on its very instant (the ready before the tick): no hide, then or later, while it beats. A
    // Ready on that instant but after the tick comes after the hide, and lifts it: that page's own
    // frames are on air, so its own crash later holds and hides again, naming it, from its own ready.
    #[test]
    fn a_reload_ready_inside_the_bound_never_hides() {
        let th = t();
        let mut checked = 0;
        for ending in ENDINGS {
            let (_, proof, restart) = reloading_after(ending, &th);
            let off = proof + th.cef_hang_after_ms;
            for ready in [restart, off - 1, off] {
                let (mut g, _, _) = reloading_after(ending, &th);
                g.on_ready(ready, 2);
                assert!(matches!(g.state(), GraphicsState::Ready { .. }), "{ready}");
                assert_eq!(
                    g.on_tick(off, &th),
                    None,
                    "{ending:?}: Ready {} ms before the bound",
                    off - ready
                );
                let mut now = ready;
                while now < off + 60_000 {
                    now += th.cef_alive_every_ms;
                    g.on_alive(now, 2);
                    assert_eq!(g.on_tick(now, &th), None, "{ending:?}: a beating reload at {now}");
                }
                checked += 1;
            }

            let (mut g, _, _) = reloading_after(ending, &th);
            assert_eq!(g.on_tick(off, &th), hide(ending, 1));
            g.on_ready(off, 2);
            assert!(
                matches!(g.state(), GraphicsState::Ready { .. }),
                "late, but Ready"
            );
            let again = off + 1;
            assert_eq!(g.on_crashed(again, 2, &th), None, "a failure below the count");
            let back = match g.state() {
                GraphicsState::Backoff { until, .. } => until,
                other => panic!("{ending:?}: expected Backoff, got {other:?}"),
            };
            assert_eq!(g.on_tick(back, &th), Some(Restart));
            let own = off + th.cef_hang_after_ms;
            assert_eq!(g.on_tick(own - 1, &th), None, "{ending:?}: from the late Ready");
            assert_eq!(
                g.on_tick(own, &th),
                hide(Crash, 2),
                "{ending:?}: the late Ready lifted the hide; its page's frame was on air, and held"
            );
            checked += 1;
        }
        assert_eq!(checked, 4 * ENDINGS.len());
    }

    // A reload that crashes while loading never showed a frame of its own, so the frame on air keeps
    // its page, its bound and its cause: a crash's reload crashing still hides as `Crash`, and a
    // recycle's as `Recycle`, both naming generation 1 at its proof plus the bound. A crash reported
    // from the generation that already ended, or again while backing off, changes nothing.
    #[test]
    fn a_reload_crash_keeps_the_frames_page_bound_and_cause() {
        let th = t();
        let mut checked = 0;
        for ending in ENDINGS {
            let (mut g, proof, restart) = reloading_after(ending, &th);
            let before = g.clone();
            assert_eq!(g.on_crashed(restart + 1, 1, &th), None);
            assert_eq!(g, before, "{ending:?}: generation 1's crash, reported again");
            let crash = restart + 2;
            assert_eq!(
                g.on_crashed(crash, 2, &th),
                None,
                "{ending:?}: the reload's crash"
            );
            let backing_off = g.clone();
            assert_eq!(g.on_crashed(crash + 1, 2, &th), None);
            assert_eq!(g, backing_off, "{ending:?}: reported again while backing off");
            let back = match g.state() {
                GraphicsState::Backoff { until, .. } => until,
                other => panic!("{ending:?}: expected Backoff, got {other:?}"),
            };
            assert_eq!(g.on_tick(back, &th), Some(Restart));
            let off = proof + th.cef_hang_after_ms;
            assert!(
                off < crash + th.cef_hang_after_ms,
                "a bound from the reload's crash would be later"
            );
            assert_eq!(g.on_tick(off - 1, &th), None);
            assert_eq!(
                g.on_tick(off, &th),
                hide(ending, 1),
                "{ending:?}: the first page, its bound, its cause"
            );
            assert_eq!(g.generation(), 3);
            checked += 1;
        }
        assert_eq!(checked, ENDINGS.len());
    }

    // Nothing is on air before the first Ready, and none from a hide until the next Ready, so a crash
    // then holds nothing and no bound hides anything: one hide per frame.
    #[test]
    fn with_nothing_on_air_no_bound_hides() {
        let th = t();
        // Generation 1 never signalled ready: nothing on air.
        let mut g = Graphics::new();
        g.on_tick(0, &th);
        assert_eq!(g.on_crashed(10, 1, &th), None);
        assert_eq!(g.on_tick(10 + wait(&th, 0), &th), Some(Restart));
        assert_eq!(
            g.on_tick(10 + th.cef_hang_after_ms, &th),
            None,
            "nothing was on air"
        );

        // A hang hid generation 1; its reload crashes while loading: already off air.
        let (mut g, r) = ready_at(&th);
        let hang = r + th.cef_hang_after_ms;
        assert_eq!(g.on_tick(hang, &th), hide(Hang, 1));
        let first = hang + wait(&th, 0);
        assert_eq!(g.on_tick(first, &th), Some(Restart));
        assert_eq!(g.on_crashed(first + 1, 2, &th), None);
        assert_eq!(g.on_tick(first + 1 + wait(&th, 1), &th), Some(Restart));
        for at in [hang + th.cef_hang_after_ms, first + 1 + th.cef_hang_after_ms] {
            assert_eq!(
                g.on_tick(at, &th),
                None,
                "the hang's hide still covers it at {at}"
            );
        }

        // A crash or recycle hide, then the reload crashes while loading: off air stays off.
        for ending in ENDINGS {
            let (mut g, proof, restart) = reloading_after(ending, &th);
            let off = proof + th.cef_hang_after_ms;
            assert_eq!(g.on_tick(off, &th), hide(ending, 1));
            assert!(off < restart + th.cef_load_timeout_ms);
            assert_eq!(g.on_crashed(off + 1, 2, &th), None);
            let back = match g.state() {
                GraphicsState::Backoff { until, .. } => until,
                other => panic!("{ending:?}: expected Backoff, got {other:?}"),
            };
            assert_eq!(g.on_tick(back, &th), Some(Restart));
            assert_eq!(
                g.on_tick(off + 1 + th.cef_hang_after_ms, &th),
                None,
                "{ending:?}: hidden once"
            );
        }
    }

    // A Ready page's crash reported past its bound but before the tick that would have declared it
    // hung is that crash and not a hang too: one failure (attempt 0, not 1), and the next tick hides
    // as `Crash`, counting nothing more.
    #[test]
    fn a_crash_reported_past_the_bound_is_one_failure_hidden_as_a_crash() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let at = r + th.cef_hang_after_ms + 5;
        assert_eq!(g.on_crashed(at, 1, &th), None);
        let backing_off = GraphicsState::Backoff {
            attempt: 0,
            until: at + wait(&th, 0),
        };
        assert_eq!(g.state(), backing_off);
        assert_eq!(g.on_tick(at, &th), hide(Crash, 1));
        assert_eq!(g.state(), backing_off, "the hide counted nothing");
    }

    // A crash that is the count-th failure hides at once as `Failed`, and no bound hide follows,
    // whether a frame was on air or not: entering Failed takes the frame off air.
    #[test]
    fn a_crash_at_the_count_hides_once_as_failed() {
        let th = t();
        let (mut g, r, c) = crashed_ready(&th);
        let mut now = c;
        for gen in 2..=th.cef_fail_count as u64 {
            now += wait(&th, gen as usize - 2);
            assert_eq!(g.on_tick(now, &th), Some(Restart));
            now += 1;
            let a = g.on_crashed(now, gen, &th);
            let at_count = gen == th.cef_fail_count as u64;
            assert_eq!(
                a,
                hide(Failed, gen).filter(|_| at_count),
                "crash of generation {gen}"
            );
        }
        assert!(
            now < r + th.cef_hang_after_ms,
            "Failed inside the first page's bound"
        );
        let GraphicsState::Failed { retry_at } = g.state() else {
            panic!("expected Failed, got {:?}", g.state())
        };
        for at in [r + th.cef_hang_after_ms, now + th.cef_hang_after_ms, retry_at - 1] {
            assert_eq!(g.on_tick(at, &th), None, "no bound hide after Failed, at {at}");
        }

        // Every crash from a Ready page, the last one the count-th.
        let span = th.cef_fail_window_ms;
        let (mut g, last) = crashes(&spread(th.cef_fail_count, span), &th);
        assert_eq!(last, hide(Failed, th.cef_fail_count as u64));
        assert_eq!(g.on_tick(span + th.cef_hang_after_ms, &th), None);
    }

    // A crash or recycle hide does not stop the reload (C08 maps both to `hide_overlay()` alone), so
    // that reload can still fail: its load timeout at the count enters Failed and hides again, as
    // `Failed`, with no restart between the two hides. Those are the only pairs of hides with none
    // between.
    #[test]
    fn a_bound_hide_then_the_reload_fails_at_the_count() {
        let th = t();
        let mut checked = 0;
        for ending in ENDINGS {
            let (mut g, proof, restart) = reloading_after(ending, &th);
            // Failures so far: one for a crash (the page's own), none for a recycle. Each crash of a
            // reload below adds one, until the next one, its load timeout, is the count-th.
            let before = usize::from(ending == Crash);
            let mut now = restart;
            for _ in before..th.cef_fail_count - 1 {
                now += 1;
                assert_eq!(g.on_crashed(now, g.generation(), &th), None);
                let back = match g.state() {
                    GraphicsState::Backoff { until, .. } => until,
                    other => panic!("{ending:?}: expected Backoff, got {other:?}"),
                };
                assert_eq!(g.on_tick(back, &th), Some(Restart));
                now = back;
            }
            let (off, timeout) = (proof + th.cef_hang_after_ms, now + th.cef_load_timeout_ms);
            assert!(
                now < off && off < timeout,
                "{ending:?}: restart, hide, then the load timeout"
            );
            let mut acts = Vec::new();
            for at in now + 1..=timeout {
                if let Some(a) = g.on_tick(at, &th) {
                    acts.push((at, a));
                }
            }
            let last = g.generation();
            assert_eq!(
                acts,
                vec![
                    (
                        off,
                        Hide {
                            reason: ending,
                            gen: 1
                        }
                    ),
                    (
                        timeout,
                        Hide {
                            reason: Failed,
                            gen: last
                        }
                    )
                ],
                "{ending:?}"
            );
            checked += 1;
        }
        assert_eq!(checked, ENDINGS.len());
    }

    // Under `Thresholds::default()` the bound equals the load timeout (both 10 s), so reading the
    // wrong field, or timing the bound from the reload's restart or from the crash, is invisible
    // there. Here they differ, and so does the beat.
    #[test]
    fn the_bound_reads_cef_hang_after_ms_from_the_last_proof() {
        let th = Thresholds {
            cef_hang_after_ms: 7_777,
            cef_load_timeout_ms: 20_000,
            cef_alive_every_ms: 1_111,
            cef_backoff_base_ms: 250,
            cef_backoff_cap_ms: 400,
            ..t()
        };
        assert!(th.cef_hang_after_ms + th.cef_backoff_base_ms < th.cef_load_timeout_ms);
        let mut checked = 0;
        for ending in ENDINGS {
            let (mut g, proof, restart) = reloading_after(ending, &th);
            let off = proof + th.cef_hang_after_ms;
            assert_eq!(g.on_tick(off - 1, &th), None);
            assert_eq!(g.on_tick(off, &th), hide(ending, 1), "{ending:?}: from the proof");
            assert_eq!(g.state(), GraphicsState::Starting { since: restart });
            checked += 1;
        }
        assert_eq!(checked, ENDINGS.len());
    }

    // A back-off that ends on the bound's instant (a base of the bound less the crash's lateness):
    // the bound and the restart fall due on one tick. The hide wins it, and the restart follows on
    // the next tick.
    #[test]
    fn the_bound_wins_over_a_restart_due_on_the_same_tick() {
        let d = t();
        let base = d.cef_hang_after_ms - (d.cef_alive_every_ms - 1);
        let th = Thresholds {
            cef_backoff_base_ms: base,
            cef_backoff_cap_ms: base,
            ..d
        };
        let (mut g, r, c) = crashed_ready(&th);
        let off = r + th.cef_hang_after_ms;
        assert_eq!(c + base, off, "the back-off ends on the bound");
        assert_eq!(g.on_tick(off - 1, &th), None);
        assert_eq!(g.on_tick(off, &th), hide(Crash, 1), "the hide first");
        assert_eq!(
            (g.state(), g.generation()),
            (
                GraphicsState::Backoff {
                    attempt: 0,
                    until: off
                },
                1
            )
        );
        assert_eq!(g.on_tick(off, &th), Some(Restart), "then the restart");
        assert_eq!(g.generation(), 2);
    }

    // PR #22 review M3: a beat stamped earlier than the last one (C08 stamps on receipt from a
    // monotonic clock, so this is unreachable today) never pulls the hang deadline in.
    #[test]
    fn an_earlier_stamped_beat_never_pulls_the_deadline_in() {
        let th = t();
        let (mut g, r) = ready_at(&th);
        let (late, early) = (r + 2 * th.cef_alive_every_ms, r + th.cef_alive_every_ms);
        g.on_alive(late, 1);
        g.on_alive(early, 1);
        assert_eq!(
            g.state(),
            GraphicsState::Ready {
                since: r,
                last_alive: late
            }
        );
        assert_eq!(g.on_tick(early + th.cef_hang_after_ms, &th), None);
        assert_eq!(g.on_tick(late + th.cef_hang_after_ms, &th), hide(Hang, 1));
    }

    #[test]
    fn the_failure_history_bound_leaves_room_for_failed() {
        let th = t();
        assert!(
            FAILURE_HISTORY_CAP >= th.cef_fail_count,
            "a window of {FAILURE_HISTORY_CAP} can never count {} failures",
            th.cef_fail_count
        );
    }

    // ---- the bound alone (C06's final hold; coordinator decision on the C05b PR #22 review) ----

    // In the final hold the graphics tick runs the bound and nothing else: a restart, a load timeout,
    // a recycle or the cooldown's end due on that tick must not move the machine, or the hold would
    // drop a `Restart` after the generation has moved and hide a healthy Final card. Each setup has
    // one timer due (the control: `on_tick` acts on it) and the bound not yet due.
    #[test]
    fn the_bound_alone_runs_no_other_timer() {
        let th = t();
        let mut checked = 0;
        // (what, the machine, the instant its timer is due)
        let mut setups: Vec<(&str, Graphics, Ms)> = Vec::new();
        setups.push(("a back-off due", Graphics::new(), 0));
        let mut loading = Graphics::new();
        assert_eq!(loading.on_tick(0, &th), Some(Restart));
        setups.push(("a load timeout due", loading.clone(), th.cef_load_timeout_ms));
        let mut ready = loading.clone();
        ready.on_ready(0, 1);
        ready.on_alive(th.cef_recycle_every_ms - 1, 1);
        setups.push(("a recycle due", ready, th.cef_recycle_every_ms));
        let fails: Vec<Ms> = (0..th.cef_fail_count as Ms)
            .map(|k| k * th.cef_backoff_cap_ms)
            .collect();
        let (failed, last) = crashes(&fails, &th);
        assert!(
            matches!(last, Some(Hide { reason: Failed, .. })),
            "setup: {last:?}"
        );
        let GraphicsState::Failed { retry_at } = failed.state() else {
            panic!("setup: {:?}", failed.state())
        };
        setups.push(("the cooldown's end", failed, retry_at));
        for (what, g, due) in setups {
            let mut control = g.clone();
            let acted = control.on_tick(due, &th);
            assert!(
                acted.is_some() || control != g,
                "{what}: setup, on_tick does nothing at {due}"
            );
            let mut bound = g.clone();
            assert_eq!(bound.on_bound_tick(due, &th), None, "{what} at {due}");
            assert_eq!(bound, g, "{what}: the bound alone moved the machine");
            checked += 1;
        }
        assert_eq!(checked, 4, "setups checked: {checked}");
    }

    // When the bound is due, the bound alone is exactly `on_tick`'s first check: the same hide, and
    // the same machine after it, for the Ready page's own frame (a hang) and a held frame (a crash).
    #[test]
    fn the_bound_alone_hides_as_on_tick_does() {
        let th = t();
        let mut ready = Graphics::new();
        assert_eq!(ready.on_tick(0, &th), Some(Restart));
        ready.on_ready(0, 1);
        let mut crashed = ready.clone();
        crashed.on_alive(th.cef_alive_every_ms, 1);
        assert_eq!(
            crashed.on_crashed(th.cef_alive_every_ms, 1, &th),
            None,
            "setup: below the count"
        );
        let cases = [
            ("a hang", ready, th.cef_hang_after_ms, hide(Hang, 1)),
            (
                "a crash's held frame",
                crashed,
                th.cef_alive_every_ms + th.cef_hang_after_ms,
                hide(Crash, 1),
            ),
        ];
        let mut checked = 0;
        for (what, g, due, want) in cases {
            let mut early = g.clone();
            assert_eq!(
                early.on_bound_tick(due - 1, &th),
                None,
                "{what}: 1 ms before the bound"
            );
            assert_eq!(early, g, "{what}: an early bound tick moved the machine");
            let (mut a, mut b) = (g.clone(), g.clone());
            assert_eq!(a.on_bound_tick(due, &th), want, "{what}");
            assert_eq!(b.on_tick(due, &th), want, "{what}: on_tick");
            assert_eq!(
                a, b,
                "{what}: the bound alone and on_tick leave different machines"
            );
            checked += 1;
        }
        assert_eq!(checked, 2, "cases checked: {checked}");
    }
}
