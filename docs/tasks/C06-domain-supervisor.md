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
  }

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

- [ ] **Step 6: Run, expect a PASS.** `cargo test -p domain supervisor`. Expected: 15 passed.

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

  Revert every mutant, and record all four in the commit body.

- [ ] **Step 10: Gate (wave 1).**
  ```bash
  cargo fmt --all --check && cargo clippy -p domain --all-targets -- -D warnings && cargo test -p domain -- --nocapture 2>&1 | tail -30
  ```
  Paste the `test result:` lines and the property counts into the commit body.
  `git commit -m "feat(domain): supervisor aggregate, lifecycle, health and properties (C06)"`

## Acceptance

- All domain tests pass, including 15 supervisor scenarios and 2 properties at 512 cases with non-zero
  counts.
- 4 mutants went red.
- Purity and deps guards are still green.

## Do not touch

Earlier modules' public APIs. A change needed there is a deviation: report it, don't make it silently.

## Output cap

Under 15 lines: counts, mutants, deviations.
