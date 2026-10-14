# C05: Domain Graphics sub-machine and DegradeLadder

**Repo:** `seazn.club.compositor`. **Depends on:** C02. Run it after C04, because both edit `lib.rs`.

## Context

- **Graphics** is the CEF overlay process, and the overlay must never block video.
  - Ready comes from the page's readiness signal (pinned in F-001).
  - A crash, or a load that misses `cef_load_timeout_ms`, counts as a failure. The overlay restarts on
    backoff, and the adapter holds the last overlay frame while it does.
  - At `cef_fail_count` failures within `cef_fail_window_ms`, the state becomes `Failed` and the
    machine emits `HideOverlay`, so a stale score never stays on screen.
  - `Failed` is **not terminal**. It retries after `cef_fail_window_ms`. Otherwise a flaky deploy of
    the page early in a match would lose the scorebug for the entire match.
  - Every `cef_recycle_every_ms`, the browser is recycled. That restart is planned and is not counted
    as a failure.
- **DegradeLadder** handles an encoder that can't keep up. Frames are dropped before the encoder (the
  adapter counts them), and once `ladder_overruns_to_step` overruns land within `ladder_window_ms`,
  the ladder steps down one rung: `Full → OverlayHalfRate → FastPreset`. It steps at most once per
  `ladder_step_min_gap_ms`. **There is no step back up in R2.** The match finishes on the degraded
  rung, and it is reported. This is YAGNI until the soak shows a need.

## Files

- Create: `crates/domain/src/graphics.rs`, `crates/domain/src/ladder.rs`
- Modify: `crates/domain/src/lib.rs`

## Interfaces

- **Consumes:** `backoff::delay`, `window::Window`, `time`, `config::Thresholds`.
- **Produces:**
  ```rust
  // graphics.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum GraphicsState { Starting { since: Ms }, Ready { since: Ms }, Backoff { attempt: u32, until: Ms }, Failed { retry_at: Ms } }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum GraphicsAction { Restart, Hide }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct Graphics { /* private */ }
  impl Graphics {
      pub fn new() -> Self;   // Backoff{0, until: 0}
      pub fn state(&self) -> GraphicsState;
      pub fn generation(&self) -> u64;
      pub fn on_ready(&mut self, now: Ms, gen: u64);
      pub fn on_crashed(&mut self, now: Ms, gen: u64, t: &Thresholds) -> Option<GraphicsAction>; // Some(Hide) when entering Failed
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction>;
  }
  // ladder.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)] pub enum Rung { Full, OverlayHalfRate, FastPreset }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct Ladder { /* private */ }
  impl Ladder {
      pub fn new() -> Self;
      pub fn rung(&self) -> Rung;
      pub fn overruns(&self) -> u64;   // total dropped-before-encoder frames this session
      pub fn on_overrun(&mut self, now: Ms, t: &Thresholds) -> Option<Rung>; // Some(new rung) ⇒ effect StepDown
  }
  ```
  Both implement `Default` via `new()`.

## Steps

- [ ] **Step 1: Write `graphics.rs` with `todo!()` bodies, plus tests.**
  ```rust
  //! CEF overlay lifecycle. Never blocks video: failures restart on back-off; repeated failures hide the
  //! overlay (no stale score on air) and retry after a cooldown. Scheduled recycles are not failures.
  use crate::backoff::delay;
  use crate::config::Thresholds;
  use crate::time::{elapsed, Ms};
  use crate::window::Window;

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum GraphicsState { Starting { since: Ms }, Ready { since: Ms }, Backoff { attempt: u32, until: Ms }, Failed { retry_at: Ms } }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum GraphicsAction { Restart, Hide }

  const FAILURE_HISTORY_CAP: usize = 32;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Graphics { state: GraphicsState, generation: u64, failures: Window }

  impl Default for Graphics { fn default() -> Self { Self::new() } }

  impl Graphics {
      pub fn new() -> Self {
          Self { state: GraphicsState::Backoff { attempt: 0, until: 0 }, generation: 0, failures: Window::new(FAILURE_HISTORY_CAP) }
      }
      pub fn state(&self) -> GraphicsState { self.state }
      pub fn generation(&self) -> u64 { self.generation }
      pub fn on_ready(&mut self, now: Ms, gen: u64) { todo!() }
      pub fn on_crashed(&mut self, now: Ms, gen: u64, t: &Thresholds) -> Option<GraphicsAction> { todo!() }
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> { todo!() }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      fn t() -> Thresholds { Thresholds::default() }

      /// Run ticks every 100 ms; the page signals ready `ready_after` ms after each restart (None = never).
      fn run(until: Ms, ready_after: Option<Ms>) -> (Graphics, Vec<(Ms, GraphicsAction)>) {
          let th = t();
          let mut g = Graphics::new();
          let mut acts = Vec::new();
          let mut started: Option<(Ms, u64)> = None;
          let mut now = 0;
          while now <= until {
              if let Some(a) = g.on_tick(now, &th) {
                  if a == GraphicsAction::Restart { started = Some((now, g.generation())); }
                  acts.push((now, a));
              }
              if let (Some((at, gen)), Some(r)) = (started, ready_after) {
                  if now >= at + r { g.on_ready(now, gen); started = None; }
              }
              now += 100;
          }
          (g, acts)
      }

      #[test]
      fn healthy_page_is_ready_after_one_restart() {
          let (g, acts) = run(5_000, Some(1_500));
          assert!(matches!(g.state(), GraphicsState::Ready { .. }));
          assert_eq!(acts, vec![(0, GraphicsAction::Restart)]);
      }
      #[test]
      fn page_that_never_signals_ready_fails_after_three_timeouts_and_hides() {
          let th = t();
          let (g, acts) = run(th.cef_fail_window_ms - 1, None);
          let restarts = acts.iter().filter(|(_, a)| *a == GraphicsAction::Restart).count();
          let hides: Vec<Ms> = acts.iter().filter(|(_, a)| *a == GraphicsAction::Hide).map(|(at, _)| *at).collect();
          assert_eq!(restarts, th.cef_fail_count, "one restart per attempt before Failed");
          assert_eq!(hides.len(), 1, "hide exactly once on entering Failed");
          assert!(matches!(g.state(), GraphicsState::Failed { .. }));
      }
      #[test]
      fn failed_retries_after_the_cooldown() {
          let th = t();
          let mut g = Graphics::new();
          let mut now = 0;
          for _ in 0..th.cef_fail_count {
              while g.on_tick(now, &th) != Some(GraphicsAction::Restart) { now += 100; }
              g.on_crashed(now + 10, g.generation(), &th);
              now += 10;
          }
          let GraphicsState::Failed { retry_at } = g.state() else { panic!("expected Failed, got {:?}", g.state()) };
          assert_eq!(retry_at - now, th.cef_fail_window_ms);
          assert_eq!(g.on_tick(retry_at - 1, &th), None);
          // cooldown over ⇒ back to back-off ⇒ the next tick restarts
          g.on_tick(retry_at, &th);
          assert_eq!(g.on_tick(retry_at, &th), Some(GraphicsAction::Restart));
      }
      #[test]
      fn scheduled_recycle_is_a_restart_not_a_failure() {
          let th = t();
          let mut g = Graphics::new();
          assert_eq!(g.on_tick(0, &th), Some(GraphicsAction::Restart));
          g.on_ready(1_000, 1);
          assert_eq!(g.on_tick(1_000 + th.cef_recycle_every_ms, &th), Some(GraphicsAction::Restart));
          // three recycles never accumulate into Failed
          for k in 2..=4u64 {
              g.on_ready(1_000 + th.cef_recycle_every_ms * (k - 1) + 500, k);
              let a = g.on_tick(1_000 + th.cef_recycle_every_ms * k + 500, &th);
              assert_eq!(a, Some(GraphicsAction::Restart));
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
              while g.on_tick(now, &th) != Some(GraphicsAction::Restart) { now += 100; }
              g.on_ready(now + 100, g.generation());
              now += th.cef_fail_window_ms / 2 + 1; // fewer than 3 inside any 5-min window
              assert_eq!(g.on_crashed(now, g.generation(), &th), None);
          }
          assert!(!matches!(g.state(), GraphicsState::Failed { .. }));
      }
  }
  ```
- [ ] **Step 2: Run, expect a FAIL.**

- [ ] **Step 3: Implement Graphics.**
  ```rust
  impl Graphics {
      pub fn on_ready(&mut self, now: Ms, gen: u64) {
          if gen == self.generation && matches!(self.state, GraphicsState::Starting { .. }) {
              self.state = GraphicsState::Ready { since: now };
          }
      }
      fn fail(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> {
          let n = self.failures.record(now, t.cef_fail_window_ms);
          if n >= t.cef_fail_count {
              self.state = GraphicsState::Failed { retry_at: now + t.cef_fail_window_ms };
              return Some(GraphicsAction::Hide);
          }
          let attempt = (n as u32).saturating_sub(1);
          self.state = GraphicsState::Backoff { attempt, until: now + delay(attempt, t.cef_backoff_base_ms, t.cef_backoff_cap_ms) };
          None
      }
      pub fn on_crashed(&mut self, now: Ms, gen: u64, t: &Thresholds) -> Option<GraphicsAction> {
          if gen != self.generation { return None; }
          match self.state {
              GraphicsState::Starting { .. } | GraphicsState::Ready { .. } => self.fail(now, t),
              _ => None,
          }
      }
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<GraphicsAction> {
          match self.state {
              GraphicsState::Backoff { until, .. } if now >= until => {
                  self.generation += 1;
                  self.state = GraphicsState::Starting { since: now };
                  Some(GraphicsAction::Restart)
              }
              GraphicsState::Starting { since } if elapsed(now, since) >= t.cef_load_timeout_ms => self.fail(now, t),
              GraphicsState::Ready { since } if elapsed(now, since) >= t.cef_recycle_every_ms => {
                  self.generation += 1;
                  self.state = GraphicsState::Starting { since: now };
                  Some(GraphicsAction::Restart)
              }
              GraphicsState::Failed { retry_at } if now >= retry_at => {
                  self.state = GraphicsState::Backoff { attempt: 0, until: now };
                  None
              }
              _ => None,
          }
      }
  }
  ```

- [ ] **Step 4: Run, expect a PASS.** `cargo test -p domain graphics`. Expected: 6 passed.

- [ ] **Step 5: Write `ladder.rs`, its tests, then the implementation.**
  ```rust
  //! Encoder-overrun degrade ladder: overlay rate first, then x264 preset. Never stalls the clock.
  use crate::config::Thresholds;
  use crate::time::{elapsed, Ms};
  use crate::window::Window;

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
  pub enum Rung { Full, OverlayHalfRate, FastPreset }

  impl Rung {
      fn next(self) -> Option<Rung> {
          match self { Rung::Full => Some(Rung::OverlayHalfRate), Rung::OverlayHalfRate => Some(Rung::FastPreset), Rung::FastPreset => None }
      }
  }

  const OVERRUN_HISTORY_CAP: usize = 64;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Ladder { rung: Rung, last_step_at: Option<Ms>, recent: Window, total: u64 }

  impl Default for Ladder { fn default() -> Self { Self::new() } }

  impl Ladder {
      pub fn new() -> Self { Self { rung: Rung::Full, last_step_at: None, recent: Window::new(OVERRUN_HISTORY_CAP), total: 0 } }
      pub fn rung(&self) -> Rung { self.rung }
      pub fn overruns(&self) -> u64 { self.total }
      pub fn on_overrun(&mut self, now: Ms, t: &Thresholds) -> Option<Rung> {
          self.total += 1;
          let n = self.recent.record(now, t.ladder_window_ms);
          if n < t.ladder_overruns_to_step { return None; }
          if self.last_step_at.is_some_and(|l| elapsed(now, l) < t.ladder_step_min_gap_ms) { return None; }
          let next = self.rung.next()?;
          self.rung = next;
          self.last_step_at = Some(now);
          Some(next)
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      fn t() -> Thresholds { Thresholds::default() }
      #[test]
      fn isolated_overruns_never_step() {
          let th = t();
          let mut l = Ladder::new();
          for i in 0..100 { assert_eq!(l.on_overrun(i * (th.ladder_window_ms + 1), &th), None); }
          assert_eq!(l.rung(), Rung::Full);
          assert_eq!(l.overruns(), 100, "every overrun is still counted as a dropped frame");
      }
      #[test]
      fn sustained_overruns_step_overlay_first_then_preset_then_stop() {
          let th = t();
          let mut l = Ladder::new();
          let mut steps = Vec::new();
          for i in 0..2_000u64 { if let Some(r) = l.on_overrun(i * 33, &th) { steps.push((i * 33, r)); } } // ~66 s of constant overrun
          assert_eq!(steps.iter().map(|s| s.1).collect::<Vec<_>>(), vec![Rung::OverlayHalfRate, Rung::FastPreset]);
          assert!(steps[1].0 - steps[0].0 >= th.ladder_step_min_gap_ms);
      }
      #[test]
      fn bottom_rung_holds() {
          let th = t();
          let mut l = Ladder::new();
          for i in 0..10_000u64 { l.on_overrun(i * 10, &th); }
          assert_eq!(l.rung(), Rung::FastPreset);
      }
  }
  ```
  Run `cargo test -p domain ladder`. Expected: 3 passed.

- [ ] **Step 6: Mutate each guard once.**
  1. In `fail`, change `>=` to `>`. Expected red:
     `page_that_never_signals_ready_fails_after_three_timeouts_and_hides`.
  2. Delete the `Failed { retry_at }` arm in `on_tick`. Expected red: `failed_retries_after_the_cooldown`.
  3. Count a recycle as a failure by calling `self.fail` in the `Ready` arm. Expected red:
     `scheduled_recycle_is_a_restart_not_a_failure`.
  4. Remove the min-gap check in the ladder. Expected red: `sustained_overruns_step…`.

  Revert each mutant, and record all four in the commit body.

- [ ] **Step 7: Commit.**
  `git commit -m "feat(domain): graphics lifecycle and degrade ladder (C05)"`

## Acceptance

- 9 new tests pass.
- All 4 mutants went red.
- Clippy is clean.

## Do not touch

The other domain modules and config values.

## Output cap

Under 15 lines.
