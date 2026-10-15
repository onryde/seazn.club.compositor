# C06: Domain Lifecycle, HealthReport, Supervisor aggregate, properties

**Repo:** `seazn.club.compositor`. **Depends on:** C03, C04, C05.

## Context

`Supervisor::decide(&self, event, now) -> Decision { next, effects, frame }` is **the only place
compositor state changes**. The app layer applies `effects` through ports and pushes `frame` to the
programme pipeline. This mirrors `seazn.club`'s `session.ts` `decide()`.

**Lifecycle:**
- `Booting` fetches the session, retrying on backoff.
- `Running` is entered from `BootstrapOk`. The `Ready`/`Streaming` distinction in spec §4.2 is
  *derived* from output and programme state for the heartbeat; it is not stored.
- (Amended 2026-10-15) `Holding { since, reason }`: the final hold. The programme shows the Final scene
  for `final_hold_ms`, then drains with `reason`. It is entered from `Running` on an `Ending` reply, a
  `SIGINT`, or the deadline. See "Amendment 2026-10-15 (overlay templates spec)" at the end.
- `Draining` sends EOS to the output and exits once drained, or after `drain_timeout_ms`.
- `Exited(reason)` is terminal and ignores every later event.

**Exit codes** (from the plan's Global Constraints):

| Code | Reasons |
|---|---|
| `0` | Ended, Deadline, Signalled, SessionGone (410) |
| `78` | Refused (401/404/wrong mode) |
| `70` | Fatal |

A crash after `ending` exits 70, and the host restarts the container (ADR 0004; ADR 0007 item 5). The restarted Machine's bootstrap then
gets 410 and exits 0. That sequence is tested here.

## Files

- Create: `crates/domain/src/{lifecycle.rs,health.rs,supervisor.rs}`
- Create: `crates/domain/tests/properties.rs`
- Modify: `crates/domain/src/lib.rs`
- (Amendment 2026-10-15) Modify: `crates/domain/src/programme.rs` (C03, merged: `Final`,
  `SlateKind::Final`, `SlateKind::ALL`, `Programme::finish`), `crates/domain/src/config.rs`
  (`final_hold_ms`), and `crates/domain/tests/programme.rs` (C03's sweeps; exact edits in the
  amendment). This is the one sanctioned change to an earlier module's public API.

## Interfaces

- **Consumes:** everything from C02–C05, with exactly the names those briefs declare.
- **Produces** (C07 and C08 build on these exact names):
  ```rust
  // lifecycle.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum ExitReason { Ended, Deadline, Signalled, SessionGone, Refused, Fatal }
  impl ExitReason { pub fn code(self) -> i32; }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Lifecycle { Booting { attempt: u32, retry_at: Option<Ms> }, Running, Draining { since: Ms, reason: ExitReason }, Exited(ExitReason) }
  // ^ superseded 2026-10-15: `Holding { since: Ms, reason: ExitReason }` sits between Running and Draining (amendment)

  // health.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum BeatState { Starting, Playing, Stalled, Stopped }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum LinkStatus { Connecting, Connected, Backoff }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum GraphicsStatus { Starting, Ready, Restarting, Failed }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct HealthReport {
      pub state: BeatState,
      pub programme_phase: ProgrammePhase,
      pub input: LinkStatus,
      pub output: LinkStatus,
      pub graphics: GraphicsStatus,
      pub input_kicks: u32,
      pub input_kick_alert: bool,
      pub pipeline_rebuilds: u64,   // input builds after the first
      pub sink_swaps: u64,          // output builds after the first
      pub dropped_frames: u64,      // ladder overruns
      pub rung: Rung,
      pub score_stale: Option<bool>,
      pub heartbeat_failures: u32,
      // + (amendment 2026-10-15) pub phase_channel: Option<PhaseChannel>,
  }
  // + (amendment 2026-10-15) #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum PhaseChannel { Loopback, Web }

  // supervisor.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Desired { Live, Ending }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum SignalKind { Term, Int }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Refusal { Gone, Unauthorized, NotFound, WrongMode }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Event {
      Tick,
      BootstrapOk { heartbeat_interval_ms: Ms, deadline_at: Ms },
      BootstrapFailed,
      BootstrapRefused(Refusal),
      InputConnected { gen: u64 }, InputFrame, InputLost { gen: u64, reason: LossReason },
      OutputConnected { gen: u64 }, OutputAck { gen: u64 }, OutputError { gen: u64 }, OutputDrained,
      EncoderOverrun,
      CefReady { gen: u64 }, CefCrashed { gen: u64 }, ScoreStale(bool),
      HeartbeatReply(Desired), HeartbeatFailed,
      Signal(SignalKind),
      Fatal,
      // + (amendment 2026-10-15) PhaseChannel(PhaseChannel),  // the page's channel signal, relayed by C11
  }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Effect {
      FetchSession,
      StartProgramme,
      RebuildInput { gen: u64 },
      SwapSink { gen: u64 },
      RestartGraphics { gen: u64 },
      HideOverlay,
      StepDown(Rung),
      SendHeartbeat(HealthReport),
      DrainOutput,
      Exit(i32),
  }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Decision { pub next: Supervisor, pub effects: Vec<Effect>, pub frame: Option<FrameChoice> }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct Supervisor { /* private */ }
  impl Supervisor {
      pub fn new(t: Thresholds) -> Self;
      pub fn decide(&self, event: Event, now: Ms) -> Decision;
      pub fn health(&self, now: Ms) -> HealthReport;
      pub fn lifecycle(&self) -> Lifecycle;
      pub fn programme_phase(&self) -> ProgrammePhase;
      pub fn input_generation(&self) -> u64;
      pub fn output_generation(&self) -> u64;
      pub fn graphics_generation(&self) -> u64;
      pub fn thresholds(&self) -> &Thresholds;
  }
  ```

## Steps

- [ ] **Step 1: `lifecycle.rs`, test first.**
  ```rust
  //! Process lifecycle and the exit-code contract (plan Global Constraints; R1 runner contract).
  use crate::time::Ms;

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum ExitReason { Ended, Deadline, Signalled, SessionGone, Refused, Fatal }

  impl ExitReason {
      /// 0 = clean (Fly does not restart; auto_destroy removes the Machine). 78 = EX_CONFIG. 70 = EX_SOFTWARE.
      pub fn code(self) -> i32 {
          match self {
              ExitReason::Ended | ExitReason::Deadline | ExitReason::Signalled | ExitReason::SessionGone => 0,
              ExitReason::Refused => 78,
              ExitReason::Fatal => 70,
          }
      }
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Lifecycle {
      /// `retry_at: None` ⇒ a fetch is in flight.
      Booting { attempt: u32, retry_at: Option<Ms> },
      Running,
      Draining { since: Ms, reason: ExitReason },
      Exited(ExitReason),
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      #[test]
      fn exit_codes_match_the_contract() {
          let table = [
              (ExitReason::Ended, 0), (ExitReason::Deadline, 0), (ExitReason::Signalled, 0),
              (ExitReason::SessionGone, 0), (ExitReason::Refused, 78), (ExitReason::Fatal, 70),
          ];
          for (r, c) in table { assert_eq!(r.code(), c, "{r:?}"); }
      }
  }
  ```

- [ ] **Step 2: `health.rs`.** Use the types from the Interfaces block exactly. No logic lives here
  apart from `Default`-free plain data. The mapping into a report is done in `Supervisor::health`.

- [ ] **Step 3: `supervisor.rs`: write the failing scenario tests first.**

  Put this test module at the bottom of `supervisor.rs`. The struct and `decide` start out as
  `todo!()`.
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::programme::SlateKind;

      fn t() -> Thresholds { Thresholds::default() }
      const FAR: Ms = 10_000_000;

      fn booted(now: Ms) -> Supervisor {
          let s = Supervisor::new(t());
          let d = s.decide(Event::Tick, 0);
          assert_eq!(d.effects, vec![Effect::FetchSession]);
          let d = d.next.decide(Event::BootstrapOk { heartbeat_interval_ms: 5_000, deadline_at: FAR }, now);
          assert!(d.effects.contains(&Effect::StartProgramme));
          d.next
      }
      fn exit_code(d: &Decision) -> Option<i32> {
          d.effects.iter().find_map(|e| if let Effect::Exit(c) = e { Some(*c) } else { None })
      }

      #[test]
      fn boot_retries_the_session_fetch_on_backoff() {
          let s = Supervisor::new(t());
          let d = s.decide(Event::Tick, 0);
          assert_eq!(d.effects, vec![Effect::FetchSession]);
          let d2 = d.next.decide(Event::Tick, 10);
          assert!(d2.effects.is_empty(), "no second fetch while one is in flight");
          let d3 = d2.next.decide(Event::BootstrapFailed, 100);
          let wait = t().bootstrap_backoff_base_ms;
          assert!(d3.next.decide(Event::Tick, 100 + wait - 1).effects.is_empty());
          assert_eq!(d3.next.decide(Event::Tick, 100 + wait).effects, vec![Effect::FetchSession]);
      }
      #[test]
      fn refusals_exit_with_the_contract_codes() {
          for (r, code) in [(Refusal::Gone, 0), (Refusal::Unauthorized, 78), (Refusal::NotFound, 78), (Refusal::WrongMode, 78)] {
              let d = Supervisor::new(t()).decide(Event::Tick, 0);
              let d = d.next.decide(Event::BootstrapRefused(r), 5);
              assert_eq!(exit_code(&d), Some(code), "{r:?}");
              assert!(matches!(d.next.lifecycle(), Lifecycle::Exited(_)));
          }
      }
      #[test]
      fn deadline_in_past_at_bootstrap_exits_clean() {
          let d = Supervisor::new(t()).decide(Event::Tick, 0);
          let d = d.next.decide(Event::BootstrapOk { heartbeat_interval_ms: 5_000, deadline_at: 1_000 }, 2_000);
          assert_eq!(exit_code(&d), Some(0));
          assert!(!d.effects.contains(&Effect::StartProgramme), "never start pipelines past the deadline");
      }
      #[test]
      fn first_running_tick_builds_input_sink_and_graphics_and_beats() {
          let s = booted(1_000);
          let d = s.decide(Event::Tick, 1_000);
          assert!(d.effects.contains(&Effect::RebuildInput { gen: 1 }));
          assert!(d.effects.contains(&Effect::SwapSink { gen: 1 }));
          assert!(d.effects.contains(&Effect::RestartGraphics { gen: 1 }));
          assert!(d.effects.iter().any(|e| matches!(e, Effect::SendHeartbeat(h) if h.state == BeatState::Starting)));
          assert_eq!(d.frame, Some(FrameChoice::Slate(SlateKind::Waiting)));
      }
      #[test]
      fn heartbeat_every_interval_not_every_tick() {
          let mut s = booted(0);
          let mut beats = Vec::new();
          for now in (0..=20_100).step_by(33) { // 33-ms ticks land each beat 16 ms late: 0, 5016, 10032, 15048, 20064
              let d = s.decide(Event::Tick, now);
              if d.effects.iter().any(|e| matches!(e, Effect::SendHeartbeat(_))) { beats.push(now); }
              s = d.next;
          }
          assert_eq!(beats.len(), 5, "beats at {beats:?}");
          for w in beats.windows(2) { assert!(w[1] - w[0] >= 5_000 && w[1] - w[0] < 5_033 + 33); }
      }
      #[test]
      fn beat_state_follows_output_and_programme() {
          let th = t();
          let mut s = booted(0).decide(Event::Tick, 0).next;
          assert_eq!(s.health(0).state, BeatState::Starting);
          s = s.decide(Event::OutputConnected { gen: 1 }, 10).next;
          // F5 option A: the slate is on air, so the session is playing; the camera shows as Waiting
          assert_eq!(s.health(10).state, BeatState::Playing, "slate on air counts as playing");
          assert_eq!(s.health(10).programme_phase, ProgrammePhase::Waiting);
          let mut now = 10;
          while now < 10 + th.resume_after_ms + 100 {
              s = s.decide(Event::InputFrame, now).next;
              s = s.decide(Event::OutputAck { gen: 1 }, now).next;
              s = s.decide(Event::Tick, now).next;
              now += 33;
          }
          assert_eq!(s.health(now).state, BeatState::Playing);
          assert_eq!(s.health(now).programme_phase, ProgrammePhase::Live);
          // camera drops while the output stays up: still playing, phase says so
          let mut later = now;
          while later < now + th.slate_after_ms + 100 {
              s = s.decide(Event::OutputAck { gen: 1 }, later).next;
              s = s.decide(Event::Tick, later).next;
              later += 33;
          }
          assert_eq!(s.health(later).state, BeatState::Playing, "camera loss is not a stalled broadcast");
          assert_eq!(s.health(later).programme_phase, ProgrammePhase::Reconnecting);
          let now = later;
          s = s.decide(Event::OutputError { gen: 1 }, now).next;
          assert_eq!(s.health(now).state, BeatState::Stalled, "output dropped after having connected");
      }
      // SUPERSEDED 2026-10-15 by `ending_reply_holds_the_final_card_then_drains` (amendment): an Ending
      // reply now holds the Final scene for final_hold_ms first. Do not write this test.
      #[test]
      fn ending_reply_drains_then_exits_zero_on_drained() {
          let s = booted(0).decide(Event::Tick, 0).next;
          let d = s.decide(Event::HeartbeatReply(Desired::Ending), 100);
          assert!(d.effects.contains(&Effect::DrainOutput));
          assert!(d.effects.iter().any(|e| matches!(e, Effect::SendHeartbeat(h) if h.state == BeatState::Stopped)));
          assert_eq!(d.next.decide(Event::Tick, 133).frame, None, "no frames pushed after EOS");
          let d = d.next.decide(Event::OutputDrained, 500);
          assert_eq!(exit_code(&d), Some(0));
      }
      #[test]
      fn drain_that_never_completes_exits_at_the_timeout() {
          let th = t();
          let s = booted(0).decide(Event::Tick, 0).next;
          let s = s.decide(Event::Signal(SignalKind::Term), 100).next;
          assert_eq!(exit_code(&s.decide(Event::Tick, 100 + th.drain_timeout_ms - 1)), None);
          assert_eq!(exit_code(&s.decide(Event::Tick, 100 + th.drain_timeout_ms)), Some(0));
      }
      // SUPERSEDED 2026-10-15 by `deadline_holds_then_drains_with_reason_deadline` (amendment).
      // Do not write this test.
      #[test]
      fn deadline_while_running_drains() {
          let s = Supervisor::new(t()).decide(Event::Tick, 0).next;
          let s = s.decide(Event::BootstrapOk { heartbeat_interval_ms: 5_000, deadline_at: 60_000 }, 0).next;
          let d = s.decide(Event::Tick, 60_000);
          assert!(d.effects.contains(&Effect::DrainOutput));
          assert!(matches!(d.next.lifecycle(), Lifecycle::Draining { reason: ExitReason::Deadline, .. }));
      }
      #[test]
      fn ending_mid_reconnect_builds_nothing_more() {
          let s = booted(0).decide(Event::Tick, 0).next;
          let s = s.decide(Event::InputLost { gen: 1, reason: LossReason::Error }, 50).next; // input now backing off
          let s = s.decide(Event::HeartbeatReply(Desired::Ending), 60).next;
          for now in (60..20_000).step_by(33) {
              let d = s.decide(Event::Tick, now);
              assert!(!d.effects.iter().any(|e| matches!(e, Effect::RebuildInput { .. } | Effect::SwapSink { .. })));
          }
      }
      #[test]
      fn connected_input_with_no_frames_is_rebuilt_after_the_stall_window() {
          // CF holds the playback socket open after the phone drops: slate first, then a fresh input
          let th = t();
          let s = booted(0).decide(Event::Tick, 0).next;               // builds input gen 1
          let mut s = s.decide(Event::InputConnected { gen: 1 }, 100).next;
          let mut rebuilt_at = None;
          let mut now = 100;
          while now < 100 + th.input_stall_after_ms + th.input_backoff_base_ms + 100 {
              let d = s.decide(Event::Tick, now);
              if d.effects.contains(&Effect::RebuildInput { gen: 2 }) { rebuilt_at = Some(now); }
              s = d.next;
              now += 33;
          }
          let at = rebuilt_at.expect("a connected-but-silent input was never rebuilt");
          assert!(at - 100 >= th.input_stall_after_ms, "rebuilt too early at +{}", at - 100);
      }
      #[test]
      fn signal_while_booting_exits_immediately() {
          let d = Supervisor::new(t()).decide(Event::Signal(SignalKind::Term), 0);
          assert_eq!(exit_code(&d), Some(0));
      }
      #[test]
      fn crash_after_ending_exits_70_and_the_restart_then_sees_410() {
          let s = booted(0).decide(Event::Tick, 0).next;
          let s = s.decide(Event::HeartbeatReply(Desired::Ending), 10).next;
          assert_eq!(exit_code(&s.decide(Event::Fatal, 20)), Some(70));
          // the Fly-restarted process: a fresh supervisor whose bootstrap is refused as gone
          let r = Supervisor::new(t()).decide(Event::Tick, 0).next.decide(Event::BootstrapRefused(Refusal::Gone), 5);
          assert_eq!(exit_code(&r), Some(0));
      }
      #[test]
      fn exited_ignores_every_event() {
          let s = Supervisor::new(t()).decide(Event::Signal(SignalKind::Term), 0).next;
          for e in [Event::Tick, Event::Fatal, Event::InputFrame, Event::HeartbeatReply(Desired::Ending), Event::Signal(SignalKind::Int)] {
              let d = s.decide(e.clone(), 10);
              assert!(d.effects.is_empty() && d.frame.is_none(), "{e:?} produced {:?}", d.effects);
          }
      }
      #[test]
      fn running_ticks_always_yield_a_frame() {
          let mut s = booted(0);
          let mut n = 0;
          for now in (0..10_000).step_by(33) {
              let d = s.decide(Event::Tick, now);
              assert!(d.frame.is_some());
              n += 1;
              s = d.next;
          }
          assert!(n > 0);
      }
  }
  ```

- [ ] **Step 4: Run, expect a FAIL.** `cargo test -p domain supervisor`

- [ ] **Step 5: Implement `Supervisor`.**
  ```rust
  //! The aggregate. `decide` is the only place compositor state changes.
  use crate::backoff::delay;
  use crate::config::Thresholds;
  use crate::graphics::{Graphics, GraphicsAction, GraphicsState};
  use crate::health::{BeatState, GraphicsStatus, HealthReport, LinkStatus};
  use crate::input_link::{InputLink, InputState, LossReason};
  use crate::ladder::{Ladder, Rung};
  use crate::lifecycle::{ExitReason, Lifecycle};
  use crate::output_link::{OutputLink, OutputState};
  use crate::programme::{FrameChoice, Programme, ProgrammePhase};
  use crate::time::{elapsed, Ms};

  // (Event, Effect, Decision, Desired, SignalKind, Refusal definitions exactly as in Interfaces)

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Supervisor {
      t: Thresholds,
      lifecycle: Lifecycle,
      programme: Programme,
      input: InputLink,
      output: OutputLink,
      graphics: Graphics,
      ladder: Ladder,
      score_stale: Option<bool>,
      heartbeat_interval_ms: Ms,
      next_beat_at: Ms,
      deadline_at: Ms,
      output_ever_connected: bool,
      heartbeat_failures: u32,
  }

  impl Supervisor {
      pub fn new(t: Thresholds) -> Self {
          let hb = t.heartbeat_interval_ms;
          Self {
              t, lifecycle: Lifecycle::Booting { attempt: 0, retry_at: Some(0) },
              programme: Programme::default(), input: InputLink::new(), output: OutputLink::new(),
              graphics: Graphics::new(), ladder: Ladder::new(), score_stale: None,
              heartbeat_interval_ms: hb, next_beat_at: 0, deadline_at: Ms::MAX,
              output_ever_connected: false, heartbeat_failures: 0,
          }
      }
      pub fn lifecycle(&self) -> Lifecycle { self.lifecycle }
      pub fn programme_phase(&self) -> ProgrammePhase { self.programme.phase() }
      pub fn input_generation(&self) -> u64 { self.input.generation() }
      pub fn output_generation(&self) -> u64 { self.output.generation() }
      pub fn graphics_generation(&self) -> u64 { self.graphics.generation() }
      pub fn thresholds(&self) -> &Thresholds { &self.t }

      pub fn decide(&self, event: Event, now: Ms) -> Decision {
          let mut next = self.clone();
          let mut fx = Vec::new();
          let mut frame = None;
          match self.lifecycle {
              Lifecycle::Exited(_) => {}
              Lifecycle::Booting { attempt, retry_at } => next.booting(event, now, attempt, retry_at, &mut fx),
              Lifecycle::Running => frame = next.running(event, now, &mut fx),
              Lifecycle::Draining { since, reason } => next.draining(event, now, since, reason, &mut fx),
          }
          Decision { next, effects: fx, frame }
      }

      fn exit(&mut self, reason: ExitReason, fx: &mut Vec<Effect>) {
          self.lifecycle = Lifecycle::Exited(reason);
          fx.push(Effect::Exit(reason.code()));
      }

      fn start_drain(&mut self, now: Ms, reason: ExitReason, fx: &mut Vec<Effect>) {
          self.lifecycle = Lifecycle::Draining { since: now, reason };
          fx.push(Effect::DrainOutput);
          fx.push(Effect::SendHeartbeat(self.health(now)));
      }

      fn booting(&mut self, event: Event, now: Ms, attempt: u32, retry_at: Option<Ms>, fx: &mut Vec<Effect>) {
          match event {
              Event::Tick => {
                  if retry_at.is_some_and(|r| now >= r) {
                      self.lifecycle = Lifecycle::Booting { attempt, retry_at: None };
                      fx.push(Effect::FetchSession);
                  }
              }
              Event::BootstrapFailed => {
                  let wait = delay(attempt, self.t.bootstrap_backoff_base_ms, self.t.bootstrap_backoff_cap_ms);
                  self.lifecycle = Lifecycle::Booting { attempt: attempt.saturating_add(1), retry_at: Some(now + wait) };
              }
              Event::BootstrapRefused(r) => {
                  let reason = if r == Refusal::Gone { ExitReason::SessionGone } else { ExitReason::Refused };
                  self.exit(reason, fx);
              }
              Event::BootstrapOk { heartbeat_interval_ms, deadline_at } => {
                  if deadline_at <= now { return self.exit(ExitReason::Deadline, fx); }
                  self.heartbeat_interval_ms = heartbeat_interval_ms;
                  self.deadline_at = deadline_at;
                  self.next_beat_at = now;
                  self.lifecycle = Lifecycle::Running;
                  fx.push(Effect::StartProgramme);
              }
              Event::Signal(_) => self.exit(ExitReason::Signalled, fx),
              Event::Fatal => self.exit(ExitReason::Fatal, fx),
              _ => {}
          }
      }

      fn running(&mut self, event: Event, now: Ms, fx: &mut Vec<Effect>) -> Option<FrameChoice> {
          let t = self.t.clone();
          match event {
              Event::Tick => {
                  if now >= self.deadline_at {
                      self.start_drain(now, ExitReason::Deadline, fx);
                      return None;
                  }
                  self.input.check_stall(now, self.programme.last_frame_at(), &t);
                  if self.input.on_tick(now, &t) { fx.push(Effect::RebuildInput { gen: self.input.generation() }); }
                  if self.output.on_tick(now, &t) { fx.push(Effect::SwapSink { gen: self.output.generation() }); }
                  match self.graphics.on_tick(now, &t) {
                      Some(GraphicsAction::Restart) => fx.push(Effect::RestartGraphics { gen: self.graphics.generation() }),
                      Some(GraphicsAction::Hide) => fx.push(Effect::HideOverlay),
                      None => {}
                  }
                  let choice = self.programme.on_tick(now, &t);
                  if now >= self.next_beat_at {
                      fx.push(Effect::SendHeartbeat(self.health(now)));
                      self.next_beat_at = now + self.heartbeat_interval_ms;
                  }
                  return Some(choice);
              }
              Event::InputFrame => self.programme.on_frame(now, &t),
              Event::InputConnected { gen } => self.input.on_connected(now, gen),
              Event::InputLost { gen, reason } => self.input.on_lost(now, gen, reason, &t),
              Event::OutputConnected { gen } => {
                  self.output.on_connected(now, gen);
                  if self.output.is_connected() { self.output_ever_connected = true; }
              }
              Event::OutputAck { gen } => self.output.on_ack(now, gen),
              Event::OutputError { gen } => self.output.on_error(now, gen, &t),
              Event::EncoderOverrun => {
                  if let Some(r) = self.ladder.on_overrun(now, &t) { fx.push(Effect::StepDown(r)); }
              }
              Event::CefReady { gen } => self.graphics.on_ready(now, gen),
              Event::CefCrashed { gen } => {
                  if self.graphics.on_crashed(now, gen, &t) == Some(GraphicsAction::Hide) { fx.push(Effect::HideOverlay); }
              }
              Event::ScoreStale(b) => self.score_stale = Some(b),
              Event::HeartbeatReply(Desired::Live) => self.heartbeat_failures = 0,
              // SUPERSEDED 2026-10-15 (amendment): Ending → start_hold(now, Ended); Signal(Int) → start_hold(now, Signalled);
              // Signal(Term) → start_drain as below; the deadline tick → start_hold(now, Deadline).
              Event::HeartbeatReply(Desired::Ending) => self.start_drain(now, ExitReason::Ended, fx),
              Event::HeartbeatFailed => self.heartbeat_failures = self.heartbeat_failures.saturating_add(1),
              Event::Signal(_) => self.start_drain(now, ExitReason::Signalled, fx),
              Event::Fatal => self.exit(ExitReason::Fatal, fx),
              Event::OutputDrained | Event::BootstrapOk { .. } | Event::BootstrapFailed | Event::BootstrapRefused(_) => {}
          }
          None
      }

      fn draining(&mut self, event: Event, now: Ms, since: Ms, reason: ExitReason, fx: &mut Vec<Effect>) {
          match event {
              Event::OutputDrained => self.exit(reason, fx),
              Event::Tick if elapsed(now, since) >= self.t.drain_timeout_ms => self.exit(reason, fx),
              Event::Fatal => self.exit(ExitReason::Fatal, fx),
              _ => {}
          }
      }

      pub fn health(&self, now: Ms) -> HealthReport {
          let state = match self.lifecycle {
              Lifecycle::Draining { .. } | Lifecycle::Exited(_) => BeatState::Stopped,
              Lifecycle::Booting { .. } => BeatState::Starting,
              // Owner ruling 2026-10-14 (plan review F5, option A): `playing` = the programme is on air,
              // slate or picture. The camera's state travels in programme_phase / input, never in `state`.
              Lifecycle::Running if self.output.is_connected() => BeatState::Playing,
              Lifecycle::Running if self.output_ever_connected => BeatState::Stalled,   // was on air, output now down
              Lifecycle::Running => BeatState::Starting,
          };
          HealthReport {
              state,
              programme_phase: self.programme.phase(),
              input: match self.input.state() {
                  InputState::Running { .. } => LinkStatus::Connected,
                  InputState::Connecting { .. } => LinkStatus::Connecting,
                  InputState::Backoff { .. } => LinkStatus::Backoff,
              },
              output: match self.output.state() {
                  OutputState::Connected { .. } => LinkStatus::Connected,
                  OutputState::Connecting { .. } => LinkStatus::Connecting,
                  OutputState::Backoff { .. } => LinkStatus::Backoff,
              },
              graphics: match self.graphics.state() {
                  GraphicsState::Ready { .. } => GraphicsStatus::Ready,
                  GraphicsState::Starting { .. } => GraphicsStatus::Starting,
                  GraphicsState::Backoff { .. } => GraphicsStatus::Restarting,
                  GraphicsState::Failed { .. } => GraphicsStatus::Failed,
              },
              input_kicks: self.input.kicks(now, &self.t) as u32,
              input_kick_alert: self.input.kick_alert(now, &self.t),
              pipeline_rebuilds: self.input.generation().saturating_sub(1),
              sink_swaps: self.output.generation().saturating_sub(1),
              dropped_frames: self.ladder.overruns(),
              rung: self.ladder.rung(),
              score_stale: self.score_stale,
              heartbeat_failures: self.heartbeat_failures,
          }
      }
  }
  ```
  `use crate::ladder::Rung` is referenced through `Effect::StepDown(Rung)`. Keep it.

- [ ] **Step 6: Run, expect a PASS.** `cargo test -p domain supervisor`. Expected: ~~15 passed~~
  22 passed (amended 2026-10-15: 15 − 2 superseded + 9 from the amendment).

- [ ] **Step 7: Write the property tests, with an independent oracle.**
  `crates/domain/tests/properties.rs`:
  ```rust
  //! Properties over random event streams. The ORACLE recomputes programme rules from raw frame times
  //! the test itself recorded — it never asks the code under test what the answer should be.
  use domain::config::Thresholds;
  use domain::input_link::LossReason;
  use domain::lifecycle::Lifecycle;
  use domain::programme::ProgrammePhase;
  use domain::supervisor::{Desired, Effect, Event, Supervisor};
  use domain::time::Ms;
  use proptest::prelude::*;
  use proptest::test_runner::{Config, TestRunner};
  use std::cell::Cell;

  const CASES: u32 = 512;

  fn steps() -> impl Strategy<Value = Vec<(Ms, u8)>> {
      // dt 0..300 ms; kind 0..=11 with ticks (0..=4) dominant
      prop::collection::vec((0u64..300, 0u8..=11), 1..600)
  }

  fn event_for(kind: u8, s: &Supervisor) -> Event {
      match kind {
          0..=4 => Event::Tick,
          5 | 6 => Event::InputFrame,
          7 => Event::InputConnected { gen: s.input_generation() },
          8 => Event::InputLost { gen: s.input_generation(), reason: LossReason::Eos },
          9 => Event::OutputConnected { gen: s.output_generation() },
          10 => Event::CefReady { gen: s.graphics_generation() },
          _ => Event::HeartbeatReply(Desired::Live),
      }
  }

  fn booted(t: &Thresholds) -> Supervisor {
      let s = Supervisor::new(t.clone()).decide(Event::Tick, 0).next;
      s.decide(Event::BootstrapOk { heartbeat_interval_ms: 5_000, deadline_at: Ms::MAX / 2 }, 0).next
  }

  #[test]
  fn programme_and_generation_properties_hold_on_random_streams() {
      let t = Thresholds::default();
      let checked = Cell::new(0u32);
      let ticks = Cell::new(0u64);
      let into_live = Cell::new(0u64);
      let mut runner = TestRunner::new(Config { cases: CASES, ..Config::default() });
      runner
          .run(&steps(), |steps| {
              checked.set(checked.get() + 1);
              let mut s = booted(&t);
              let mut now: Ms = 0;
              let mut frames: Vec<Ms> = Vec::new();
              let mut last_rebuild = 0u64;
              for (dt, kind) in steps {
                  now += dt;
                  let ev = event_for(kind, &s);
                  if ev == Event::InputFrame { frames.push(now); }
                  let before = s.programme_phase();
                  let d = s.decide(ev.clone(), now);
                  // P1: a Running tick yields exactly one frame; nothing else ever does
                  let running = matches!(s.lifecycle(), Lifecycle::Running);
                  prop_assert_eq!(d.frame.is_some(), running && ev == Event::Tick, "frame on {:?}", ev);
                  if ev == Event::Tick && running { ticks.set(ticks.get() + 1); }
                  // P2: entering Live from Waiting/Reconnecting needs ≥ resume_after of unbroken frames
                  let after = d.next.programme_phase();
                  if after == ProgrammePhase::Live && matches!(before, ProgrammePhase::Waiting | ProgrammePhase::Reconnecting) {
                      into_live.set(into_live.get() + 1);
                      let mut run_start = *frames.last().expect("live with no frames");
                      for w in frames.windows(2).rev() {
                          if w[1] - w[0] > t.frozen_after_ms { break; }
                          run_start = w[0];
                      }
                      prop_assert!(now - run_start >= t.resume_after_ms, "live after only {} ms of run", now - run_start);
                  }
                  // P3: leaving Live needs the newest frame to be older than frozen_after
                  if before == ProgrammePhase::Live && after != ProgrammePhase::Live {
                      let age = frames.last().map_or(Ms::MAX, |&f| now - f);
                      prop_assert!(age > t.frozen_after_ms, "left Live with a {age} ms old frame");
                  }
                  // P4: input generations rise by exactly one per rebuild
                  for e in &d.effects {
                      if let Effect::RebuildInput { gen } = e {
                          prop_assert_eq!(*gen, last_rebuild + 1);
                          last_rebuild = *gen;
                      }
                  }
                  s = d.next;
              }
              Ok(())
          })
          .expect("property failed");
      eprintln!("cases checked: {}, running ticks: {}, transitions into Live: {}", checked.get(), ticks.get(), into_live.get());
      assert_eq!(checked.get(), CASES, "anti-vacuity: every configured case ran");
      assert!(ticks.get() > 0, "anti-vacuity: no running tick was ever checked");
      assert!(into_live.get() > 0, "anti-vacuity: P2 never fired — the generator never reached Live");
  }

  #[test]
  fn exit_is_emitted_at_most_once_and_nothing_follows_it() {
      let t = Thresholds::default();
      let checked = Cell::new(0u32);
      let exits = Cell::new(0u32);
      let mut runner = TestRunner::new(Config { cases: CASES, ..Config::default() });
      runner
          .run(&prop::collection::vec((0u64..2_000, 0u8..=4), 1..200), |steps| {
              checked.set(checked.get() + 1);
              let mut s = booted(&t);
              let mut now = 0;
              let mut seen_exit = false;
              for (dt, k) in steps {
                  now += dt;
                  let ev = match k {
                      0 | 1 => Event::Tick,
                      2 => Event::HeartbeatReply(Desired::Ending),
                      3 => Event::OutputDrained,
                      _ => Event::Fatal,
                  };
                  let d = s.decide(ev, now);
                  let n = d.effects.iter().filter(|e| matches!(e, Effect::Exit(_))).count();
                  if seen_exit { prop_assert!(d.effects.is_empty() && d.frame.is_none()); }
                  prop_assert!(n <= 1);
                  if n == 1 { seen_exit = true; exits.set(exits.get() + 1); }
                  s = d.next;
              }
              Ok(())
          })
          .expect("property failed");
      eprintln!("cases checked: {}, exits observed: {}", checked.get(), exits.get());
      assert_eq!(checked.get(), CASES);
      assert!(exits.get() > 0, "anti-vacuity: no case ever exited");
  }
  ```
  If `into_live` is 0 because the generator rarely lands frames ≤ 0.5 s apart for 1 s, give kinds
  5 and 6 more weight (e.g. `0u8..=15`, with 5..=9 mapped to `InputFrame`). Do not weaken the
  assertion.

- [ ] **Step 8: Run, expect a PASS.** `cargo test -p domain --test properties -- --nocapture`. Read the
  printed counts. `cases checked` must be 512 in both tests, and `transitions into Live` must be > 0.

- [ ] **Step 9: Mutate once each. All four must go red.**
  1. In `running`, `Tick` arm: push `SendHeartbeat` on every tick. Expected red:
     `heartbeat_every_interval_not_every_tick`.
  2. In `booting`, remove the `deadline_at <= now` check. Expected red:
     `deadline_in_past_at_bootstrap_exits_clean`.
  3. In `C03 programme.rs`, change `resume_after_ms` to `frozen_after_ms` in `feed`. Expected red:
     property P2, which is the oracle-based check. Also expected red: C03's own budget test.
  4. In `health()`, map output connected → `Playing` only when the programme phase is `Live`
     (the pre-ruling-A shape). Expected red: `beat_state_follows_output_and_programme` (slate on air
     must be `Playing`).

  5–9. **(Amendment 2026-10-15)** mutants 5–9 from the amendment section.

  Revert every mutant, and record all ~~four~~ nine in the commit body.

- [ ] **Step 10: Gate (wave 1).**
  ```bash
  cargo fmt --all --check && cargo clippy -p domain --all-targets -- -D warnings && cargo test -p domain -- --nocapture 2>&1 | tail -30
  ```
  Paste the `test result:` lines and the property counts into the commit body.
  `git commit -m "feat(domain): supervisor aggregate, lifecycle, health and properties (C06)"`

## Acceptance

- All domain tests pass, including ~~15~~ 22 supervisor scenarios (amended 2026-10-15) and 2 properties
  at 512 cases with non-zero counts, including the amendment's `holds entered` and `final ticks`.
- ~~4~~ 9 mutants went red (amended 2026-10-15).
- Purity and deps guards are still green.
- (Amended 2026-10-15) C03's sweeps pass with `Final` added: `unlisted pairs checked` is
  `5 × 4 − PROGRAMME_TABLE.len()`, and `final ticks checked` is > 0 from all four camera phases.


## Final heartbeat with the exit reason (owner ruling 2026-10-15)

Machine logs disappear when the Machine is destroyed, so the exit cause must reach the DB.
**Every** path that ends in `Effect::Exit(code)` emits a `SendHeartbeat` first, with
`state: Stopped` and a new `HealthReport.exit: Option<ExitInfo>`, where
`pub struct ExitInfo { pub reason: ExitReason, pub detail: Option<String> }`. A draining path
(`Draining { reason }`) emits it when draining **starts**; an immediate exit (Refused, Fatal) emits it
in the **same** `Decision`, ordered before `Exit`. `health()` returns `exit: None` in every other state.
Exception: `SessionGone` (the session GET or heartbeat returned 404) still emits the beat; the adapter
drops it, because web has nothing to attach it to.

Tests (add to `supervisor_props.rs`):
- `every_exit_is_preceded_by_a_final_beat_with_its_reason`. Drive each `ExitReason` from its real
  trigger event (Ending reply → Ended, deadline tick → Deadline, `Signal` → Signalled, 404 → SessionGone,
  401 at bootstrap → Refused, `ProgrammeStartFailed` → Fatal). Assert the `SendHeartbeat` index is
  lower than the `Exit` index and that `exit.reason` matches. The test prints `exit paths checked: 6`,
  and fewer than 6 fails it.
- **Mutant:** drop the final beat on the Fatal path. Expected red: that test.
- (Amended 2026-10-15) The Ended, Deadline and Signalled(`Int`) paths now pass through the final hold
  before draining, so the test ticks through `final_hold_ms` before looking for the drain-start beat.
  Signalled via `Term` still drains at once. Drive both signals: `exit paths checked: 7`, and fewer than
  7 fails it.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.2 (D4) and §6, C06 row (quoted verbatim from `29fa9f59c`): "Adds the final-hold phase: on
`Desired::Ending`, hold for `FINAL_HOLD_MS`, with `programmePhase = final`, then drain. Adds
`ProgrammePhase::Final` to the merged C03 domain enum (`crates/domain/src/programme.rs:23`); `Final` is
terminal and wins over every camera state". Re-pinned 2026-10-15 on `main` `21284d6`: `ProgrammePhase` at `programme.rs:23` has the
variants `{Waiting, Live, Frozen, Reconnecting}`, `SlateKind` is at `:31`, `PROGRAMME_TABLE` at `:60`
(an unlisted pair holds its phase), and `choice_for` at `:77` (an exhaustive match).

### False premise: the organiser's end does not arrive as an Ending reply first

The spec says "on `Desired::Ending`". R1's runner moves to `stopping` and sends `SIGINT` **immediately**
on the organiser's stop and on the deadline. The file is `seazn.club`
`apps/web/src/server/relay/domain/runner.ts` (path corrected by the review): `:101`, `:117-118` and
`:251-252` on main `b01d07cd5`, which R2003 (#967) moves to `:143`, `:160-161` and `:324-325`. Fly
SIGKILLs `RUNNER_STOP_GRACE_SECONDS = 10` later (`server/relay/config.ts:141` and `:147` on main;
`:146` and `:161` after #967).
The `ending` heartbeat reply can arrive up to one beat (5 s) after the signal, or never. So:

- **The hold is entered on whichever end request comes first while `Running`:** an `Ending` reply
  (reason `Ended`), `Signal(SignalKind::Int)` (R1's stop signal; reason `Signalled`), or the deadline
  tick (reason `Deadline`). Holding only on the reply would make the Final scene an inert seam: the
  `SIGINT` would cut the programme before the reply landed.
- **`Signal(SignalKind::Term)` still drains at once.** Viewers must not see "Stream ended" for a host
  restart. **(Corrected 2026-10-15, review I4)** SIGTERM is the platform's stop **only because web
  makes it so**. Fly's default stop signal is SIGINT (docs.fly.io `kill_signal`, default 5 s
  timeout), so without a change a Fly-initiated stop would look like ours. R2004's PR 0 (coordinator
  decision I4) creates composed Machines with `stop_config: { signal: "SIGTERM", timeout: <grace> }`,
  and web's own stop keeps passing `signal: "SIGINT"` explicitly. PR 0 measures that the explicit
  signal wins (`_FINDINGS` F-0xx).

  | Signal | Who sends it | C06 behaviour |
  |---|---|---|
  | `SIGINT` | web's organiser stop and deadline stop (`stopMachine`, explicit `signal: "SIGINT"`) | `Running` → hold (`Signalled`), Final scene, then drain |
  | `SIGTERM` | Fly's controlled stops (`fly machine stop`, deploy, auto-stop) via `stop_config.signal` | drain at once (`Signalled`), no Final scene |
  | `SIGKILL` | Fly, `timeout` after either | none (the process dies) |

  Until PR 0 ships, a Fly-initiated stop arrives as SIGINT and shows the Final card. That's
  cosmetic, and C17 must not run before PR 0.
- **Budget:** hold 10 000 + `drain_timeout_ms` 7 000 = 17 s, and R1 grants 10 s. **OWNER DECISION
  PENDING**, owed through R2004's PR 0 Part 2. The recommendation there is to raise R1's grace to
  20 s; it is a recommendation, not a ruling. C12's
  `final_hold_and_drain_fit_inside_r1_stop_grace` stays red until web ships the owner's pick, so the
  budget can't be forgotten. This task does **not** shorten the hold on its own.

### `programme.rs` (C03's module)

```rust
pub enum ProgrammePhase {
    Waiting,
    Live,
    Frozen,
    Reconnecting,
    /// Overlay templates spec 2026-10-15 §4.2 (D4): the final hold before EOS. Terminal: PROGRAMME_TABLE has no
    /// row from it, so no feed leaves it, and only `Programme::finish` enters it. It wins over every camera state.
    Final,
}

pub enum SlateKind { Waiting, Reconnecting, Final }
impl SlateKind {
    /// Every kind once. C09 renders the one textless slate for each, and its test enumerates this.
    pub const ALL: [SlateKind; 3] = [SlateKind::Waiting, SlateKind::Reconnecting, SlateKind::Final];
}

// choice_for: one new arm. Spec §4.2's Final backdrop is opaque, so the camera is not shown; with
// graphics up the page's Final scene covers the slate, and with graphics failed C09's textless slate shows.
Final => Slate(SlateKind::Final),

impl Programme {
    /// Enter the final hold (C06). Idempotent. Later frames and ticks keep it Final.
    pub fn finish(&mut self) { self.phase = Final; }
}
```
`PROGRAMME_TABLE` is **unchanged**: no row into `Final`, and none out of it. The inline
`choice_per_phase` test gains `assert_eq!(choice_for(Final), Slate(SlateKind::Final));`.

### `crates/domain/tests/programme.rs` (C03's sweeps), exact edits

These go red the moment `Final` exists. `PHASES` names 4 variants, `unlisted_pairs_hold_their_phase`
hardcodes 16, the reachability test expects every phase from Waiting, and the reference sweep asserts
every phase is ticked. Two more spots break the **compile** (briefs review):
- the import at `:10`: `use ProgrammePhase::{Final, Frozen, Live, Reconnecting, Waiting};`
- the reference model's exhaustive `match choice` (`:328-338`) gains the arm
  `FrameChoice::Slate(SlateKind::Final) => panic!("Final slate from a camera feed at {now}")`.
  No feed reaches `Final` (see the sweep below), so this arm is a guard: reaching it is a failure.

The rest of the list:
- `const PHASES: [ProgrammePhase; 5] = [Waiting, Live, Frozen, Reconnecting, Final];` and a new
  `const FEED_PHASES: [ProgrammePhase; 4] = [Waiting, Live, Frozen, Reconnecting];` with a doc line:
  "the phases a camera feed can reach (C03); `Final` is entered only by `Programme::finish` (C06)".
- `phase_index`: `Final => 4`.
- `PDF` becomes `[[ProgrammePhase; 4]; 5]`, with the row `[Final, Final, Final, Final], // Final` and
  this source line: "overlay templates spec 2026-10-15 §6, C06 row: Final is terminal and wins over
  every camera state".
- `spec_choice`: `Final => FrameChoice::Slate(SlateKind::Final)` (spec §4.2: an opaque backdrop).
- `unlisted_pairs_hold_their_phase`: `assert_eq!(checked, PHASES.len() * FEEDS.len() - PROGRAMME_TABLE.len());`
- `every_phase_is_reachable_from_waiting`: `assert_eq!(seen.len(), FEED_PHASES.len(), …)` and
  `assert!(!seen.contains(&Final), "no feed reaches Final")`.
- The reference sweep: `phase_ticks: [usize; 5]`. Assert `n > 0` for every `FEED_PHASES` entry and
  `phase_ticks[phase_index(Final)] == 0`. That second assertion is a positive statement: a table row
  into `Final` would turn it red.
- New, `final_is_terminal_against_every_feed`: for each `FEED_PHASES` start, reach it from
  `Programme::default()` with frames and ticks (Live: 33 ms frames for `resume_after_ms` + 100;
  Frozen: then no frame for `frozen_after_ms` + 100; Reconnecting: then no frame up to
  `slate_after_ms` + 100), then call `finish()`. Then run a schedule that produces every `Feed`:
  33 ms frames for 1 200 ms (Fresh, then Continuous), silence for 600 ms (Gap), and silence for
  3 100 ms (LongGap), with every number from `Thresholds::default()`. On every tick, assert
  `phase() == Final` and `on_tick == Slate(SlateKind::Final)`. Print
  `final ticks checked: N, starts: 4, feeds seen: [..]`; fail if N is 0 or any feed count is 0.

### `config.rs`

```rust
/// Overlay templates spec 2026-10-15 §4.2 (D4): FINAL_HOLD_MS, how long the Final scene holds before
/// EOS. C12's `final_hold_and_drain_fit_inside_r1_stop_grace` pins final_hold + drain + 3 s inside R1's grace.
pub final_hold_ms: Ms,          // default 10_000
```
Also rewrite the `drain_timeout_ms` doc line (`config.rs:66-69`, re-pinned at `21284d6`), which names
C12's superseded `drain_timeout_fits_inside_r1_stop_grace`. It now says that C12's
`final_hold_and_drain_fit_inside_r1_stop_grace` pins `final_hold_ms + drain_timeout_ms + 3 s` inside
R1's grace. Keep the 3 s headroom sentence: R2004's and C12's budget tests both cite it.

### `lifecycle.rs`, `health.rs`, `supervisor.rs`

```rust
// lifecycle.rs
pub enum Lifecycle {
    Booting { attempt: u32, retry_at: Option<Ms> },
    Running,
    /// The final hold: the Final scene is on air for `final_hold_ms`, then the drain starts with `reason`.
    Holding { since: Ms, reason: ExitReason },
    Draining { since: Ms, reason: ExitReason },
    Exited(ExitReason),
}

// health.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhaseChannel { Loopback, Web }
// HealthReport gains: pub phase_channel: Option<PhaseChannel>,   // None until the page reports one: absent, never defaulted

// supervisor.rs
// Event gains: PhaseChannel(PhaseChannel),
// Supervisor gains the field `phase_channel: Option<PhaseChannel>` (None in `new`).
// decide(): Lifecycle::Holding { since, reason } => frame = next.holding(event, now, since, reason, &mut fx),

fn start_hold(&mut self, now: Ms, reason: ExitReason) {
    self.lifecycle = Lifecycle::Holding { since: now, reason };
    self.programme.finish();
}

// running(), changed arms:
Event::Tick => {
    if now >= self.deadline_at {
        self.start_hold(now, ExitReason::Deadline);
        return self.holding(Event::Tick, now, now, ExitReason::Deadline, fx);
    }
    /* … unchanged … */
}
Event::HeartbeatReply(Desired::Ending) => self.start_hold(now, ExitReason::Ended),
Event::Signal(SignalKind::Int) => self.start_hold(now, ExitReason::Signalled),
Event::Signal(SignalKind::Term) => self.start_drain(now, ExitReason::Signalled, fx),
Event::PhaseChannel(c) => self.phase_channel = Some(c),

fn holding(&mut self, event: Event, now: Ms, since: Ms, reason: ExitReason, fx: &mut Vec<Effect>) -> Option<FrameChoice> {
    let t = self.t.clone();
    match event {
        Event::Tick => {
            if elapsed(now, since) >= t.final_hold_ms {
                self.start_drain(now, reason, fx);
                return None;
            }
            // No link or graphics on_tick here: nothing is rebuilt during the hold (no rebuild could finish inside it).
            let choice = self.programme.on_tick(now, &t); // Final ⇒ Slate(SlateKind::Final)
            if now >= self.next_beat_at {
                fx.push(Effect::SendHeartbeat(self.health(now)));
                self.next_beat_at = now + self.heartbeat_interval_ms;
            }
            return Some(choice);
        }
        Event::InputFrame => self.programme.on_frame(now, &t),
        Event::InputConnected { gen } => self.input.on_connected(now, gen),
        Event::InputLost { gen, reason: r } => self.input.on_lost(now, gen, r, &t),
        Event::OutputConnected { gen } => {
            self.output.on_connected(now, gen);
            if self.output.is_connected() { self.output_ever_connected = true; }
        }
        Event::OutputAck { gen } => self.output.on_ack(now, gen),
        Event::OutputError { gen } => self.output.on_error(now, gen, &t),
        Event::CefReady { gen } => self.graphics.on_ready(now, gen),
        Event::ScoreStale(b) => self.score_stale = Some(b),
        Event::PhaseChannel(c) => self.phase_channel = Some(c),
        Event::HeartbeatReply(Desired::Live) => self.heartbeat_failures = 0,
        Event::HeartbeatFailed => self.heartbeat_failures = self.heartbeat_failures.saturating_add(1),
        Event::Signal(SignalKind::Term) => self.start_drain(now, ExitReason::Signalled, fx),
        Event::Fatal => self.exit(ExitReason::Fatal, fx),
        // A second end request (the reply after the SIGINT, or the reverse) keeps the FIRST hold's clock. An
        // overrun or a CEF crash changes nothing: the hold is ≤ final_hold_ms and the held overlay frame stays (C11).
        Event::HeartbeatReply(Desired::Ending) | Event::Signal(SignalKind::Int) | Event::EncoderOverrun
        | Event::CefCrashed { .. } | Event::OutputDrained | Event::BootstrapOk { .. } | Event::BootstrapFailed
        | Event::BootstrapRefused(_) => {}
    }
    None
}

// health(): the Final scene is on air, so Holding maps like Running
Lifecycle::Running | Lifecycle::Holding { .. } if self.output.is_connected() => BeatState::Playing,
Lifecycle::Running | Lifecycle::Holding { .. } if self.output_ever_connected => BeatState::Stalled,
Lifecycle::Running | Lifecycle::Holding { .. } => BeatState::Starting,
// and `phase_channel: self.phase_channel,`
```
`Booting` is unchanged: a signal or a past deadline there exits at once, because nothing is on air.

### Tests (inline in `supervisor.rs`; write them failing first)

Every expected number comes from `Thresholds::default()` or the spec, never from `decide`. Use the
existing `t()`, `booted` and `exit_code` helpers, and add `reach(phase)`, which drives `booted(0)` with
frames and ticks into each camera phase exactly as `final_is_terminal_against_every_feed` does.

1. `final_hold_is_the_spec_number`: `assert_eq!(t().final_hold_ms, 10_000, "overlay spec §4.2 FINAL_HOLD_MS")`.
2. `ending_reply_holds_the_final_card_then_drains`, replacing the superseded drain test:
   - boot, connect the output (gen 1), and send `InputLost { gen: 1, reason: LossReason::Error }` at
     20, so a rebuild **falls due inside the hold**
   - an `Ending` reply at 100: no `DrainOutput`, `programme_phase() == Final`
   - every 33 ms tick in `[100, 100 + final_hold_ms)`: `frame == Some(Slate(SlateKind::Final))`, and
     none of `DrainOutput`, `RebuildInput`, `SwapSink` or `RestartGraphics`
   - the tick at exactly `100 + final_hold_ms`: `DrainOutput`, a `Stopped` beat whose `exit.reason` is
     `Ended`, and `frame == None`
   - `OutputDrained` → exit 0
   - the assertion message carries the tick count, and it must be > 0
3. `final_wins_over_every_camera_state`: for each of Waiting, Live, Frozen and Reconnecting (via
   `reach`), send `Ending`, then 1 200 ms of 33 ms frames, 600 ms of silence and 3 100 ms of silence
   (all inside the hold). Every tick: `programme_phase() == Final`. Assert 4 starts were checked.
4. `a_second_end_request_during_the_hold_changes_nothing`: an `Ending` reply at 100, then a second
   `Ending` at 2 000 and `Signal(Int)` at 3 000. Each gives no effects, no frame, and an unchanged
   `lifecycle()`. The tick at `100 + final_hold_ms − 1` doesn't drain; the tick at `100 + final_hold_ms` does.
5. `int_signal_holds_and_term_signal_drains_at_once`: `Signal(Int)` while Running →
   `Holding { reason: Signalled }` with no `DrainOutput`; `Signal(Term)` while Running →
   `DrainOutput`, `Draining { reason: Signalled }`; `Signal(Term)` during a hold → `DrainOutput`.
6. `deadline_holds_then_drains_with_reason_deadline`, replacing the superseded deadline test: the
   tick at `deadline_at` yields `Some(Slate(SlateKind::Final))`, no `DrainOutput`, and
   `Holding { reason: Deadline }`; the tick at `deadline_at + final_hold_ms` → `Draining { reason: Deadline }`.
7. `fatal_during_the_hold_exits_70`.
8. `health_during_the_hold_reports_final_and_stays_on_air`: with the output connected,
   `health().state == Playing` and `programme_phase == Final` throughout the hold. Collect the
   `SendHeartbeat`s over the hold: at least one, and every one reports `Final`.
9. `phase_channel_reaches_the_report_and_is_absent_until_told`: `None` after boot (absent, never
   defaulted); `Some(PhaseChannel::Web)` after `Event::PhaseChannel(PhaseChannel::Web)`.

**Properties** (`properties.rs`):
- P1 becomes P1′: a tick yields a frame iff the lifecycle before **and** after is `Running` or
  `Holding` (`on_air`). The deadline tick (Running → Holding) yields the Final frame; the hold-end tick
  (Holding → Draining) yields none.
- In `exit_is_emitted_at_most_once_and_nothing_follows_it`, add P5: once `programme_phase()` is
  `Final`, it stays `Final` until exit, and no `RebuildInput`, `SwapSink` or `RestartGraphics` follows
  it. Print `holds entered: N, final ticks: M`, and assert both are > 0. If `final ticks` is 0, lower
  the `Fatal` weight in that generator; never weaken the assertion.

**Mutants** (restore each one; record all in the commit body):

| # | Mutant | Expected red |
|---|---|---|
| 5 | add `(Final, Feed::Continuous, Live)` to `PROGRAMME_TABLE` | C03's table-vs-PDF test, `final_is_terminal_against_every_feed`, test 3, P5 |
| 6 | an `Ending` reply calls `start_drain` (skips the hold) | test 2 |
| 7 | `Signal(Int)` drains at once | test 5 |
| 8 | `holding` also runs the links' and graphics' `on_tick` | test 2 (the due rebuild appears) |
| 9 | `health()` maps `Holding` to `Stopped` | test 8 |

### Owed elsewhere (not this task)

- **C07**: `heartbeat_wire` emits `programmePhase: "final"` and `phaseChannel` (`"loopback"`/`"web"`,
  only when `Some`). Its contract test's script must reach `Final`. It vendors the contract only after
  R2007 PR B merges (web's strict schema returns 400 for `final` until then). This is recorded in the
  plan and `_INDEX.md`.
- **C08**: the driver publishes phase changes to the loopback endpoint (amended).

## Do not touch

Earlier modules' public APIs. A change needed there is a deviation: report it, don't make it silently.
(Amended 2026-10-15) **Exception:** the `programme.rs` additions, the `config.rs` field and the C03
test edits listed in the amendment. Nothing else in C03's module changes.

## Output cap

Under 15 lines: counts, mutants, deviations.
