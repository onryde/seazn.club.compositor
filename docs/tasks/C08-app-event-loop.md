# C08: Ports, testkit fakes, the event loop, and scenario tests

**Repo:** `seazn.club.compositor`. **Depends on:** C07.

## Context

The app layer turns domain `Effect`s into port calls and turns adapter callbacks into domain
`Event`s. Three rules:

1. **The frame clock never waits on I/O.** HTTP (fetch and heartbeat) runs on a control worker thread.
   Results come back as inbound messages.
2. **Ticks are paced by index, not by wall time.** Tick `n` is due at `start + n·period`. If the loop
   falls behind, it catches up by issuing every missed index, at most `fps` of them (1 s). Beyond that
   it skips ahead and counts the skip. The programme adapter timestamps frames by index, so the output
   stays constant-rate.
3. **`Driver` is step-driven and deterministic.** `Driver::step(inbound, now)` and
   `Driver::tick(now)` do the work, and `run()` is a thin blocking wrapper around them. Tests drive
   `Driver` with fakes and a manual clock.

## Files

- Modify: `crates/app/src/ports.rs`. Add the remaining ports. C07 created the file with
  `ControlPlane`/DTOs.
- Create: `crates/app/src/{driver.rs,control_worker.rs,run.rs}`
- Create: `crates/testkit/src/{lib.rs,fakes.rs}`
- Test: `crates/app/tests/scenarios.rs`, `crates/app/tests/control_worker.rs`
- `crates/app/Cargo.toml`: add `crossbeam-channel = "0.5"`

## Interfaces

- **Consumes:** C06 domain API, and C07 `ports::{ControlPlane, SessionConfig, InputSpec, TargetSpec, Measured, FetchError}`.
- **Produces:**
  ```rust
  // ports.rs (additions)
  use domain::{config::OutputProfile, ladder::Rung, programme::FrameChoice, supervisor::Event};
  pub trait InputSource: Send { fn rebuild(&mut self, gen: u64, spec: &InputSpec); fn stop(&mut self); }
  pub trait ProgrammeOutput: Send {
      fn start(&mut self, profile: &OutputProfile) -> Result<(), String>;
      fn push(&mut self, index: u64, choice: FrameChoice);
      fn swap_sink(&mut self, gen: u64, target: &TargetSpec);
      fn step_down(&mut self, rung: Rung);
      fn hide_overlay(&mut self);
      fn drain(&mut self);
  }
  pub trait GraphicsRenderer: Send { fn restart(&mut self, gen: u64, url: &str); fn stop(&mut self); }
  pub trait Metrics: Send { fn sample(&mut self) -> Measured; }
  #[derive(Debug, Clone, PartialEq)] pub enum ControlRequest { FetchSession, Heartbeat(domain::health::HealthReport, Measured) }
  pub trait ControlQueue: Send { fn request(&mut self, req: ControlRequest); }
  #[derive(Debug, Clone, PartialEq)] pub enum Inbound { Event(Event), Session(SessionConfig) }
  pub struct Ports { pub input: Box<dyn InputSource>, pub programme: Box<dyn ProgrammeOutput>, pub graphics: Box<dyn GraphicsRenderer>, pub metrics: Box<dyn Metrics>, pub control: Box<dyn ControlQueue> }
  // trait objects can't derive Debug, and C01's `missing_debug_implementations` + `clippy -D warnings` makes it fatal
  impl std::fmt::Debug for Ports {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Ports { input, programme, graphics, metrics, control }") }
  }

  // driver.rs
  #[derive(Debug)] pub struct Driver { /* supervisor, ports, session: Option<SessionConfig>, deadline_ms, tick bookkeeping, skipped_ticks */ }
  impl Driver {
      pub fn new(t: domain::config::Thresholds, ports: Ports, deadline_ms: u64, boot_ms: u64) -> Self;
      pub fn step(&mut self, msg: Inbound, now: u64) -> Option<i32>;   // Some(code) ⇒ exit
      pub fn tick_due(&mut self, now: u64) -> Option<i32>;              // runs every due tick index up to `now`
      pub fn next_tick_at(&self) -> u64;
      pub fn skipped_ticks(&self) -> u64;
  }
  // control_worker.rs
  pub fn spawn(cp: Box<dyn ControlPlane>, tx: crossbeam_channel::Sender<Inbound>) -> ControlHandle; // ControlHandle: ControlQueue
  // run.rs
  pub fn run(driver: Driver, rx: crossbeam_channel::Receiver<Inbound>, clock: &dyn Fn() -> u64) -> i32;
  ```

## Behaviour of `Driver`

- **Before bootstrap,** the tick period is 100 ms, because there's no profile yet. After
  `Inbound::Session(cfg)`:
  1. store `cfg`
  2. call `step(Event::BootstrapOk{ heartbeat_interval_ms: cfg.heartbeat_interval_ms, deadline_at: deadline_ms })`
  3. reset the tick period to `cfg.output.frame_period_ms()`, rounded to µs internally (use
     `f64 → u64` ns), with tick index 0 at `now`

  `Inbound::Event(e)` is handed straight to `supervisor.decide(e, now)`.
- **Applying effects, in order:**

  | Effect | Port call |
  |---|---|
  | `FetchSession` | `control.request(FetchSession)` |
  | `StartProgramme` | `programme.start(&cfg.output)`. On `Err`, decide `Event::Fatal`. |
  | `RebuildInput{gen}` | `input.rebuild(gen, &cfg.input)` |
  | `SwapSink{gen}` | `programme.swap_sink(gen, &cfg.target)` |
  | `RestartGraphics{gen}` | `graphics.restart(gen, &cfg.overlay_url)` |
  | `HideOverlay` | `programme.hide_overlay()` |
  | `StepDown(r)` | `programme.step_down(r)` |
  | `SendHeartbeat(h)` | `control.request(Heartbeat(h, metrics.sample()))` |
  | `DrainOutput` | `programme.drain()`, then `input.stop()` and `graphics.stop()` |
  | `Exit(c)` | `input.stop()`, `graphics.stop()`, and return `Some(c)`. Nothing is applied after it. |

- **`tick_due(now)`** runs `Event::Tick` for every index `i` with `start + i·period ≤ now`, capped at
  `fps` ticks per call. When the cap is hit, it jumps the index forward and adds the jump to
  `skipped_ticks`. Each tick's `frame` is passed as `programme.push(i, choice)`.

## Steps

- [ ] **Step 1: Write the fakes** (`crates/testkit/src/fakes.rs`). Each fake records calls in an
  `Arc<Mutex<Vec<…>>>` exposed through a cloneable handle, so tests can read the calls after moving
  the boxed fake into `Ports`.
  ```rust
  //! Fakes for every port. Built before any live adapter (house rule). Each records every call.
  use app::ports::*;
  use domain::{config::OutputProfile, ladder::Rung, programme::FrameChoice};
  use std::sync::{Arc, Mutex};

  #[derive(Debug, Clone, PartialEq)]
  pub enum Call {
      InputRebuild(u64, InputSpec), InputStop,
      Start(OutputProfile), Push(u64, FrameChoice), Swap(u64, TargetSpec), StepDown(Rung), HideOverlay, Drain,
      GraphicsRestart(u64, String), GraphicsStop,
      Control(ControlRequest),
  }
  #[derive(Debug, Clone, Default)]
  pub struct Log(pub Arc<Mutex<Vec<Call>>>);
  impl Log {
      pub fn push(&self, c: Call) { self.0.lock().expect("log").push(c) }
      pub fn calls(&self) -> Vec<Call> { self.0.lock().expect("log").clone() }
      pub fn count(&self, f: impl Fn(&Call) -> bool) -> usize { self.calls().iter().filter(|c| f(c)).count() }
  }
  #[derive(Debug)] pub struct FakeInput(pub Log);
  impl InputSource for FakeInput {
      fn rebuild(&mut self, gen: u64, spec: &InputSpec) { self.0.push(Call::InputRebuild(gen, spec.clone())) }
      fn stop(&mut self) { self.0.push(Call::InputStop) }
  }
  #[derive(Debug)] pub struct FakeProgramme { pub log: Log, pub fail_start: bool }
  impl ProgrammeOutput for FakeProgramme {
      fn start(&mut self, p: &OutputProfile) -> Result<(), String> { self.log.push(Call::Start(*p)); if self.fail_start { Err("boom".into()) } else { Ok(()) } }
      fn push(&mut self, i: u64, c: FrameChoice) { self.log.push(Call::Push(i, c)) }
      fn swap_sink(&mut self, gen: u64, t: &TargetSpec) { self.log.push(Call::Swap(gen, t.clone())) }
      fn step_down(&mut self, r: Rung) { self.log.push(Call::StepDown(r)) }
      fn hide_overlay(&mut self) { self.log.push(Call::HideOverlay) }
      fn drain(&mut self) { self.log.push(Call::Drain) }
  }
  #[derive(Debug)] pub struct FakeGraphics(pub Log);
  impl GraphicsRenderer for FakeGraphics {
      fn restart(&mut self, gen: u64, url: &str) { self.0.push(Call::GraphicsRestart(gen, url.into())) }
      fn stop(&mut self) { self.0.push(Call::GraphicsStop) }
  }
  #[derive(Debug, Default)] pub struct FakeMetrics(pub Measured);
  impl Metrics for FakeMetrics { fn sample(&mut self) -> Measured { self.0.clone() } }
  #[derive(Debug)] pub struct FakeControl(pub Log);
  impl ControlQueue for FakeControl { fn request(&mut self, r: ControlRequest) { self.0.push(Call::Control(r)) } }

  /// One shared log for every port, plus a composed-720p SessionConfig matching contracts/…/session-composed-720p.json.
  pub fn ports(log: &Log, metrics: Measured) -> Ports {
      Ports {
          input: Box::new(FakeInput(log.clone())),
          programme: Box::new(FakeProgramme { log: log.clone(), fail_start: false }),
          graphics: Box::new(FakeGraphics(log.clone())),
          metrics: Box::new(FakeMetrics(metrics)),
          control: Box::new(FakeControl(log.clone())),
      }
  }
  /// Every value is copied by hand from contracts/fixtures/relay-job.v1/session-composed-720p.json.
  /// C12's `fake_session_matches_the_vendored_fixture` asserts equality with parse_session(fixture),
  /// so a re-vendor that changes the fixture reds there instead of drifting silently.
  pub fn session_720p() -> SessionConfig {
      SessionConfig {
          session_id: "<fixture sessionId>".into(), fixture_id: "<fixture fixtureId>".into(),
          input: InputSpec { url: "<fixture input.url>".into(), stream_id: "<fixture input.streamId>".into(), passphrase: "<fixture input.passphrase>".into() },
          target: TargetSpec { url: "<fixture target.url>".into(), stream_key: "<fixture target.streamKey>".into() },
          output: OutputProfile::P720,
          heartbeat_interval_ms: 5_000, overlay_url: "<fixture overlayUrl>".into(),
          overlay_delay_ms: 0 /* fixture overlayDelayMs */,
      }
  }
  ```
  Each `<fixture …>` is the literal string at that JSON path in the vendored file. Open it and copy
  the value; never compute it.

- [ ] **Step 2: Write the failing scenario tests.** `crates/app/tests/scenarios.rs`:
  ```rust
  use app::driver::Driver;
  use app::ports::{ControlRequest, Inbound, Measured};
  use domain::config::{OutputProfile, Thresholds};
  use domain::programme::{FrameChoice, SlateKind};
  use domain::supervisor::{Desired, Event};
  use testkit::fakes::{ports, session_720p, Call, Log};

  const FAR: u64 = 100_000_000;

  fn boot(log: &Log) -> Driver {
      let mut d = Driver::new(Thresholds::default(), ports(log, Measured { cpu_pct: Some(40.0), ..Default::default() }), FAR, 0);
      assert_eq!(d.tick_due(0), None);
      assert_eq!(log.count(|c| matches!(c, Call::Control(ControlRequest::FetchSession))), 1);
      assert_eq!(d.step(Inbound::Session(session_720p()), 10), None);
      d
  }
  fn pushes(log: &Log) -> Vec<(u64, FrameChoice)> {
      log.calls().into_iter().filter_map(|c| if let Call::Push(i, f) = c { Some((i, f)) } else { None }).collect()
  }

  #[test]
  fn boot_starts_programme_then_first_tick_wires_input_sink_and_overlay_from_the_session() {
      let log = Log::default();
      let mut d = boot(&log);
      let cfg = session_720p();
      assert!(log.calls().contains(&Call::Start(OutputProfile::P720)));
      d.tick_due(10);
      let calls = log.calls();
      assert!(calls.contains(&Call::InputRebuild(1, cfg.input.clone())));
      assert!(calls.contains(&Call::Swap(1, cfg.target.clone())));
      assert!(calls.contains(&Call::GraphicsRestart(1, cfg.overlay_url.clone())));
      assert_eq!(pushes(&log), vec![(0, FrameChoice::Slate(SlateKind::Waiting))]);
  }
  #[test]
  fn heartbeat_carries_the_metrics_sample() {
      let log = Log::default();
      let mut d = boot(&log);
      d.tick_due(10);
      let beat = log.calls().into_iter().find_map(|c| if let Call::Control(ControlRequest::Heartbeat(_, m)) = c { Some(m) } else { None });
      assert_eq!(beat.expect("no heartbeat").cpu_pct, Some(40.0));
  }
  #[test]
  fn ticks_are_indexed_and_catch_up_after_a_stall() {
      let log = Log::default();
      let mut d = boot(&log);
      let p = OutputProfile::P720.frame_period_ms();
      d.tick_due(10);
      d.tick_due(10 + (5.0 * p) as u64 + 1); // loop stalled 5 frames
      let idx: Vec<u64> = pushes(&log).iter().map(|x| x.0).collect();
      assert_eq!(idx, vec![0, 1, 2, 3, 4, 5], "every missed index is pushed once, in order");
  }
  #[test]
  fn catch_up_is_capped_at_one_second_and_counted() {
      let log = Log::default();
      let mut d = boot(&log);
      d.tick_due(10);
      d.tick_due(10 + 10_000); // 10 s stall
      let n = pushes(&log).len() as u64;
      assert_eq!(n, 1 + u64::from(OutputProfile::P720.fps), "at most fps catch-up ticks");
      assert!(d.skipped_ticks() > 0);
  }
  #[test]
  fn camera_drop_repeats_then_slates_through_the_real_driver() {
      let log = Log::default();
      let mut d = boot(&log);
      let mut now = 10;
      while now < 3_000 { d.step(Inbound::Event(Event::InputFrame), now); d.tick_due(now); now += 33; }
      let stop = now;
      while now < stop + 4_000 { d.tick_due(now); now += 33; }
      let p = pushes(&log);
      assert!(p.iter().any(|x| x.1 == FrameChoice::Emit));
      assert!(p.iter().any(|x| x.1 == FrameChoice::Repeat));
      assert_eq!(p.last().unwrap().1, FrameChoice::Slate(SlateKind::Reconnecting));
  }
  #[test]
  fn ending_drains_stops_children_and_exits_zero_on_drained() {
      let log = Log::default();
      let mut d = boot(&log);
      d.tick_due(10);
      assert_eq!(d.step(Inbound::Event(Event::HeartbeatReply(Desired::Ending)), 20), None);
      assert!(log.calls().contains(&Call::Drain));
      assert_eq!(d.step(Inbound::Event(Event::OutputDrained), 30), Some(0));
      assert!(log.count(|c| *c == Call::InputStop) >= 1 && log.count(|c| *c == Call::GraphicsStop) >= 1);
      let before = log.calls().len();
      assert_eq!(d.tick_due(1_000), None);
      assert_eq!(log.calls().len(), before, "nothing is applied after exit");
  }
  #[test]
  fn programme_start_failure_is_fatal_70() {
      let log = Log::default();
      let mut p = ports(&log, Measured::default());
      p.programme = Box::new(testkit::fakes::FakeProgramme { log: log.clone(), fail_start: true });
      let mut d = Driver::new(Thresholds::default(), p, FAR, 0);
      d.tick_due(0);
      assert_eq!(d.step(Inbound::Session(session_720p()), 10), Some(70));
  }
  ```

- [ ] **Step 3: Write the failing control-worker test.** `crates/app/tests/control_worker.rs`:
  - `fetch_success_arrives_as_session_message`: a fake `ControlPlane` returns `Ok(session_720p())`,
    and the receiver gets `Inbound::Session`.
  - `refusal_arrives_as_bootstrap_refused` and `transient_arrives_as_bootstrap_failed`
  - `slow_heartbeat_does_not_queue_a_second_one`: a `ControlPlane` whose `heartbeat` sleeps 300 ms.
    Request 3 beats back to back, and assert the plane was called **at most 2** times: one in flight
    plus one pending slot (the channel capacity is 1). The others are dropped and counted.
  - `heartbeat_reply_and_failure_arrive_as_events`

- [ ] **Step 4: Run, expect a FAIL.** `cargo test -p app`

- [ ] **Step 5: Implement `driver.rs`, `control_worker.rs` and `run.rs`** as described in Behaviour.
  `control_worker::spawn`:
  - starts one thread with a `bounded(1)` request channel
  - `ControlHandle::request` uses `try_send`, and on `Full` increments a dropped counter (heartbeats
    are periodic, so dropping one is safe)
  - a `FetchSession` request is **never** dropped. Use a separate `bounded(1)` fetch channel, and
    have the worker `select!` over both.
  - results are mapped to `Inbound`:
    - `Ok(cfg)` → `Session`
    - `Err(Refused(r))` → `Event(BootstrapRefused(r))`
    - `Err(Transient(_))` → `Event(BootstrapFailed)`
    - heartbeat `Ok(d)` → `Event(HeartbeatReply(d))`
    - heartbeat `Err` → `Event(HeartbeatFailed)`

  `run()` loops on `rx.recv_timeout(next_tick_at − now)`. On a message it calls `step`, and on a
  timeout it calls `tick_due`. It returns the code on `Some(code)`.

- [ ] **Step 6: Run, expect a PASS.** `cargo test -p app -- --nocapture`. Also run C01's boundary
  guard, which must still be green: `app` never names `adapters`.

- [ ] **Step 7: Mutate once each.**
  1. Remove the catch-up cap. Expected red: `catch_up_is_capped…`.
  2. Apply effects after `Exit`. Expected red: `ending_drains…` ("nothing is applied after exit").
  3. Make `FetchSession` droppable by sending it through the heartbeat channel. Add a test
     `fetch_is_never_dropped_behind_a_slow_heartbeat` if no test goes red, then re-run.

- [ ] **Step 8: Commit.** `feat(app): driver, control worker, ports and fakes (C08)`

## Acceptance

- The scenario and control-worker tests pass.
- Boundary guard is green.
- 3 mutants went red.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.

## Do not touch

The domain crate's API, and the adapters (except to keep them compiling).

## Output cap

Under 15 lines.
