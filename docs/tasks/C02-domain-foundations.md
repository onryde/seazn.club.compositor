# C02: Domain foundations: time, config, backoff, window

**Repo:** `seazn.club.compositor`. **Depends on:** C01.

## Context

Every sub-machine needs four primitives:
- a time type
- the single number authority (`Thresholds`, `OutputProfile`)
- capped exponential backoff
- a **bounded** sliding window of timestamps, for "N events in M minutes" rules

All four are pure.

## Files

- Create: `crates/domain/src/{time.rs,config.rs,backoff.rs,window.rs}`
- Modify: `crates/domain/src/lib.rs` (add `pub mod` lines)
- Test: unit tests inline (`#[cfg(test)] mod tests`) in each file

## Interfaces

**Produces** (later tasks rely on these exact names):
```rust
// time.rs
pub type Ms = u64;
pub fn elapsed(now: Ms, since: Ms) -> Ms;              // saturating

// config.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thresholds { /* fields listed in step 3 */ }
impl Default for Thresholds;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputProfile { pub width: u32, pub height: u32, pub fps: u32, pub video_kbps: u32 }
impl OutputProfile { pub const P720: Self; pub const P1080: Self; pub fn frame_period_ms(&self) -> f64; }

// backoff.rs
pub fn delay(attempt: u32, base: Ms, cap: Ms) -> Ms;

// window.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window { /* private */ }
impl Window {
    pub fn new(cap: usize) -> Self;
    pub fn record(&mut self, now: Ms, span: Ms) -> usize;   // count within span, including this one
    pub fn count(&self, now: Ms, span: Ms) -> usize;
}
```

## Steps

- [ ] **Step 1: Write failing tests for `time` and `backoff`.**

  `crates/domain/src/time.rs`:
  ```rust
  //! Domain time: milliseconds on a monotonic clock that the app layer supplies. The domain never reads a clock.
  pub type Ms = u64;

  /// `now - since`, saturating at 0 so a reordered timestamp can never underflow into a huge age.
  pub fn elapsed(now: Ms, since: Ms) -> Ms {
      now.saturating_sub(since)
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      #[test]
      fn elapsed_is_difference() { assert_eq!(elapsed(1_500, 1_000), 500); }
      #[test]
      fn elapsed_saturates_when_since_is_later() { assert_eq!(elapsed(1_000, 1_500), 0); }
  }
  ```

  `crates/domain/src/backoff.rs`, tests first:
  ```rust
  //! Capped exponential backoff: base · 2^attempt, clamped to `cap`.
  use crate::time::Ms;

  pub fn delay(attempt: u32, base: Ms, cap: Ms) -> Ms {
      todo!()
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      // PDF failure table: "reconnects forever with back-off (1 s → 10 s)".
      #[test]
      fn input_sequence_is_1_2_4_8_then_capped_at_10_s() {
          let got: Vec<Ms> = (0..7).map(|a| delay(a, 1_000, 10_000)).collect();
          assert_eq!(got, vec![1_000, 2_000, 4_000, 8_000, 10_000, 10_000, 10_000]);
      }
      #[test]
      fn huge_attempt_never_overflows() { assert_eq!(delay(u32::MAX, 1_000, 10_000), 10_000); }
      #[test]
      fn zero_base_is_zero() { assert_eq!(delay(5, 0, 10_000), 0); }
  }
  ```
  Add `pub mod time; pub mod backoff;` to `lib.rs`.

- [ ] **Step 2: Run, expect a FAIL.** `cargo test -p domain backoff`. Expected: panics at `todo!()`.
  (C01 denies `clippy::todo`, which is fine here: this step runs tests, not clippy.)

- [ ] **Step 3: Implement backoff, and write `config.rs`.**
  ```rust
  pub fn delay(attempt: u32, base: Ms, cap: Ms) -> Ms {
      let factor = 1u64.checked_shl(attempt.min(32)).unwrap_or(u64::MAX);
      base.saturating_mul(factor).min(cap)
  }
  ```

  `crates/domain/src/config.rs`:
  ```rust
  //! THE number authority. Every tunable lives here, each with the source that set it.
  //! "PDF" = Live Sports Streaming Compositor — Production Design & Specification (2026-10-03).
  //! A spike measurement (F-id in `_FINDINGS.md`) overrides a PDF value and must be cited.
  use crate::time::Ms;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Thresholds {
      /// PDF robustness state machine: "no frame 0.5 s" → Frozen.
      pub frozen_after_ms: Ms,
      /// PDF state machine "no frame 3 s" → Reconnecting slate; timing budget "slate ≤ 3.1 s".
      pub slate_after_ms: Ms,
      /// PDF: "a return to live requires 1 s of continuous frames".
      pub resume_after_ms: Ms,
      /// PDF failure table: input back-off 1 s → 10 s.
      pub input_backoff_base_ms: Ms,
      pub input_backoff_cap_ms: Ms,
      /// Spec §4.2: a connect that neither succeeds nor fails within this is treated as lost.
      pub input_connect_timeout_ms: Ms,
      /// Spec §6 "reconnect forever": a Running input that delivers no frame for this long is treated as
      /// a Stalled loss and rebuilt (CF can hold the playback socket open after the phone drops). Set after
      /// the 3 s slate so the viewer sees the slate first; re-pinned by F-002's reconnect measurement.
      pub input_stall_after_ms: Ms,
      /// PDF failure table: "alert if it repeats > 3 times in 5 min" (alert when count EXCEEDS this).
      pub kick_alert_count: usize,
      pub kick_window_ms: Ms,
      /// PDF chaos "destination accepts but stalls: detected within 5 s".
      pub output_ack_stall_ms: Ms,
      /// PDF budget "output reconnect ≤ 5 s": back-off 0.5 s → 2 s keeps attempts inside it.
      pub output_backoff_base_ms: Ms,
      pub output_backoff_cap_ms: Ms,
      pub output_connect_timeout_ms: Ms,
      /// Spec §4.2: 3 CEF failures in 5 min → graphics Failed.
      pub cef_fail_count: usize,
      pub cef_fail_window_ms: Ms,
      /// PDF page contract "loads in under 2 s"; ×5 margin before it counts as a failure.
      pub cef_load_timeout_ms: Ms,
      /// Spec §6: CEF recycled every 2 h with its last frame held.
      pub cef_recycle_every_ms: Ms,
      pub cef_backoff_base_ms: Ms,
      pub cef_backoff_cap_ms: Ms,
      /// PDF failure table "real time restored within 10 s": at most one ladder rung per this gap.
      pub ladder_step_min_gap_ms: Ms,
      /// Overruns within `ladder_window_ms` needed to step down one rung.
      pub ladder_overruns_to_step: usize,
      pub ladder_window_ms: Ms,
      /// R1 contract: on ending/SIGTERM flush and exit within 10 s.
      pub drain_timeout_ms: Ms,
      /// Spec §5.3 default; the bootstrap's `heartbeatIntervalMs` overrides it.
      pub heartbeat_interval_ms: Ms,
      /// Bootstrap GET retry back-off (web briefly unreachable at boot).
      pub bootstrap_backoff_base_ms: Ms,
      pub bootstrap_backoff_cap_ms: Ms,
  }

  impl Default for Thresholds {
      fn default() -> Self {
          Self {
              frozen_after_ms: 500,
              slate_after_ms: 3_000,
              resume_after_ms: 1_000,
              input_backoff_base_ms: 1_000,
              input_backoff_cap_ms: 10_000,
              input_connect_timeout_ms: 5_000,
              input_stall_after_ms: 5_000,
              kick_alert_count: 3,
              kick_window_ms: 300_000,
              output_ack_stall_ms: 5_000,
              output_backoff_base_ms: 500,
              output_backoff_cap_ms: 2_000,
              output_connect_timeout_ms: 5_000,
              cef_fail_count: 3,
              cef_fail_window_ms: 300_000,
              cef_load_timeout_ms: 10_000,
              cef_recycle_every_ms: 7_200_000,
              cef_backoff_base_ms: 1_000,
              cef_backoff_cap_ms: 10_000,
              ladder_step_min_gap_ms: 10_000,
              ladder_overruns_to_step: 3,
              ladder_window_ms: 1_000,
              drain_timeout_ms: 10_000,
              heartbeat_interval_ms: 5_000,
              bootstrap_backoff_base_ms: 1_000,
              bootstrap_backoff_cap_ms: 10_000,
          }
      }
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct OutputProfile { pub width: u32, pub height: u32, pub fps: u32, pub video_kbps: u32 }

  impl OutputProfile {
      /// ADR 0005 default. 4000 kbps = top of YouTube's 720p30 range; re-pinned by F-005.
      pub const P720: Self = Self { width: 1280, height: 720, fps: 30, video_kbps: 4_000 };
      /// ADR 0005 flag. 6000 kbps = the POC default; re-pinned by F-005.
      pub const P1080: Self = Self { width: 1920, height: 1080, fps: 30, video_kbps: 6_000 };
      pub fn frame_period_ms(&self) -> f64 { 1_000.0 / f64::from(self.fps) }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      // These pin the declarations to their SOURCES (PDF / ADR), not to the code that reads them.
      #[test]
      fn programme_thresholds_match_the_pdf_state_machine() {
          let t = Thresholds::default();
          assert_eq!((t.frozen_after_ms, t.slate_after_ms, t.resume_after_ms), (500, 3_000, 1_000));
      }
      #[test]
      fn slate_lands_inside_the_3_1_s_budget_with_one_frame_of_tick_jitter() {
          let t = Thresholds::default();
          assert!(t.slate_after_ms as f64 + OutputProfile::P720.frame_period_ms() <= 3_100.0);
      }
      #[test]
      fn input_stall_rebuild_comes_after_the_slate() {
          let t = Thresholds::default();
          assert!(t.input_stall_after_ms > t.slate_after_ms, "viewers see the slate before the input is torn down");
      }
      #[test]
      fn output_backoff_cap_fits_inside_the_5_s_reconnect_budget() {
          let t = Thresholds::default();
          assert!(t.output_backoff_cap_ms < 5_000);
      }
      #[test]
      fn profiles_are_adr_0005() {
          assert_eq!((OutputProfile::P720.width, OutputProfile::P720.height, OutputProfile::P720.fps), (1280, 720, 30));
          assert_eq!((OutputProfile::P1080.width, OutputProfile::P1080.height), (1920, 1080));
      }
  }
  ```

- [ ] **Step 4: Window: write the failing tests.** `crates/domain/src/window.rs`:
  ```rust
  //! Bounded sliding window of timestamps: "N events within span". Never grows past `cap`.
  use crate::time::{elapsed, Ms};
  use std::collections::VecDeque;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Window { stamps: VecDeque<Ms>, cap: usize }

  impl Window {
      pub fn new(cap: usize) -> Self { todo!() }
      pub fn record(&mut self, now: Ms, span: Ms) -> usize { todo!() }
      pub fn count(&self, now: Ms, span: Ms) -> usize { todo!() }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      #[test]
      fn counts_only_inside_span() {
          let mut w = Window::new(16);
          w.record(0, 300_000);
          w.record(100_000, 300_000);
          assert_eq!(w.record(299_999, 300_000), 3);
          // at 300_001 the stamp at 0 is outside the 5-min span
          assert_eq!(w.count(300_001, 300_000), 2);
      }
      #[test]
      fn empty_window_counts_zero() { assert_eq!(Window::new(4).count(10, 10), 0); }
      #[test]
      fn never_exceeds_cap() {
          let mut w = Window::new(4);
          for i in 0..100 { w.record(i, 1_000_000); }
          assert_eq!(w.count(100, 1_000_000), 4, "bounded: oldest dropped beyond cap");
      }
      #[test]
      fn zero_cap_is_clamped_to_one() {
          let mut w = Window::new(0);
          assert_eq!(w.record(5, 10), 1);
      }
  }
  ```

- [ ] **Step 5: Run, expect a FAIL.** `cargo test -p domain window`. Expected: `todo!()` panics.

- [ ] **Step 6: Implement Window.**
  ```rust
  impl Window {
      pub fn new(cap: usize) -> Self { Self { stamps: VecDeque::with_capacity(cap.max(1)), cap: cap.max(1) } }
      fn prune(&mut self, now: Ms, span: Ms) {
          while self.stamps.front().is_some_and(|&s| elapsed(now, s) > span) {
              self.stamps.pop_front();
          }
      }
      pub fn record(&mut self, now: Ms, span: Ms) -> usize {
          self.prune(now, span);
          if self.stamps.len() == self.cap { self.stamps.pop_front(); }
          self.stamps.push_back(now);
          self.stamps.len()
      }
      pub fn count(&self, now: Ms, span: Ms) -> usize {
          self.stamps.iter().filter(|&&s| elapsed(now, s) <= span).count()
      }
  }
  ```
  Add `pub mod config; pub mod window;` to `lib.rs`.

- [ ] **Step 7: Run, expect a PASS.** `cargo test -p domain`. Expected: 14 passed in total
  (time 2, backoff 3, config 5, window 4), plus C01's 2 guards.

- [ ] **Step 8: Mutate once.** Change `elapsed(now, s) > span` to `>= span` in `prune`. Expected:
  `counts_only_inside_span` stays green but its boundary moves, so this mutant *survives*. That's
  why you should add the boundary test below, then confirm the mutant goes red:
  ```rust
  #[test]
  fn stamp_exactly_span_old_still_counts() {
      let mut w = Window::new(4);
      w.record(0, 100);
      assert_eq!(w.count(100, 100), 1);
      assert_eq!(w.record(100, 100), 2, "a stamp exactly `span` old is still inside");
  }
  ```
  Revert the mutant. Record both runs in the commit body.

- [ ] **Step 9: Commit.**
  ```bash
  cargo clippy -p domain --all-targets -- -D warnings && cargo test -p domain
  git add crates/domain && git commit -m "feat(domain): time, thresholds, backoff and bounded window (C02)"
  ```

## Acceptance

`cargo test -p domain` reports 17 passed: 15 unit tests (the 14 from step 7 plus the boundary test
from step 8) and the 2 guards. Clippy is clean. The mutation is recorded.

## Do not touch

Other crates. Do not add any dependency to `domain`.

## Output cap

Under 15 lines.
