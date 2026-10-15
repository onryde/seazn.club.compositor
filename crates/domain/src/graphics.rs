//! CEF overlay lifecycle. It never blocks video: a failed page restarts on a back-off while the adapter
//! holds the last overlay frame (C11); repeated failures hide the overlay, so a stale score never stays
//! on air, and retry after a cooldown; the planned recycle is a restart, never a failure.
//!
//! ```text
//! Backoff --due tick: restart, next generation--> Starting --ready signal--> Ready
//!    ^                                               |                         |
//!    +---------- failure: load timeout or crash -----+--------- crash ---------+
//!                (the cef_fail_count-th in the window enters Failed + Hide)    |
//!                                                                              |
//! Ready --recycle due--> Starting (Restart; not a failure)                     |
//! Failed --retry_at--> Starting (Restart)                                      v
//! ```
//!
//! - **Ready.** The page's readiness signal for the current generation, while Starting: F-001 pinned it
//!   to `console.info("seazn:ready")`, read by C11 from the `cefconsole` log category. Frames flow
//!   before it (F-001), so a first frame is never ready (C11). A ready from another generation, a
//!   second ready, or one that comes after the load timed out changes nothing: the recycle clock runs
//!   from the first ready, and a timed-out load stays one failure.
//! - **Failure.** A crash of the current generation while Starting or Ready (C11: a bus error, the
//!   renderer exiting, a CEF child gone, RSS over its cap), or a load with no ready signal
//!   `cef_load_timeout_ms` after its restart. A crash reported while backing off or Failed is that same
//!   failure reported again, and is not counted twice.
//! - **Back-off.** The n-th failure within `cef_fail_window_ms` waits `delay(n - 1)` on the CEF
//!   back-off (1 s → 10 s), so a page that keeps failing waits longer.
//! - **Failed.** The `cef_fail_count`-th failure within `cef_fail_window_ms` (spec §4.2: 3 in 5 min; a
//!   failure exactly the window old still counts, as in `Window`) enters Failed and returns `Hide`,
//!   once. Failed is not terminal (C05 brief): a flaky deploy of the page early in a match must not
//!   lose the score bug for the whole match, so `cef_fail_window_ms` after it the page restarts. By then
//!   every failure that led to Failed is at least the window old, so the retried page gets its full
//!   `cef_fail_count` attempts again.
//! - **Hide.** After a `Hide` the next action is always the `Restart` at `retry_at`. Nothing of the
//!   hidden generation may reach the programme before it; under F-001's console readiness a page that
//!   never signals ready keeps painting, so the adapters must hold the hide against its frames
//!   (`docs/_INDEX.md`, the C05 false-premise rows).
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
    /// The current generation signalled ready at `since`; its recycle is due `cef_recycle_every_ms`
    /// after it.
    Ready { since: Ms },
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
    /// Take the overlay off air (C06 `HideOverlay`) until the next `Restart`.
    Hide,
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
            self.state = GraphicsState::Ready { since: now };
        }
    }

    /// The page of generation `gen` crashed. `Some(Hide)` when this failure enters Failed.
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
    /// cooldown is over); `Some(Hide)` when a load timeout enters Failed.
    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> {
        match self.state {
            GraphicsState::Backoff { until, .. } if now >= until => Some(self.restart(now)),
            GraphicsState::Starting { since } if elapsed(now, since) >= t.cef_load_timeout_ms => {
                self.fail(now, t)
            }
            // The planned recycle: a restart, and no failure is recorded.
            GraphicsState::Ready { since } if elapsed(now, since) >= t.cef_recycle_every_ms => {
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
            return Some(GraphicsAction::Hide);
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

    fn t() -> Thresholds {
        Thresholds::default()
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
        let hides: Vec<Ms> = acts
            .iter()
            .filter(|(_, a)| *a == Hide)
            .map(|(at, _)| *at)
            .collect();
        assert_eq!(
            restarts, th.cef_fail_count,
            "one restart per attempt before Failed"
        );
        assert_eq!(hides.len(), 1, "hide exactly once on entering Failed");
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
        assert_eq!(g.on_tick(1_000 + th.cef_recycle_every_ms, &th), Some(Restart));
        // three recycles never accumulate into Failed
        for k in 2..=4u64 {
            g.on_ready(1_000 + th.cef_recycle_every_ms * (k - 1) + 500, k);
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
        assert_eq!(g.state(), GraphicsState::Ready { since: 2_804 });
        let due = 2_804 + th.cef_recycle_every_ms;
        assert_eq!(g.on_tick(due - 1, &th), None);
        assert_eq!(g.state(), GraphicsState::Ready { since: 2_804 });
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
        let mut now = next_restart(&mut g, 2_804, 1_000, &th); // the recycle
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
        assert_eq!(last, Some(Hide), "{count} failures exactly {span} ms apart");
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
        assert_eq!(last, Some(Hide));
        let retry_at = span + span;
        let hidden = g.generation();
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
        assert_eq!(last, Some(Hide), "first Failed");
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
                assert_eq!(a, Some(Hide), "the second Failed hides again");
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
        let hides: Vec<Ms> = acts
            .iter()
            .filter(|(_, a)| *a == Hide)
            .map(|(at, _)| *at)
            .collect();
        let restarts = acts.iter().filter(|(_, a)| *a == Restart).count();
        assert_eq!(hides, vec![to_hide, second], "{acts:?}");
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
        assert_eq!(g.state(), GraphicsState::Ready { since: 100 });
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
            assert_eq!(
                a == Some(Hide),
                gen == th.cef_fail_count as u64,
                "generation {gen}"
            );
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
                        (Some(Hide), 4, now + 77_000),
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
