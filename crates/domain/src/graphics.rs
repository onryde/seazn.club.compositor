//! CEF overlay lifecycle. It never blocks video: a failed page restarts on a back-off while the adapter
//! holds the last overlay frame (C11); a hung page and repeated failures hide the overlay, so a stale
//! score never stays on air, and retry after a cooldown; the planned recycle is a restart, never a
//! failure.
//!
//! ```text
//! Backoff --due tick: restart, next generation--> Starting --ready signal--> Ready
//!    ^                                               |                         |
//!    +---------- failure: load timeout or crash -----+--------- crash ---------+
//!                (the cef_fail_count-th in the window enters Failed + Hide)    |
//!                                                                              |
//! Ready --silent cef_hang_after_ms: failure + Hide--> Backoff | Failed         |
//! Ready --beat--> Ready (the hang deadline moves; the recycle does not)        |
//! Ready --recycle due--> Starting (Restart; not a failure)                     |
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
//!   page Ready. A beat moves only the hang deadline, never the recycle.
//! - **Failure.** A crash of the current generation while Starting or Ready (C11: a bus error, the
//!   renderer exiting, a CEF child gone, RSS over its cap), a load with no ready signal
//!   `cef_load_timeout_ms` after its restart, or a hang: a Ready page with no beat for
//!   `cef_hang_after_ms` (`docs/_INDEX.md` Owed 18). A JS hang leaves the renderer alive, and cefsrc
//!   emits only on paint (F-001), so the beat is the only signal. A crash reported while backing off or
//!   Failed is that same failure reported again, and is not counted twice. When the hang and the
//!   recycle fall due on the same tick, the hang wins: it is counted, never laundered as a recycle.
//! - **Back-off.** The n-th failure within `cef_fail_window_ms` waits `delay(n - 1)` on the CEF
//!   back-off (1 s → 10 s), so a page that keeps failing waits longer.
//! - **Failed.** The `cef_fail_count`-th failure within `cef_fail_window_ms` (spec §4.2: 3 in 5 min; a
//!   failure exactly the window old still counts, as in `Window`) enters Failed and returns `Hide`,
//!   once. Failed is not terminal (C05 brief): a flaky deploy of the page early in a match must not
//!   lose the score bug for the whole match, so `cef_fail_window_ms` after it the page restarts. By then
//!   every failure that led to Failed is at least the window old, so the retried page gets its full
//!   `cef_fail_count` attempts again.
//! - **Hide.** A `Hide` comes from entering Failed, or from a hang, at once and below the count too:
//!   a hung page's last frame may be a stale score, so it does not stay on air through the back-off
//!   and reload (owner-approved design, 2026-10-18; "at once" is the coordinator's reading of it). A
//!   crash below the count still holds the frame. The overlay stays hidden from a `Hide` until the
//!   next generation's Ready; nothing else lifts it. After a `Hide` the next action is always a
//!   `Restart`: at `retry_at` from Failed, at the back-off's end from a hang. Nothing of the hidden
//!   generation may reach the programme before it; under F-001's console readiness a page that never
//!   signals ready keeps painting, so the adapters must hold the hide against its frames, and lift it
//!   once a later generation signals ready (C08 maps `HideOverlay` to `hide_overlay()` and
//!   `graphics.stop()`; C11 writes the slot only from a generation that signalled its own ready;
//!   `docs/_INDEX.md` Owed 17).
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
    /// `cef_recycle_every_ms` after it). `last_alive` is its last beat, the ready signal included.
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
    /// `gen` is the generation whose failure took it off. C12 logs the one line
    /// `overlay hidden reason=<r> gen=<g>` for it (PR #22 review I2).
    Hide { reason: HideReason, gen: u64 },
}

/// What took the overlay off air. It names the cause, not the state the machine is left in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideReason {
    /// A Ready page stopped beating for `cef_hang_after_ms` (Owed 18): below the count, and at it.
    Hang,
    /// A crash or a load timeout entered Failed (the `cef_fail_count`-th failure in the window).
    Failed,
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
    /// only while that page is loading.
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
        if let GraphicsState::Ready { since, .. } = self.state {
            self.state = GraphicsState::Ready {
                since,
                last_alive: now,
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
            GraphicsState::Starting { .. } | GraphicsState::Ready { .. } => self.fail(now, t),
            // Already failed: the same page reporting again changes nothing.
            GraphicsState::Backoff { .. } | GraphicsState::Failed { .. } => None,
        }
    }

    /// `Some(Restart)` when a restart is due (the back-off is over, the recycle is due, or the
    /// cooldown is over); `Hide { reason: Failed, .. }` when a load timeout enters Failed, and
    /// `Hide { reason: Hang, .. }` on every hang.
    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> {
        match self.state {
            GraphicsState::Backoff { until, .. } if now >= until => Some(self.restart(now)),
            GraphicsState::Starting { since } if elapsed(now, since) >= t.cef_load_timeout_ms => {
                self.fail(now, t)
            }
            // A hung page (Owed 18): a counted failure, and hidden at once whatever `fail` answers,
            // because its last frame may be a stale score. Checked before the recycle, so a hang that
            // falls due with it is never laundered as a planned restart.
            GraphicsState::Ready { last_alive, .. } if elapsed(now, last_alive) >= t.cef_hang_after_ms => {
                self.fail(now, t);
                Some(GraphicsAction::Hide {
                    reason: HideReason::Hang,
                    gen: self.generation,
                })
            }
            // The planned recycle: a restart, and no failure is recorded.
            GraphicsState::Ready { since, .. } if elapsed(now, since) >= t.cef_recycle_every_ms => {
                Some(self.restart(now))
            }
            GraphicsState::Failed { retry_at } if now >= retry_at => Some(self.restart(now)),
            _ => None,
        }
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
            return Some(GraphicsAction::Hide {
                reason: HideReason::Failed,
                gen: self.generation,
            });
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
    use HideReason::{Failed, Hang};

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
    /// crash. Each instant must lie at or after the restart before it. Returns the machine and the
    /// last crash's action.
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
    // loading, so its load timeout still fires.
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

    #[test]
    fn the_failure_history_bound_leaves_room_for_failed() {
        let th = t();
        assert!(
            FAILURE_HISTORY_CAP >= th.cef_fail_count,
            "a window of {FAILURE_HISTORY_CAP} can never count {} failures",
            th.cef_fail_count
        );
    }
}
