# C04: Domain InputLink and OutputLink

**Repo:** `seazn.club.compositor`. **Depends on:** C02. Run it after C03, not alongside, because both
edit `lib.rs`.

## Context

This task builds two sub-machines that reconnect forever and never stop the programme:

- **InputLink** — the camera pull, SRT from CF Input A.
  - Retries on capped backoff from 1 s up to 10 s.
  - Counts peer **kicks** (EOS while running, e.g. a second reader or CF maintenance).
  - Raises an alert flag when there are **more than** 3 kicks in 5 min.
- **OutputLink** — the RTMPS push to CF Input B.
  - Treats 5 s with no acked bytes as a stall and swaps the sink immediately.
  - On an error, backs off from 0.5 s up to 2 s so a reconnect lands inside 5 s.

**Generation guard.** Every rebuild or swap starts a new generation. Events carry the generation of
the pipeline that emitted them, and an event from an older generation is ignored. Without this guard,
a late `InputLost` from a torn-down pipeline would knock down the healthy new one. This is a "cannot
happen" turned into a tested guard.

## Files

- Create: `crates/domain/src/input_link.rs`, `crates/domain/src/output_link.rs`
- Modify: `crates/domain/src/lib.rs` (`pub mod input_link; pub mod output_link;`)

## Interfaces

- **Consumes:** `backoff::delay`, `window::Window`, `time::{Ms, elapsed}`, `config::Thresholds`.
- **Produces:**
  ```rust
  // input_link.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum LossReason { Stalled, Eos, Error }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum InputState { Connecting { attempt: u32, since: Ms }, Running { since: Ms }, Backoff { attempt: u32, until: Ms } }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct InputLink { /* private */ }
  impl InputLink {
      pub fn new() -> Self;                       // Backoff{0, until: 0}: first tick builds generation 1
      pub fn state(&self) -> InputState;
      pub fn generation(&self) -> u64;            // number of builds so far
      pub fn on_connected(&mut self, now: Ms, gen: u64);
      pub fn on_lost(&mut self, now: Ms, gen: u64, reason: LossReason, t: &Thresholds);
      /// Running but no frame since max(running since, last frame) for input_stall_after_ms ⇒ on_lost(Stalled).
      pub fn check_stall(&mut self, now: Ms, last_frame_at: Option<Ms>, t: &Thresholds);
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool;   // true ⇒ effect RebuildInput(generation())
      pub fn kicks(&self, now: Ms, t: &Thresholds) -> usize;
      pub fn kick_alert(&self, now: Ms, t: &Thresholds) -> bool;
  }
  // output_link.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum OutputState { Connecting { attempt: u32, since: Ms }, Connected { last_ack: Ms }, Backoff { attempt: u32, until: Ms } }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct OutputLink { /* private */ }
  impl OutputLink {
      pub fn new() -> Self;                       // Backoff{0, until: 0}
      pub fn state(&self) -> OutputState;
      pub fn generation(&self) -> u64;            // number of sink builds so far
      pub fn is_connected(&self) -> bool;
      pub fn on_connected(&mut self, now: Ms, gen: u64);
      pub fn on_ack(&mut self, now: Ms, gen: u64);
      pub fn on_error(&mut self, now: Ms, gen: u64, t: &Thresholds);
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool;   // true ⇒ effect SwapSink(generation())
  }
  ```
  Both also implement `Default` via `new()`. Clippy's `new_without_default` requires it.

## Steps

- [ ] **Step 1: Write `input_link.rs` with `todo!()` bodies and these tests.**
  ```rust
  //! Camera pull link: reconnect forever on capped back-off; count peer kicks; ignore stale generations.
  use crate::backoff::delay;
  use crate::config::Thresholds;
  use crate::time::{elapsed, Ms};
  use crate::window::Window;

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum LossReason { Stalled, Eos, Error }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum InputState { Connecting { attempt: u32, since: Ms }, Running { since: Ms }, Backoff { attempt: u32, until: Ms } }

  /// Bounded: kick history never holds more than this many stamps.
  const KICK_HISTORY_CAP: usize = 32;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct InputLink { state: InputState, generation: u64, kicks: Window }

  impl Default for InputLink { fn default() -> Self { Self::new() } }

  impl InputLink {
      pub fn new() -> Self {
          Self { state: InputState::Backoff { attempt: 0, until: 0 }, generation: 0, kicks: Window::new(KICK_HISTORY_CAP) }
      }
      pub fn state(&self) -> InputState { self.state }
      pub fn generation(&self) -> u64 { self.generation }
      pub fn on_connected(&mut self, now: Ms, gen: u64) { todo!() }
      pub fn on_lost(&mut self, now: Ms, gen: u64, reason: LossReason, t: &Thresholds) { todo!() }
      pub fn check_stall(&mut self, now: Ms, last_frame_at: Option<Ms>, t: &Thresholds) { todo!() }
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool { todo!() }
      pub fn kicks(&self, now: Ms, t: &Thresholds) -> usize { self.kicks.count(now, t.kick_window_ms) }
      pub fn kick_alert(&self, now: Ms, t: &Thresholds) -> bool { self.kicks(now, t) > t.kick_alert_count }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      fn t() -> Thresholds { Thresholds::default() }

      /// Build, connect, run: returns a link in Running at generation 1.
      fn running(now: Ms) -> InputLink {
          let mut l = InputLink::new();
          assert!(l.on_tick(now, &t()));
          l.on_connected(now, 1);
          l
      }

      #[test]
      fn first_tick_builds_generation_one() {
          let mut l = InputLink::new();
          assert!(l.on_tick(0, &t()));
          assert_eq!(l.generation(), 1);
          assert!(matches!(l.state(), InputState::Connecting { attempt: 0, .. }));
          assert!(!l.on_tick(1, &t()), "no second build while connecting");
      }
      #[test]
      fn consecutive_failures_follow_the_pdf_backoff_1_2_4_8_10_10() {
          let th = t();
          let mut l = InputLink::new();
          let mut now = 0;
          let mut gaps = Vec::new();
          let mut last_build = None;
          for _ in 0..7 {
              // wait for the next build
              while !l.on_tick(now, &th) { now += 10; }
              if let Some(b) = last_build { gaps.push(now - b); }
              last_build = Some(now);
              let gen = l.generation();
              l.on_lost(now, gen, LossReason::Error, &th); // fails instantly
          }
          // failure f (1-based) waits delay(f - 1): the first retry after a never-connected build is 1 s, like a loss from Running
          let want: Vec<Ms> = (0..6).map(|a| delay(a, th.input_backoff_base_ms, th.input_backoff_cap_ms)).collect();
          let rounded: Vec<Ms> = gaps.iter().map(|g| g / 10 * 10).collect();
          assert_eq!(rounded, want);
      }
      #[test]
      fn loss_from_running_restarts_backoff_at_one_second() {
          let th = t();
          let mut l = running(0);
          l.on_lost(5_000, 1, LossReason::Stalled, &th);
          assert_eq!(l.state(), InputState::Backoff { attempt: 1, until: 5_000 + th.input_backoff_base_ms });
      }
      #[test]
      fn connect_that_hangs_times_out_into_backoff() {
          let th = t();
          let mut l = InputLink::new();
          l.on_tick(0, &th);
          assert!(!l.on_tick(th.input_connect_timeout_ms - 1, &th));
          l.on_tick(th.input_connect_timeout_ms, &th);
          assert!(matches!(l.state(), InputState::Backoff { attempt: 1, .. }));
      }
      #[test]
      fn stale_generation_loss_is_ignored() {
          let th = t();
          let mut l = running(0);
          l.on_lost(1_000, 1, LossReason::Eos, &th);
          while !l.on_tick(2_000, &th) {}
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
              while !l.on_tick(now, &th) { now += 100; }
              l.on_connected(now, l.generation());
              assert_eq!(l.kick_alert(now, &th), k > th.kick_alert_count, "after {k} kicks");
          }
          assert!(!l.kick_alert(now + th.kick_window_ms + 1, &th), "alert clears once kicks age out");
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
              while !l.on_tick(now, &th) { now += 100; }
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
          l.check_stall(th.input_stall_after_ms - 1, None, &th);
          assert!(matches!(l.state(), InputState::Running { .. }));
          l.check_stall(th.input_stall_after_ms, None, &th);
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
          assert!(matches!(l.state(), InputState::Running { .. }), "an old frame must not stall a fresh connection");
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
  }
  ```
  Every wait loop advances `now`. A `while !on_tick(now)` that does not advance `now` never
  terminates, because the backoff's `until` lies in the future.

- [ ] **Step 2: Run, expect a FAIL.** `cargo test -p domain input_link`

- [ ] **Step 3: Implement InputLink.**
  ```rust
  pub fn on_connected(&mut self, now: Ms, gen: u64) {
      if gen == self.generation && matches!(self.state, InputState::Connecting { .. }) {
          self.state = InputState::Running { since: now };
      }
  }
  pub fn check_stall(&mut self, now: Ms, last_frame_at: Option<Ms>, t: &Thresholds) {
      if let InputState::Running { since } = self.state {
          let fresh = last_frame_at.map_or(since, |f| f.max(since));
          if elapsed(now, fresh) >= t.input_stall_after_ms {
              let gen = self.generation;
              self.on_lost(now, gen, LossReason::Stalled, t);
          }
      }
  }
  pub fn on_lost(&mut self, now: Ms, gen: u64, reason: LossReason, t: &Thresholds) {
      if gen != self.generation { return; }
      // `attempt` counts consecutive failures; failure number f waits delay(f - 1): 1, 2, 4, 8, 10, 10 s.
      let failed_before = match self.state {
          InputState::Running { .. } => {
              if reason == LossReason::Eos { self.kicks.record(now, t.kick_window_ms); }
              0
          }
          InputState::Connecting { attempt, .. } => attempt,
          InputState::Backoff { .. } => return,
      };
      self.state = InputState::Backoff { attempt: failed_before + 1, until: now + delay(failed_before, t.input_backoff_base_ms, t.input_backoff_cap_ms) };
  }
  pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool {
      match self.state {
          InputState::Backoff { attempt, until } if now >= until => {
              self.generation += 1;
              self.state = InputState::Connecting { attempt, since: now };
              true
          }
          InputState::Connecting { attempt, since } if elapsed(now, since) >= t.input_connect_timeout_ms => {
              self.state = InputState::Backoff { attempt: attempt + 1, until: now + delay(attempt, t.input_backoff_base_ms, t.input_backoff_cap_ms) };
              false
          }
          _ => false,
      }
  }
  ```

- [ ] **Step 4: Run, expect a PASS.** `cargo test -p domain input_link`. Expected: 12 passed.

- [ ] **Step 5: Write `output_link.rs` with `todo!()` bodies and these tests.**
  ```rust
  //! Programme push link: swap the sink on error or on a 5 s ack stall; back-off 0.5 s → 2 s.
  use crate::backoff::delay;
  use crate::config::Thresholds;
  use crate::time::{elapsed, Ms};

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum OutputState { Connecting { attempt: u32, since: Ms }, Connected { last_ack: Ms }, Backoff { attempt: u32, until: Ms } }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct OutputLink { state: OutputState, generation: u64 }

  impl Default for OutputLink { fn default() -> Self { Self::new() } }

  impl OutputLink {
      pub fn new() -> Self { Self { state: OutputState::Backoff { attempt: 0, until: 0 }, generation: 0 } }
      pub fn state(&self) -> OutputState { self.state }
      pub fn generation(&self) -> u64 { self.generation }
      pub fn is_connected(&self) -> bool { matches!(self.state, OutputState::Connected { .. }) }
      pub fn on_connected(&mut self, now: Ms, gen: u64) { todo!() }
      pub fn on_ack(&mut self, now: Ms, gen: u64) { todo!() }
      pub fn on_error(&mut self, now: Ms, gen: u64, t: &Thresholds) { todo!() }
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool { todo!() }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      fn t() -> Thresholds { Thresholds::default() }

      /// A destination that refuses until `available_at`, then connects `connect_ms` after each swap.
      fn reconnect_time(available_at: Ms, connect_ms: Ms) -> Ms {
          let th = t();
          let mut o = OutputLink::new();
          let mut pending: Option<(Ms, u64)> = None;
          let mut now = 0;
          while now < 120_000 {
              if o.on_tick(now, &th) { pending = Some((now, o.generation())); }
              if let Some((at, gen)) = pending {
                  if now >= at + connect_ms {
                      pending = None;
                      if now >= available_at { o.on_connected(now, gen); return now; }
                      o.on_error(now, gen, &th);
                  }
              }
              now += 33;
          }
          panic!("never reconnected");
      }

      #[test]
      fn reconnects_within_5_s_of_destination_returning() {
          // PDF chaos: destination disconnects for 30 s → output back within 5 s of availability
          let back = reconnect_time(30_000, 400);
          assert!(back - 30_000 <= 5_000, "reconnected {} ms after availability", back - 30_000);
      }
      #[test]
      fn ack_stall_swaps_the_sink_within_5_s() {
          let th = t();
          let mut o = OutputLink::new();
          assert!(o.on_tick(0, &th));
          o.on_connected(100, 1);
          let mut now = 100;
          while now < 10_000 { o.on_ack(now, 1); now += 33; } // healthy until 10 s, then acks stop
          let mut swapped_at = None;
          while now < 20_000 { if o.on_tick(now, &th) { swapped_at = Some(now); break; } now += 33; }
          let at = swapped_at.expect("stall never detected");
          assert!(at - 10_000 <= th.output_ack_stall_ms + 33, "detected after {} ms", at - 10_000);
          assert_eq!(o.generation(), 2, "an ack stall swaps immediately; no back-off wait");
      }
      #[test]
      fn stale_generation_error_is_ignored() {
          let th = t();
          let mut o = OutputLink::new();
          o.on_tick(0, &th);
          o.on_connected(10, 1);
          o.on_error(20, 1, &th);
          while !o.on_tick(1_000, &th) {}
          o.on_connected(1_100, 2);
          o.on_error(1_200, 1, &th); // late error from the old sink
          assert!(o.is_connected());
      }
      #[test]
      fn ack_from_old_generation_does_not_keep_new_sink_alive() {
          let th = t();
          let mut o = OutputLink::new();
          o.on_tick(0, &th);
          o.on_connected(0, 1);
          o.on_error(10, 1, &th);
          while !o.on_tick(600, &th) {}
          o.on_connected(700, 2);
          for now in (700..20_000).step_by(100) { o.on_ack(now, 1); }
          assert!(o.on_tick(700 + th.output_ack_stall_ms, &th), "gen-1 acks must not mask a gen-2 stall");
      }
      #[test]
      fn connect_that_hangs_times_out() {
          let th = t();
          let mut o = OutputLink::new();
          o.on_tick(0, &th);
          o.on_tick(th.output_connect_timeout_ms, &th);
          assert!(matches!(o.state(), OutputState::Backoff { attempt: 1, .. }));
      }
      #[test]
      fn backoff_never_exceeds_the_2_s_cap() {
          let th = t();
          let mut o = OutputLink::new();
          let mut now = 0;
          for _ in 0..10 {
              while !o.on_tick(now, &th) { now += 10; }
              o.on_error(now, o.generation(), &th);
              if let OutputState::Backoff { until, .. } = o.state() { assert!(until - now <= th.output_backoff_cap_ms); }
          }
      }
  }
  ```

- [ ] **Step 6: Run, expect a FAIL.** Then implement:
  ```rust
  pub fn on_connected(&mut self, now: Ms, gen: u64) {
      if gen == self.generation && matches!(self.state, OutputState::Connecting { .. }) {
          self.state = OutputState::Connected { last_ack: now };
      }
  }
  pub fn on_ack(&mut self, now: Ms, gen: u64) {
      if gen != self.generation { return; }
      if let OutputState::Connected { last_ack } = &mut self.state { *last_ack = now; }
  }
  pub fn on_error(&mut self, now: Ms, gen: u64, t: &Thresholds) {
      if gen != self.generation { return; }
      // same counting as InputLink: failure number f waits delay(f - 1)
      let failed_before = match self.state {
          OutputState::Connected { .. } => 0,
          OutputState::Connecting { attempt, .. } => attempt,
          OutputState::Backoff { .. } => return,
      };
      self.state = OutputState::Backoff { attempt: failed_before + 1, until: now + delay(failed_before, t.output_backoff_base_ms, t.output_backoff_cap_ms) };
  }
  pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> bool {
      match self.state {
          OutputState::Backoff { attempt, until } if now >= until => {
              self.generation += 1;
              self.state = OutputState::Connecting { attempt, since: now };
              true
          }
          OutputState::Connecting { attempt, since } if elapsed(now, since) >= t.output_connect_timeout_ms => {
              self.state = OutputState::Backoff { attempt: attempt + 1, until: now + delay(attempt, t.output_backoff_base_ms, t.output_backoff_cap_ms) };
              false
          }
          OutputState::Connected { last_ack } if elapsed(now, last_ack) >= t.output_ack_stall_ms => {
              self.generation += 1;
              self.state = OutputState::Connecting { attempt: 0, since: now };
              true
          }
          _ => false,
      }
  }
  ```

- [ ] **Step 7: Run, expect a PASS.** `cargo test -p domain`. Expected: input_link 12 and
  output_link 6, plus earlier tasks' tests, all passing.

- [ ] **Step 8: Mutate each guard once, and record it in the commit body.**
  1. Remove `if gen != self.generation { return; }` in `InputLink::on_lost`. Expected red:
     `stale_generation_loss_is_ignored`.
  2. Remove the same check in `OutputLink::on_ack`. Expected red:
     `ack_from_old_generation_does_not_keep_new_sink_alive`.
  3. Change `>` to `>=` in `kick_alert`. Expected red: `kick_alert_needs_more_than_three…`.
  4. Delete the `Connected { last_ack }` arm in `OutputLink::on_tick`. Expected red:
     `ack_stall_swaps_the_sink_within_5_s`.
  5. In `check_stall`, replace `f.max(since)` with `f`. Expected red:
     `stall_clock_runs_from_the_later_of_connect_and_last_frame`.

  Revert every mutant.

- [ ] **Step 9: Commit.**
  `git commit -m "feat(domain): input and output link sub-machines with generation guards (C04)"`

## Acceptance

- 18 new tests pass.
- 5 mutants went red.
- Clippy is clean.
- (Amended 2026-10-19, Owed 38.) A refused publish in F-011's measured shapes (CF: 476 B, then
  262 334 B by 538 ms, then an error; mediamtx: 9 growing totals to 1 008 779 B, then an error at
  2 033 ms) is never Connected, and backs off as a failure.
- (Amended 2026-10-19.) Refusals in a row keep growing the back-off (0.5, 1, 2, 2 s); only Connected
  resets it, and an error from Connected is failure 1.
- (Amended 2026-10-19.) Growth past the bound connects at the first growth at least
  `output_connected_after_ms` after the first growth (inclusive), and not 1 ms sooner.
- (Amended 2026-10-19.) A generation change mid-window restarts the proof: the old sink's bytes never
  count toward the new one.
- ~~(Amended 2026-10-19.) Growth that stops before the bound is a stall that counts as a failure, never
  Connected.~~ (Amended 2026-10-24, Owed 87; corrected by the PR #73 review's m-2.) Growth that stops
  before the bound is not Connected while it is silent: the sink's error ends it as a failure, and a
  growth that resumes at or past the bound, counted from the first growth, connects. A silence inside
  the proof is no failure and does not restart it
  (`a_proving_sink_that_grows_again_after_a_silence_connects_at_the_bound`). (Amended 2026-10-25,
  Owed 94: while the encoders starve or are unread; with both delivering, a silence of
  `output_egress_flat_ms` is the backstop's, below.)
- (Amended 2026-10-19.) Every sweep prints its count and fails on 0. Each guard's mutant goes red: the
  bound's `>=`, the reset placement, and the generation checks.
- (Amended 2026-10-24, Owed 87.) No stall rule: `output_stall_after_ms` and `SwapReason::Stall` are
  gone, and no tick ends a Proving or Connected sink, only its error
  (`a_connected_sink_that_sends_nothing_is_swapped_only_by_its_error`,
  `a_proving_sink_that_goes_silent_waits_for_its_error`). A destination stall swaps one base back-off
  after the sink's own error at `OUTPUT_SINK_TIMEOUT_MS` (`a_destination_stall_swaps_on_the_sinks_timeout_error`).
  Each mutant went red: a stall path re-added in Proving, one in Connected, and the constant at 20 s.
  (Amended 2026-10-25, Owed 94; the PR #76 review's I-2.) True only while the encoders starve or are
  unread: those two tests sent no encoder reading, which production always sends. They now send
  starving readings (the totals repeat) and are renamed
  `a_connected_sink_that_sends_nothing_while_the_encoders_starve_is_swapped_only_by_its_error` and
  `a_proving_sink_that_goes_silent_while_the_encoders_starve_waits_for_its_error`. With both encoders
  delivering, the backstop ends a flat sink (next bullet). And "a destination stall swaps on the
  sink's error" holds when one socket wait outlasts the timeout; a multi-wake trickle never errors
  (the PR #73 review's P-3).
- (Amended 2026-10-25, Owed 94.) The egress backstop, not a stall rule. Owner, 2026-10-24, in chat,
  relayed by the coordinator: "B". A Proving or Connected sink whose total has not grown for
  `output_egress_flat_ms` while both encoded tracks grew at every reading backs off with
  `SwapReason::EgressStalled` (from Connected it is failure 1, waiting the base; from Proving the
  failures before it count). A starved
  reading restarts the clock, each new sink starts it again, and another generation's readings never
  touch it (`a_hung_output_is_swapped_by_the_egress_backstop_while_the_encoders_deliver`,
  `a_proving_sink_that_stops_growing_is_swapped_too_and_carries_its_failures`,
  `the_backstop_never_counts_while_the_encoders_starve`, `a_starved_reading_restarts_the_backstops_clock`,
  `the_backstop_restarts_with_each_sink_and_ignores_other_generations`). A destination stall still swaps
  on the sink's error (`a_destination_stall_still_swaps_on_the_sinks_error_with_the_encoders_delivering`).
  See the amendment below.

## Amendment 2026-10-25 (Owed 94: the egress backstop)

**Why.** Owed 87 retired the stall rule (owner, 2026-10-24, in chat, relayed by the coordinator: "A"),
which left one gap: an output pipeline that hangs while the encoders keep delivering never raises a bus
error, so nothing swapped it. The coordinator asked the owner: (A) accept and record the gap, or (B) add
a backstop, "no egress bytes for about 15 s while the encoders deliver", matching the 15 s outage
target. Owner, 2026-10-24, in chat, relayed by the coordinator: "B".

**As built.**
- `config.rs`: `output_egress_flat_ms(backoff_base_ms, connect_timeout_ms)`, one derived constant: the
  15 s gap budget less one connect's worst case (the connect timeout; not a failed reconnect: the hung
  path fits with no failed connect, as the stall path does), the base back-off, one progress poll and
  two widest ticks, floored to the poll. With today's constants that is 9 000 ms. The owner,
  2026-10-25, in chat, relayed by the coordinator ("Apply rec", the coordinator's recommendation of
  the derived value): 9 s, keeps the worst outage under 15 s. `OUTPUT_PROGRESS_POLL_MS` and `TICK_WIDEST_MS` move
  into `domain::config` so the derivation reads them (adapters re-exports the poll). `Thresholds` gains
  `output_egress_flat_ms`.
- Two assertions from independent sums: `the_egress_backstop_is_the_longest_threshold_that_keeps_the_15_s_budget`
  (the worst hung gap stays inside the budget, and one poll more would not) and
  `a_socket_wait_past_the_timeout_errors_before_the_backstop` (the threshold sits above the sink
  timeout plus a poll and a tick; renamed from `a_destination_stall_reaches_the_sinks_error_before_the_backstop`,
  since a multi-wake trickle never errors). The poll's pin moved with it: `the_progress_poll_is_owed_15s_250_ms`.
  `output_recovery_paths_against_the_f004_budget_on_a_1_ms_grid` pins 0 failed connects after a hang
  (the PR #76 review's m-1).
- `OutputLink::on_delivery(now, gen, Encoded)`: a reading whose two encoded totals both grew keeps
  `delivering_since`; any other reading clears it. `egress_flat` reads elapsed(now, max(grew_at,
  delivering_since)), and a build clears the egress, so the new sink's clock starts at its first growth.
- The supervisor's `OutputProgress` carries `encoded`; the beat stops reading `playing` the tick the
  backstop fires (`a_hung_output_is_swapped_by_the_backstop_and_the_beat_stops_reading_playing`).
- `links.rs` sweeps: `the_egress_backstop_ends_only_a_flowing_sink_and_only_while_the_encoders_deliver`
  (the delivery/egress pairs) and, for the PR #73 review's P-3,
  `a_multi_wake_trickle_is_swapped_by_the_backstop_only_where_no_socket_wait_reaches_the_timeout`
  (26 rows at 9 000 ms: 10 errored at +5 508 ms, 12 swapped by the backstop at +9 520 ms, 4 never;
  the tally is no longer pinned, so it moves with the threshold, the PR #76 review's m-4). A trickle
  whose every socket wait is under the sink timeout was never swapped before; the backstop now swaps it,
  with the reason `egress_stalled`, not `error`. When the trickle persists (the review's m-2,
  `a_persistent_multi_wake_trickle_swaps_every_later_sink_from_proving_and_none_connects`), every later
  sink is swapped from Proving too, never Connected, its back-off climbing to the cap: builds every
  10.3, 11.2, 11.3, 11.3 s, the beat never `playing`, where before one Connected sink read `playing`
  over one frame per message.
- `app::run` (the review's m-5) steps the messages already queued before it ticks, so a driver
  thread that stalled past the threshold reads the queued growth before the backstop judges it
  (`a_driver_stalled_past_the_backstop_reads_its_queued_readings_before_it_ticks`; red before:
  `[Backoff, Backoff]`).
- Mutants D1 to D7, each red with named tests: the threshold below the sink timeout (4 reds, the two
  ordering/budget assertions among them), counting during starvation (5), no swap from Connected (6),
  the supervisor dropping the encoded counts (1), the egress not cleared at a build
  (`the_backstop_restarts_with_each_sink_and_ignores_other_generations`), no swap from Proving
  (`a_proving_sink_that_stops_growing_is_swapped_too_and_carries_its_failures`), and a starved reading
  not restarting the clock (2). Fix round 1 (the PR #76 review): D8, a repeated encoded total counted as
  delivery, reds both renamed starvation tests and two more (4); A1, `run()` stepping nothing before
  it ticks, reds the stalled-driver scenario.

## Amendment 2026-10-19 (Owed 38: Connected is earned by sustained growth)

**Why.** C09's live legs (F-011) found that rtmp2sink's first nonzero `out-bytes-total` is the 476 B
RTMP `connect` command (22–69 ms after the build, n = 10 on CF). A refused publish then keeps
counting until its refusal arrives: 262 334 B by 538 ms on CF (a made-up key) and 1 008 779 B over
2 033 ms on mediamtx (a denied path). Under the rule as first built ("Connected is the first progress
after a build"; "a first progress resets" the back-off), a wrong stream key was Connected, reset the
back-off to attempt 0, and retried at the base back-off for the whole session. C04's own unit test
`a_first_progress_resets_the_backoff_to_its_base` had frozen that rule as its expected value.

**The coordinator's ruling** (medium, under the owner's standing medium delegation; relayed in the
C04b dispatch). Connected is earned by SUSTAINED progress, not by the first bytes:
- an OutputLink becomes Connected only after `OutputProgress` totals have kept growing for
  `output_connected_after_ms`, with no `OutputError` for that generation;
- the default comes from F-011's data, with margin over the measured refusals (5 000 ms);
- the back-off resets to attempt 0 only on Connected;
- an error before Connected counts as a failed attempt, so the back-off grows;
- the stall rule is unchanged.

**As built (PR #29).**
- **A fourth state: `OutputState::Proving { attempt, first_progress, last_progress, sent }`.** The
  first growth after a build moves Connecting to Proving, and never connects. The sink is Connected
  at the first growth at least `output_connected_after_ms` after its first growth (the bound is
  inclusive). Connected is decided on a growth, never on a tick, so it lands in the final hold too,
  where no link is ticked.
- **`Thresholds::output_connected_after_ms` = 5 000** (`config.rs`, cited to F-011). That is about
  2.5× the slowest refusal measured (2 033 ms on mediamtx) and over 9× CF's (538 ms). It is no
  longer, because the first `playing` beat and the back-off's reset both wait on Connected.
- **The connect timeout runs from the build to the first growth.** Once bytes flow, the stall rule
  takes over. Otherwise a healthy sink, first growth at 31 ms, would time out at 5 000 ms, before its
  proof ends at 5 031 ms.
- **Every end before Connected is one more failure**: a connect that times out, an error, or no growth
  for `output_stall_after_ms` while proving. The last one backs off; it is not a swap. **Only
  Connected resets the count**: an error from Connected is failure 1, and a Connected sink's stall
  still swaps at once with attempt 0. The stall rule for a Connected sink is unchanged.
- **`OutputLink::on_progress(now, gen, total_bytes, t)` takes the thresholds.** C06's supervisor
  passes `&self.t`, and reports a proving sink as `LinkStatus::Connecting`. Its beat stays `starting`
  (or `stalled`, after an earlier connect) until Connected. No event, effect, port or contract changed.
- **The 15 s budget** (`tests/links.rs`, `recover`) still ends a recovery at the new sink's first
  bytes, when its output is back, not at its Connected.
- **Consequences** (Owed 42): the first `playing` beat comes at least 5 s after the first bytes; each
  swap reads `stalled` until the new sink earns Connected; a refused stream key never reads `playing`,
  and its retries slow to the 2 s cap.

**Tests.** These are added to the brief's acceptance below.
- Unit tests in `output_link.rs`:
  - F-011's two refusals, as measured;
  - four refusals in a row (waits 0.5, 1, 2, 2 s);
  - a CF-shaped healthy publish that connects at the bound and resets the count;
  - the bound's edge, 1 ms short and exact, under default and distinct thresholds;
  - the connect timeout ending at the first growth;
  - a generation change mid-window: a refusal, a stall swap, and a generation never built;
  - growth that stops before the bound (a stall that is a failure);
  - a repeated total while proving.
- In `supervisor.rs`: a refused stream key, beat by beat, for 40 s.
- In `tests/links.rs`: the pair matrix gains Proving, and the failures each state carries. The
  generated sweep gains Proving's rules and counters.
- In `tests/output_connected.rs`: a proptest property over random refusals, random thresholds and
  random tick grids. Its oracle is written from the rule's text.

## Amendment 2026-10-20 (C10b: the link reports its own give-ups; Owed 50)

- **`InputLink::on_tick` and `check_stall` return `Option<InputAction>`** (was `bool` and `()`):
  `Rebuild { gen }` for a due back-off (the old `true`), `Abandon { gen }` for the two rules that are
  the link's own decision, a connect past `input_connect_timeout_ms` and a Running link with no frame
  for `input_stall_after_ms`. The abandon names the generation given up; the link then backs off, so
  each generation is given up at most once. A loss the adapter reported (`on_lost`) returns nothing:
  the adapter decided it. `OutputLink` is unchanged.
- **`InputLink::takes_frames_from(gen)`**: the current generation, not Backoff (Connecting counts.
  The adapter never sends a build's frames ahead of its `InputConnected`: one streaming thread sends
  both, that one first, on a FIFO channel. A frame reaches a Connecting link first only when the
  `InputConnected` was refused for a full channel, to be tried again on the next sample, and that
  frame is the build's own. Corrected by the PR #35 review's m3: the first wording, "they race on one
  channel", named a race that cannot happen).
- Tests: the inline ones above read the action; `a_generation_is_given_up_once_by_either_rule`,
  `a_loss_the_adapter_reported_is_never_abandoned`, `an_abandon_names_the_generation_it_gave_up`,
  `frames_count_only_from_the_current_live_generation`; `tests/links.rs`'s pair matrix has an abandon
  column and its generated schedules check every abandon.

## Amendment 2026-10-24 (Owed 87: the stall rule is retired)

**The ruling.** Owner, 2026-10-24, in chat, relayed by the coordinator: "A", choosing between the coordinator's (A) retire
the stall rule, so rtmp2sink's own socket timeout is the only detector of a destination stall, and
(B) keep the rule and raise the sink's timeout (`docs/_INDEX.md` Owed 87, decision log "Owed 87
decided").

**Why.** On CI, rtmp2sink's own 5 s socket timeout swapped 17 of 17 destination stalls; the rule
won once, by about 1 ms. rtmp2 counts bytes only when a whole message is written, so "no growth for
5 s" and "one 5 s socket wait" start at the same write.

**As built (branch `feat/owed87-retire-stall`).**
- `OutputState` is `Connecting { attempt, since }`, `Proving { attempt, first_progress, sent }`,
  `Connected` and `Backoff { attempt, until }`. No state keeps a last progress. Proving still earns
  Connected only by growth (Owed 38), and a repeated or backwards total is no progress. A silence
  inside the proof no longer fails it: the first growth at or past the bound connects, however long
  the sink was silent before it (the PR #73 review's m-2). (Amended 2026-10-25, Owed 94: however long
  while the encoders starve or are unread; with both delivering, a silence of `output_egress_flat_ms`
  is the backstop's.)
- `on_tick` keeps two timers: a due back-off builds, and a Connecting sink past
  `output_connect_timeout_ms` backs off (`SwapReason::ConnectTimeout`). Proving and Connected have
  no timer: only `on_error` ends them (`SwapReason::Error`; from Connected it is failure 1, so the
  base back-off comes first). (Amended 2026-10-25, Owed 94: since the backstop, Proving and Connected
  have one more timer, `output_egress_flat_ms` of flat egress while both encoders deliver.)
- `Thresholds::output_stall_after_ms` and `SwapReason::Stall` are removed. `config::OUTPUT_SINK_TIMEOUT_MS`
  (5 000, GStreamer 1.24.2's rtmp2 default) is the adapter's sink timeout (C09's amendment of this
  date). The domain reads it only in tests, to time a stall's error.
- Owed 38's amendment above, read again: "no growth for `output_stall_after_ms` while proving" is now
  the sink's error while proving, still a failure; "a Connected sink's stall still swaps at once with
  attempt 0" is now an error from Connected, failure 1.
- The 15 s budget (`tests/links.rs`): a stall's recovery from the last bytes is the sink's timeout
  plus the recovery from an error, 10 499 ms, then 16 499 ms through one failed connect.
- **Not covered any more** (the implementer's note): a sink that sends nothing and never errors stays
  Connected. (Amended 2026-10-25: while the encoders starve; with both delivering, Owed 94's backstop
  swaps it.)

## Amendment 2026-10-27 (Owed 108 fix (a), F-032: Cloudflare's hold-end close)

- **`InputLink::on_error(now, gen, facts, last_frame_at, t)`**: an error on the current build, or the
  first one on the last build the link gave up while it still runs. `facts` is `BuildFacts {
  handshake_completed, carried_media }`, read by the adapter off the build itself.
- **The rule.** Between builds (Connecting or Backoff), an error on a build whose handshake completed
  and that never carried media, at least `Thresholds::input_hold_end_after_ms` (20 000) after the last
  frame, is Cloudflare's hold-end close: the link backs off a base wait with the attempt count reset,
  and never later than a build already due. Bound: once an outage (`hold_end_armed`, spent by the
  rebuild), re-armed only when a generation reaches Running, disarmed until the link's first Running.
  Any other error is `on_lost(Error)`, unchanged: a loss while Running or Connecting, nothing in
  Backoff.
- Tests: the inline ones under "Owed 108 fix (a)" (each guard has a case where only it decides);
  `tests/hold_end.rs` (4 tick grids, 12 cases; a phone that never returns costs a constant 1 to 2
  extra connects when connects time out and 2 to 3 when they error, over 3 362 horizons, derived from
  `backoff::delay`; and the gate's anchor, the last frame, with the camera on air three gates before
  the drop, 4 more tick grids); `tests/links.rs`'s input column gains `InputEvent::Error` (21 pairs,
  33 no-ops).

## Amendment 2026-10-27 (Owed 108 fix (b), F-032: the phone is live again)

- **`InputLink::on_phone_live_edge(count)`**: web's phone-live edge counter from a heartbeat reply.
  The first value the link sees is a baseline (a boot and an in-place restart act on nothing); each
  later increase latches ONE nudge, however far the value rose; a lower value latches nothing and
  becomes the value the next one is read against.
- **`InputLink::on_media(gen)`**: the current generation's first byte out of its source. Every build
  clears it.
- **`InputLink::check_nudge(now, t)`**, on each tick before `on_tick`, and `back_off` at the link's
  next failure: the latched nudge makes the build due now with the attempt count reset (the state of a
  first failure), never later than a build already due. In Backoff the tick builds it. Connecting
  with no byte yet, the connect is given up (`Abandon`) and rebuilt now, once an outage (re-armed
  when a generation reaches Running, armed from the start). Running, or a connect carrying bytes, or
  the outage's discard spent, it stays latched and acts at the stall, the loss or the connect timeout.
  The nudge clears when it acts or when a generation reaches Running. A nudge and fix (a)'s close in
  one outage build once.
- **No 1 s wait.** The nudge builds now, as F-032's replay's `nudge` policy does; fix (a)'s close
  keeps its base wait.
- Tests: `tests/phone_live.rs` (4 tick grids, 12 cases, waits from `backoff::delay`);
  `tests/links.rs`'s input column gains `InputEvent::Media` (24 pairs, 39 no-ops).

## Do not touch

`config.rs` values, `programme.rs`. (Amended 2026-10-19: Owed 38's ruling adds
`output_connected_after_ms`; every existing value is unchanged.) (Amended 2026-10-24: Owed 87's
ruling removes `output_stall_after_ms` and adds `OUTPUT_SINK_TIMEOUT_MS`; every other value is
unchanged.)

## Output cap

Under 15 lines.
