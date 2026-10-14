# C03: Domain Programme sub-machine

**Repo:** `seazn.club.compositor`. **Depends on:** C02.

## Context

The programme always has a frame to encode (PDF rank 1). On every output tick, this machine reads
the input's frame timestamps and picks a `FrameChoice`:

- `Emit` (newest camera frame)
- `Repeat` (frozen last frame)
- `Slate(Waiting|Reconnecting)`

The rules come from the PDF's robustness state machine and are encoded **as a table**:

```
Waiting --1 s continuous--> Live --0.5 s no frame--> Frozen --3 s no frame--> Reconnecting
                             ^ <------frames resume----- |                        |
                             +-----------------1 s continuous---------------------+
```

The "Repeat up to 1 s" sentence in the PDF's scheduler paragraph is superseded by the state machine.
See the spec §3.3 refinement.

## Files

- Create: `crates/domain/src/programme.rs`
- Modify: `crates/domain/src/lib.rs` (add `pub mod programme;`)

## Interfaces

- **Consumes:** `time::{Ms, elapsed}`, `config::{Thresholds, OutputProfile}` (C02).
- **Produces:**
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum ProgrammePhase { Waiting, Live, Frozen, Reconnecting }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum SlateKind { Waiting, Reconnecting }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum FrameChoice { Emit, Repeat, Slate(SlateKind) }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum Feed { Continuous, Fresh, Gap, LongGap }
  pub const PROGRAMME_TABLE: &[(ProgrammePhase, Feed, ProgrammePhase)];
  pub fn next_phase(phase: ProgrammePhase, feed: Feed) -> ProgrammePhase;
  pub fn choice_for(phase: ProgrammePhase) -> FrameChoice;
  #[derive(Debug, Clone, PartialEq, Eq, Default)] pub struct Programme { /* private */ }
  impl Programme {
      pub fn phase(&self) -> ProgrammePhase;
      pub fn last_frame_at(&self) -> Option<Ms>;   // C06's input stall rule reads it
      pub fn on_frame(&mut self, now: Ms, t: &Thresholds);
      pub fn feed(&self, now: Ms, t: &Thresholds) -> Feed;
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> FrameChoice;
  }
  ```

## Steps

- [ ] **Step 1: Write the module with `todo!()` bodies, plus the failing tests.**

  `crates/domain/src/programme.rs`:
  ```rust
  //! Programme sub-machine: what the encoder gets on every tick. Table-driven (PDF robustness state machine).
  use crate::config::Thresholds;
  use crate::time::{elapsed, Ms};
  use FrameChoice::*;
  use ProgrammePhase::*;

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum ProgrammePhase { Waiting, Live, Frozen, Reconnecting }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum SlateKind { Waiting, Reconnecting }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum FrameChoice { Emit, Repeat, Slate(SlateKind) }

  /// The input as seen at one tick, derived from frame arrival times.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum Feed {
      /// Latest frame ≤ frozen_after old AND the unbroken run is ≥ resume_after long.
      Continuous,
      /// Latest frame ≤ frozen_after old, run shorter than resume_after.
      Fresh,
      /// frozen_after < age < slate_after.
      Gap,
      /// age ≥ slate_after, or no frame ever.
      LongGap,
  }

  /// (from, feed) → to. Any pair not listed keeps its phase.
  pub const PROGRAMME_TABLE: &[(ProgrammePhase, Feed, ProgrammePhase)] = &[
      (Waiting, Feed::Continuous, Live),
      (Live, Feed::Gap, Frozen),
      (Live, Feed::LongGap, Reconnecting), // a stalled tick thread can skip Frozen entirely
      (Frozen, Feed::Fresh, Live),
      (Frozen, Feed::Continuous, Live),
      (Frozen, Feed::LongGap, Reconnecting),
      (Reconnecting, Feed::Continuous, Live),
  ];

  pub fn next_phase(phase: ProgrammePhase, feed: Feed) -> ProgrammePhase { todo!() }
  pub fn choice_for(phase: ProgrammePhase) -> FrameChoice { todo!() }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Programme { phase: ProgrammePhase, last_frame_at: Option<Ms>, run_since: Option<Ms> }

  impl Default for Programme {
      fn default() -> Self { Self { phase: Waiting, last_frame_at: None, run_since: None } }
  }

  impl Programme {
      pub fn phase(&self) -> ProgrammePhase { self.phase }
      pub fn last_frame_at(&self) -> Option<Ms> { self.last_frame_at }
      pub fn on_frame(&mut self, now: Ms, t: &Thresholds) { todo!() }
      pub fn feed(&self, now: Ms, t: &Thresholds) -> Feed { todo!() }
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> FrameChoice { todo!() }
  }
  ```

  Tests (same file, `#[cfg(test)] mod tests`). The helper drives frames and ticks at the 720p30 frame
  period:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::config::OutputProfile;
      use std::collections::HashSet;

      fn t() -> Thresholds { Thresholds::default() }
      fn period() -> Ms { OutputProfile::P720.frame_period_ms().round() as Ms } // 33

      /// Frames arrive at every period inside any of `frames_in` ranges; a tick runs every period.
      /// Returns (time, choice) per tick from 0 to `until`.
      fn drive(frames_in: &[(Ms, Ms)], until: Ms) -> Vec<(Ms, FrameChoice)> {
          let (th, p) = (t(), period());
          let mut m = Programme::default();
          let mut out = Vec::new();
          let mut now = 0;
          while now <= until {
              if frames_in.iter().any(|&(a, b)| now >= a && now < b) { m.on_frame(now, &th); }
              out.push((now, m.on_tick(now, &th)));
              now += p;
          }
          out
      }
      fn first(out: &[(Ms, FrameChoice)], from: Ms, want: FrameChoice) -> Ms {
          out.iter().find(|&&(at, c)| at >= from && c == want).map(|&(at, _)| at).expect("choice never reached")
      }

      // ---- the table itself (source: PDF robustness diagram) ----
      #[test]
      fn table_has_no_duplicate_keys() {
          let keys: HashSet<_> = PROGRAMME_TABLE.iter().map(|&(f, s, _)| (f, s)).collect();
          assert_eq!(keys.len(), PROGRAMME_TABLE.len());
      }
      #[test]
      fn every_phase_is_reachable_from_waiting() {
          let mut seen = HashSet::from([Waiting]);
          loop {
              let before = seen.len();
              for &(f, _, to) in PROGRAMME_TABLE { if seen.contains(&f) { seen.insert(to); } }
              if seen.len() == before { break; }
          }
          assert_eq!(seen.len(), 4, "reachable: {seen:?}");
      }
      #[test]
      fn waiting_never_jumps_to_reconnecting() {
          // never-live input stays on the WAITING slate, not "reconnecting"
          for feed in [Feed::Continuous, Feed::Fresh, Feed::Gap, Feed::LongGap] {
              assert_ne!(next_phase(Waiting, feed), Reconnecting);
          }
      }
      #[test]
      fn unlisted_pairs_hold_their_phase() {
          let mut checked = 0;
          for ph in [Waiting, Live, Frozen, Reconnecting] {
              for feed in [Feed::Continuous, Feed::Fresh, Feed::Gap, Feed::LongGap] {
                  if !PROGRAMME_TABLE.iter().any(|&(f, s, _)| f == ph && s == feed) {
                      assert_eq!(next_phase(ph, feed), ph);
                      checked += 1;
                  }
              }
          }
          eprintln!("unlisted pairs checked: {checked}");
          assert_eq!(checked, 16 - PROGRAMME_TABLE.len());
      }
      #[test]
      fn choice_per_phase() {
          assert_eq!(choice_for(Waiting), Slate(SlateKind::Waiting));
          assert_eq!(choice_for(Live), Emit);
          assert_eq!(choice_for(Frozen), Repeat);
          assert_eq!(choice_for(Reconnecting), Slate(SlateKind::Reconnecting));
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
          assert!(out.iter().all(|&(_, c)| c == Slate(SlateKind::Waiting)), "{} ticks", out.len());
      }
      #[test]
      fn freezes_after_half_second_then_slates_within_3_1_s_budget() {
          let out = drive(&[(0, 5_000)], 10_000);
          let last_frame = out.iter().filter(|&&(at, _)| at < 5_000).last().map(|&(at, _)| at).unwrap();
          let frozen = first(&out, 5_000, Repeat);
          let slate = first(&out, 5_000, Slate(SlateKind::Reconnecting));
          assert!(frozen - last_frame > t().frozen_after_ms, "froze too early at +{}", frozen - last_frame);
          assert!(slate - last_frame <= 3_100, "PDF budget: slate ≤ 3.1 s, got +{}", slate - last_frame);
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
          assert!(back - 8_000 >= t().resume_after_ms);
          assert!(back - 8_000 <= 1_200, "PDF budget: live ≤ 1.2 s after camera returns, got {}", back - 8_000);
      }
      #[test]
      fn hiccup_shorter_than_frozen_threshold_does_not_restart_the_run() {
          // 400 ms gap inside the first second: the run since t=0 is unbroken by the 0.5 s rule
          let out = drive(&[(0, 300), (700, 5_000)], 2_000);
          let live = first(&out, 0, Emit);
          assert!(live < 1_100, "went live at {live}; the 400 ms hiccup must not reset the run");
      }
      #[test]
      fn second_outage_behaves_like_the_first() {
          let out = drive(&[(0, 2_000), (8_000, 12_000), (18_000, 22_000)], 22_000);
          let a = first(&out, 8_000, Emit) - 8_000;
          let b = first(&out, 18_000, Emit) - 18_000;
          // `drive` walks one global 33 ms grid, so the two outages start at different grid phases
          // (8_019 vs 18_018): equal behaviour shows up as a difference under one frame period.
          assert!((a as i64 - b as i64).abs() < period() as i64, "second outage recovered in {b} ms vs {a} ms");
      }
      #[test]
      fn stalled_tick_thread_skips_frozen_straight_to_slate() {
          let (th, mut m) = (t(), Programme::default());
          for now in (0..2_000).step_by(33) { m.on_frame(now, &th); m.on_tick(now, &th); }
          assert_eq!(m.phase(), Live);
          assert_eq!(m.on_tick(2_000 + 4_000, &th), Slate(SlateKind::Reconnecting));
      }
  }
  ```

- [ ] **Step 2: Run, expect a FAIL.** `cargo test -p domain programme`. Expected: `todo!()` panics.

- [ ] **Step 3: Implement.**
  ```rust
  pub fn next_phase(phase: ProgrammePhase, feed: Feed) -> ProgrammePhase {
      PROGRAMME_TABLE.iter().find(|&&(f, s, _)| f == phase && s == feed).map_or(phase, |&(_, _, to)| to)
  }
  pub fn choice_for(phase: ProgrammePhase) -> FrameChoice {
      match phase {
          Waiting => Slate(SlateKind::Waiting),
          Live => Emit,
          Frozen => Repeat,
          Reconnecting => Slate(SlateKind::Reconnecting),
      }
  }
  impl Programme {
      pub fn on_frame(&mut self, now: Ms, t: &Thresholds) {
          let broke = self.last_frame_at.is_none_or(|l| elapsed(now, l) > t.frozen_after_ms);
          if broke { self.run_since = Some(now); }
          self.last_frame_at = Some(now);
      }
      pub fn feed(&self, now: Ms, t: &Thresholds) -> Feed {
          let Some(last) = self.last_frame_at else { return Feed::LongGap };
          let age = elapsed(now, last);
          if age >= t.slate_after_ms { return Feed::LongGap; }
          if age > t.frozen_after_ms { return Feed::Gap; }
          let run = self.run_since.map_or(0, |s| elapsed(now, s));
          if run >= t.resume_after_ms { Feed::Continuous } else { Feed::Fresh }
      }
      pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> FrameChoice {
          self.phase = next_phase(self.phase, self.feed(now, t));
          choice_for(self.phase)
      }
  }
  ```

- [ ] **Step 4: Run, expect a PASS.** `cargo test -p domain programme`. Expected: 13 passed.

- [ ] **Step 5: Mutate each guard once, and record it in the commit body.** Each mutant must turn at
  least one test red.
  1. Delete the `(Live, Feed::LongGap, Reconnecting)` row. Expected red:
     `stalled_tick_thread_skips_frozen_straight_to_slate`.
  2. Change `(Frozen, Feed::Fresh, Live)` to `(Frozen, Feed::Fresh, Frozen)`. Expected red:
     `frozen_returns_to_live_as_soon_as_frames_resume`.
  3. In `on_frame`, use `>=` instead of `>`. Run the suite. If nothing goes red, add a test that pins
     a frame exactly `frozen_after_ms` after the previous one as **not** breaking the run, then
     re-run the mutant.
  4. In `feed`, swap the `Gap` and `LongGap` checks. Expected red: the slate budget test.

  Revert every mutant.

- [ ] **Step 6: Commit.**
  `git commit -m "feat(domain): table-driven programme sub-machine (C03)"`

## Acceptance

- The 13 or more programme tests pass.
- All 4 mutants went red, plus any test added in mutant 3.
- `cargo clippy -p domain --all-targets -- -D warnings` is clean.

## Do not touch

`config.rs` values (a failing budget test means the code is wrong, not the number), other modules.

## Output cap

Under 15 lines.
