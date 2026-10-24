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
  // (Amended 2026-10-18, C05b PR #22 review I1) `stop()` returns without waiting: it takes `restart()`'s
  // reaper teardown and kill fallback (C11). Before it returns, it closes the stopped generation's slot
  // gate, so no sample of that generation reaches the overlay slot after it, even from a page that revives.
  // It never blocks the loop: F-001 measured `set_state(NULL)` not returning for 10 s on a hung renderer.
  pub trait Metrics: Send { fn sample(&mut self) -> Measured; }
  #[derive(Debug, Clone, PartialEq)] pub enum ControlRequest { FetchSession, Heartbeat(domain::health::HealthReport, Measured) }
  pub trait ControlQueue: Send { fn request(&mut self, req: ControlRequest); }
  #[derive(Debug, Clone, PartialEq)] pub enum Inbound { Event(Event), Session(SessionConfig) }
  pub struct Ports { pub input: Box<dyn InputSource>, pub programme: Box<dyn ProgrammeOutput>, pub graphics: Box<dyn GraphicsRenderer>, pub metrics: Box<dyn Metrics>, pub control: Box<dyn ControlQueue> }
  // ^ amended 2026-10-15: + `pub phase: Box<dyn PhasePublisher>` (see the amendment; the Debug string names it too)
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
      /// (Amended 2026-10-15, review I1) The supervisor's current programme phase, read-only; it
      /// delegates to `supervisor.programme_phase()`. Tests compare it with the last published phase.
      pub fn programme_phase(&self) -> domain::programme::ProgrammePhase;
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
  | `StartProgramme` | `programme.start(&cfg.output)`. On `Err(e)`, decide `Event::Fatal { detail: Some(e.to_string()) }` (amended 2026-10-18, Owed 27 (a): the error's text, unredacted; C07 redacts and caps it). |
  | `RebuildInput{gen}` | `input.rebuild(gen, &cfg.input)` |
  | `SwapSink{gen}` | `programme.swap_sink(gen, &cfg.target)` |
  | `RestartGraphics{gen}` | `graphics.restart(gen, &cfg.overlay_url)` |
  | ~~`HideOverlay`~~ | ~~`programme.hide_overlay()`, then **`graphics.stop()`** (amended 2026-10-18; coordinator decision 2026-10-16, Owed 17)~~ |
  | `HideOverlay { reason: Hang \| Failed, gen }` | log `overlay hidden reason=<r> gen=<g>` (C12), `programme.hide_overlay()`, then **`graphics.stop()`** (amended 2026-10-18; coordinator decision 2026-10-16, Owed 17; the reason from the C05b PR #22 review) |
  | `HideOverlay { reason: Crash \| Recycle, gen }` | log `overlay hidden reason=<crash\|recycle> gen=<g>`, then `programme.hide_overlay()` **alone** (amended 2026-10-18, C05b PR #22 review; `Recycle` added in its second round, the one bound on a frame's age). A crashed generation's gate closed at its reported crash, and a recycled one's at its restart (C11). The reload now loading must survive: stopping it would turn the bound's hide into one more load timeout |
  | `StepDown(r)` | `programme.step_down(r)` |
  | `SendHeartbeat(h)` | `control.request(Heartbeat(h, metrics.sample()))`; when `h.exit` is `Some`, the never-dropped final-beat slot instead (amended 2026-10-18, wave gate 1 I4; see that amendment) |
  | `DrainOutput` | `programme.drain()`, then `input.stop()` and `graphics.stop()` |
  | `Exit(c)` | `input.stop()`, `graphics.stop()`, and return `Some(c)`. Nothing is applied after it. |

- **`tick_due(now)`** runs `Event::Tick` for every index `i` with `start + i·period ≤ now`, capped at
  `fps` ticks per call. When the cap is hit, it jumps the index forward and adds the jump to
  `skipped_ticks`. Each tick's `frame` is passed as `programme.push(i, choice)`.
- **(Amended 2026-10-15) Phase publication.** After **every** `decide` (in `step` and in `tick_due`), if
  `supervisor.programme_phase()` differs from the last published phase, the driver increments its
  `phase_seq` and calls `phase.publish(phase, phase_seq)`. The driver starts at `(Waiting, 0)`, which is
  what C12's endpoint serves before the first change, so the first change publishes `seq 1`. The value
  is a projection of domain state, not a domain `Effect`, so the domain API doesn't change for it.

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
          // (amended 2026-10-18, C07 as built) fixture_id is `Option<String>` (F-006): Some("<fixture fixtureId>".into())
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
  // SUPERSEDED 2026-10-15 by `ending_holds_final_then_drains_stops_children_and_exits_zero` (amendment):
  // an Ending reply now holds the Final scene for final_hold_ms before the drain (C06). Do not write this test.
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
    - heartbeat `Err` → `Event(HeartbeatFailed)`, except C07's 410 (`Gone`) →
      `Event(HeartbeatRefused(HeartbeatRefusal::Gone))` (amended 2026-10-18, wave gate 1 I5)
    - (amended 2026-10-18, C07 as built; Owed 29) **the final beat's result maps the same way.**
      C07 returns `HeartbeatError::Gone` for every heartbeat 410, the final beat's included, and
      `HeartbeatError::Transport` for everything else (a 404 too). The worker posts each result as
      its event: `Draining` turns `HeartbeatRefused(Gone)` into `Exit(0)` (the drain's own beat may
      draw it) and ignores `HeartbeatFailed`, and `Exited` ignores both, so a result that lands after
      `Exit` changes nothing.

  `run()` loops on `rx.recv_timeout(next_tick_at − now)`. On a message it calls `step`, and on a
  timeout it calls `tick_due`. It returns the code on `Some(code)`.

- [ ] **Step 6: Run, expect a PASS.** `cargo test -p app -- --nocapture`. Also run C01's boundary
  guard, which must still be green: `app` never names `adapters`.

- [ ] **Step 7: Mutate once each.**
  1. Remove the catch-up cap. Expected red: `catch_up_is_capped…`.
  2. Apply effects after `Exit`. Expected red: `ending_drains…` ("nothing is applied after exit").
  3. Make `FetchSession` droppable by sending it through the heartbeat channel. Add a test
     `fetch_is_never_dropped_behind_a_slow_heartbeat` if no test goes red, then re-run.
  4. **(Amendment 2026-10-15)** Publish the phase on every tick, not on change. Expected red:
     `phase_is_published_on_change_only_with_a_rising_seq`.
  5. **(Amendment 2026-10-15)** Publish before `decide` (the old phase). Expected red: the same test's
     per-call order assertion (corrected 2026-10-15, review I1: the published list is unchanged under
     this mutant, so only the order leg kills it). If it survives, the order leg is missing, so fix the
     test, not the mutant list.

- [ ] **Step 8: Commit.** `feat(app): driver, control worker, ports and fakes (C08)`

## Acceptance

- The scenario and control-worker tests pass.
- Boundary guard is green.
- ~~3 mutants~~ 5 mutants went red (amended 2026-10-15).
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- (Amended 2026-10-18, Owed 17) `HideOverlay` maps to `hide_overlay()` then `graphics.stop()`.
  `hide_stops_the_hidden_page_and_the_retry_reaches_air` passes both halves, and the
  dropped-stop mutant went red. (Amended 2026-10-18, C05b PR #22 review) That holds for a `Hang` or
  `Failed` hide. A `Crash` or `Recycle` hide maps to `hide_overlay()` alone.
  `a_crash_hide_leaves_the_reload_loading` passes both its legs, and the stop-on-every-hide mutant
  went red.
- (Amended 2026-10-18, Owed 18) `hung_page_is_restarted_through_the_port` passes. Every hang gives
  `HideOverlay` + `GraphicsStop` at once (owner-approved design (2026-10-18); "at once" is the coordinator's reading of it), then one restart after
  `cef_backoff_base_ms`, and the `cef_fail_count`-th hang enters Failed.
- (Amended 2026-10-19, C08 as built) `scenarios.rs`, `control_worker.rs`, `run.rs` and `secrets.rs`
  pass, and every sweep prints its count and fails on 0. The scenario sequences the dispatch named
  pass: a crash then a reload, a hang in Reconnecting, Final with the ladder stepped, SIGTERM in the
  hold, a 410 mid-drain, a fatal error during the drain, and the final beat under a full channel.
- (Amended 2026-10-19, Owed 28) The final beat is posted under a full periodic slot, on a fatal start,
  and both of a drain's final beats behind a full slot; a hung final beat delays `run()` by at least
  and at most `FINAL_BEAT_TIMEOUT_MS` (plus one tick). The three briefed mutants went red, and so did
  the drop-oldest form of "the final beat through the periodic slot".
- (Amended 2026-10-19, Owed 35) The driver's lines go through `Ports.log`, and no `Debug` of a C08 type,
  no port call and no log line carries a planted credential or fatal text (`secrets.rs`).
- (Amended 2026-10-19, PR #27 review round 1) Final beats have their own lane (the coordinator's
  ruling): through `run()` with the real worker, the final beat posts past a periodic POST stalled
  5 s, and with a bounded(4) inbound channel kept full (the reviewer's probe), and `run()` logs no lost
  beat; at the worker, a second final beat posts while the first's result waits on a full channel,
  and the lane's plane is scrubbed of credentials learned after it was made (C07's suite). A crash at
  the count logs its line before its hide (the reviewer's mutant M13 went red). The skipped ticks and
  the dropped beats reach the log, at most a line a minute each, and the rest at the exit.
- (Amended 2026-10-20, C11b) The driver reads `GraphicsRenderer::health()` once per `tick_due` that ran a tick and
  hands every read to `decide`, with a `cef health …` line on a change and no read after `Exit`:
  `tests/cef_health.rs` (7) takes each trigger through the real driver to one final beat and one `Exit(75)`, and
  `run.rs` posts the restart's final beat before `run()` returns 75. 7 mutants red.
- (Amended 2026-10-24, Owed 86) Each beat whose sample carries a finite `fps`, in a session, logs
  `output fps beat=<fps> floor=<floor>` (two decimals; the floor `OutputProfile::fps_floor`), and the
  driver's `domain::starvation::StarvationAlarm` adds `output starvation alarm raised fps=<fps>
  floor=<floor>` on the beat below the floor and `output starvation alarm cleared fps=<fps>
  recovered=<level>` on the first beat at or above `OutputProfile::fps_recovered` after it. The
  heartbeat carries the same reading as `fps`; a beat before a session logs none; no `StepDown` follows.
  `the_real_output_fps_is_logged_each_beat_and_starvation_raises_then_clears_the_alarm` and
  `a_beat_before_any_session_writes_no_output_fps_line` pass, and the nominal fps in the line or on the
  wire, the alarm dropped, and the finite filter removed each go red.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.3 (D10) and §6, C08/C12 row (verbatim, `29fa9f59c`): "Adds the loopback `/phase` endpoint
(127.0.0.1 only; CORS + PNA headers; long-poll)."

**Ownership split, stated in both briefs:**
- **C08 owns** the port (`PhasePublisher`), the driver's publication rule, and the testkit fake.
- **C12 owns** the HTTP endpoint adapter (`crates/adapters/src/phase_http.rs`), its bind, CORS and
  Private Network Access headers, the long-poll, and the wiring in `main`.

The app crate never names the adapter (C01's boundary guard).

```rust
// ports.rs (addition)
use domain::programme::ProgrammePhase;
/// The compositor's programme phase, served to the overlay page on loopback (overlay spec §4.3). `seq`
/// rises by one per change. The adapter (C12) holds the latest value and wakes long-polls.
pub trait PhasePublisher: Send { fn publish(&mut self, phase: ProgrammePhase, seq: u64); }
// Ports gains `pub phase: Box<dyn PhasePublisher>`; the Debug impl prints "Ports { input, programme, graphics, metrics, control, phase }".

// testkit fakes.rs
// Call gains `Phase(ProgrammePhase, u64)`.
#[derive(Debug)] pub struct FakePhase(pub Log);
impl PhasePublisher for FakePhase { fn publish(&mut self, p: ProgrammePhase, seq: u64) { self.0.push(Call::Phase(p, seq)) } }
// `ports()` wires `phase: Box::new(FakePhase(log.clone()))`.
```

**Tests** (in `crates/app/tests/scenarios.rs`, failing first):
- `phase_is_published_on_change_only_with_a_rising_seq`. Walk spec §5's sequence through the real
  driver with the fakes:
  - frames until Live
  - stop frames past `frozen_after_ms` (Frozen), then past `slate_after_ms` (Reconnecting)
  - frames past `resume_after_ms` (Live)
  - an `Ending` reply (Final)
  - tick through `final_hold_ms` and drain

  Every duration comes from `Thresholds::default()`. Assert the `Call::Phase` list is exactly
  `[(Live,1), (Frozen,2), (Reconnecting,3), (Live,4), (Final,5)]`, with no repeats across the hundreds
  of ticks in between.

  **Order, within each call (review I1):** after **every** `d.step(..)` and `d.tick_due(..)` call, the
  phase of the last `Call::Phase` in the log, or `Waiting` if there is none yet, equals
  `d.programme_phase()`. So the publication for a change lands in the same call as the `decide` that
  made it, never one call later. The list assertion alone can't see that: a one-call lag publishes the
  same list. Advance `now` by one tick period per `tick_due` call, so each call runs exactly one tick;
  a call that ran several would let a later tick in it hide the lag. Print `ticks driven: N, calls checked: M, phases published: 5`; fail if N or M is 0.
- `ending_holds_final_then_drains_stops_children_and_exits_zero`, replacing the superseded test:
  - an `Ending` reply at 20: no `Call::Drain` yet
  - every push from 20 to `20 + final_hold_ms` is `Slate(SlateKind::Final)`
  - the first tick at or after `20 + final_hold_ms` → `Call::Drain`
  - `OutputDrained` → `Some(0)`; input and graphics stopped
  - nothing is applied after the exit
- `holding_ticks_still_push_frames_at_the_profile_rate`: over the hold, the pushed indices are
  consecutive (the hold never pauses the programme clock: a frozen output would trip YouTube's gap
  timer, F-004).

## Amendment 2026-10-18 (the hang beat; the ladder's rungs)

Owner, 2026-10-18, in chat, relayed by the coordinator: "2 Ok", approving hang detection (`docs/_INDEX.md` Owed 18). Also owner, 2026-10-18, in chat, relayed by the coordinator: "no lite mode; if CPU impacts, measure and raise the CPU size" (`docs/_INDEX.md` Owed 20).

- **Mapping.** `Inbound::Event(Event::CefAlive{gen})` is handed straight to `decide`, like every
  other event. No driver rule changes: a hang becomes the domain's `RestartGraphics{gen}`, which the
  driver applies as `graphics.restart(gen, &cfg.overlay_url)`.
- **`FakeGraphics` gains `alive(gen)`.** It is a testkit helper that returns
  `Inbound::Event(Event::CefAlive{gen})`, so scenario tests can send the beat by name.
- **New scenario test:** `hung_page_is_restarted_through_the_port`.
  - Ready gen 1, then beats every `cef_alive_every_ms` to t = 30 s, then silence.
  - The log shows `Call::HideOverlay`, then `Call::GraphicsStop`, at the first tick at or after
    `last_alive + cef_hang_after_ms`. That is the hide at once (owner-approved design (2026-10-18); "at once" is the coordinator's reading of it).
  - Then exactly one `Call::GraphicsRestart{gen: 2}`, at the first tick at or after
    `+ cef_backoff_base_ms`.
  - Repeat the hang `cef_fail_count` times within `cef_fail_window_ms`. Every hang logs
    `HideOverlay`. The last one enters Failed, and no `GraphicsRestart` follows until `retry_at`.
  - Times come from `Thresholds::default()`.
- **`StepDown(r)` → `programme.step_down(r)`:** `r` is only ever `OverlayHalfRate` (C05). The
  existing overrun scenario asserts exactly one `step_down` call for a session of sustained overruns.

### Owed 17: the hide stops the hidden page and lifts for the retried one (Owner, 2026-10-18, in chat, relayed by the coordinator: "apply rec 4-9")

The coordinator closed Owed 17 on 2026-10-16. C08 maps `HideOverlay` to `programme.hide_overlay()`
**and** `graphics.stop()` (amended 2026-10-18, C05b PR #22 review: for a `Hang` or `Failed` hide;
see the scenario below for `Crash`). Stopping the hidden page means no late ready can re-open it, and it
frees the page's CPU for the cooldown.

- **New scenario test** `hide_stops_the_hidden_page_and_the_retry_reaches_air`, with
  `Thresholds::default()`. Run `cef_fail_count` load timeouts: Ready never arrives, so this is
  `?noready=1` as the domain sees it. Then:
  - **The negative half.** The log has `HideOverlay` and then `GraphicsStop`, in that order, and
    no `GraphicsRestart` before `retry_at`. A `CefReady` from the stopped generation, sent after
    the hide, changes nothing: `health().graphics` stays `failed`.
  - **The positive pair.** At the first tick at or after `retry_at`, `GraphicsRestart{gen: n+1}`
    appears. `CefReady{n+1}` makes `health().graphics` read `ready`. No `HideOverlay` follows, and
    no extra call is needed to lift the hide: there is no `show_overlay` port, so nothing in the
    driver holds a hide flag. (Amended 2026-10-18, C05b: "no `HideOverlay` follows" holds while
    generation n+1 beats, `FakeGraphics::alive(n+1)` every `cef_alive_every_ms`, or within
    `cef_hang_after_ms` of its ready. A Ready page that never beats hangs, and that hide is correct.)
  - Print the calls checked, and fail on 0.
- **New mutant:** drop `graphics.stop()` from the `HideOverlay` mapping. Expected red: the
  negative half.

### The crash and recycle hides leave the reload loading (amended 2026-10-18, C05b PR #22 review; coordinator decisions, relayed)

The frame on air stays on air at most `cef_hang_after_ms` after its page's last beat (C05b's one
bound, round 2 of the review). After a crash or a recycle, the domain then returns
`Hide { reason: Crash | Recycle, gen }`, naming the page whose frame came off, usually while the
reload is loading (the 1 s back-off has run, or the recycle itself restarted). So the mapping must
not stop that reload.

- **New scenario test** `a_crash_hide_leaves_the_reload_loading`, with `Thresholds::default()`.
  `CefReady{1}`, then `CefAlive{1}` every `cef_alive_every_ms`, the last at `b`, then
  `CefCrashed{1}` at `c`, before the next beat would come.
  - `GraphicsRestart{gen: 2}` lands at the first tick at or after `c + cef_backoff_base_ms`.
    Generation 2 sends nothing.
  - At the first tick at or after `b + cef_hang_after_ms` (the last beat, not the crash), the log
    has `Call::HideOverlay`, and **no** `Call::GraphicsStop`, then or before generation 2's ready.
  - `CefReady{2}` comes 1 ms later, inside generation 2's load timeout. `health().graphics` reads
    `ready`, and no further `HideOverlay` follows while generation 2 beats.
  - The positive pair: a hang of generation 2 still logs `HideOverlay` then `GraphicsStop`.
  - **The recycle leg**, with `cef_recycle_every_ms: 60_000` (the length is not under test).
    Generation 1 beats until its recycle, the last beat at `b`; `GraphicsRestart{gen: 2}` lands on
    the recycle's tick, and generation 2 sends nothing. At the first tick at or after
    `b + cef_hang_after_ms`, the log has `Call::HideOverlay` with the log line
    `overlay hidden reason=recycle gen=1`, and no `Call::GraphicsStop`.
  - Print the calls checked, and fail on 0.
- **New mutant:** map every `HideOverlay` to `hide_overlay()` + `graphics.stop()`. Expected red:
  the scenario's "no `GraphicsStop`", in both legs.

## Amendment 2026-10-18 (wave gate 1 review of C06, PR #24; the coordinator's decisions)

**The final beat is never dropped, and it is flushed before exit** (review I4; `docs/_INDEX.md`
Owed 28). As briefed, the final beat could be lost two ways: `ControlHandle::request` sends every
beat by `try_send` on a `bounded(1)` slot and drops it on `Full` (Step 5), so a final beat behind a
slow periodic one was dropped and counted; and `Exit(c)` returns at once, so `main`'s
`process::exit` (C12) could kill the worker before it posted. The coordinator's ruling:
- A `SendHeartbeat(h)` whose `h.exit` is `Some` goes on its **own slot, never dropped**, like
  `FetchSession`'s channel: capacity 2, because a run sends at most two (the drain's, then a fatal
  error's during the drain, C06 review m2), so its `send` never blocks the frame clock. The worker
  `select!`s it with priority over the periodic slot. Periodic beats keep `try_send` and the dropped
  counter.
- `run()` returns the exit code only once the final beat's request has completed or C07's 2 s
  timeout has passed (for example, the worker acks each final beat on a channel that `run()` waits on
  with `recv_timeout`, bounded by 2 s from `Exit`; amended 2026-10-18, C07 as built: the bound is
  `app::ports::FINAL_BEAT_TIMEOUT_MS`, the one value C07's final-beat request is capped at too). The flush is bounded, so `Exit` is never delayed
  past 2 s (C07's final-beat section, reconciled with C12's). An exit with no final beat (a heartbeat
  410) has nothing to flush and returns at once.
- Tests (`control_worker.rs`, `driver.rs`):
  - `the_final_beat_is_posted_under_a_full_heartbeat_channel`: a `ControlPlane` whose `heartbeat`
    sleeps 300 ms; fill the periodic slot (one in flight, one pending), then decide
    `Event::Fatal { detail: Some("x") }`. The plane receives the `Stopped` beat (exit `fatal`, detail
    `x`) before `run()` returns 70.
  - `the_final_beat_is_posted_on_a_fatal_start`: `programme.start` returns `Err("boom")`. The plane
    receives a `Stopped` beat whose `exit.detail` is `Some("boom")`, then `run()` returns 70.
  - `a_hung_final_beat_delays_exit_at_most_2_s`: the plane's `heartbeat` sleeps 10 s. `run()`
    returns within 2 s + one tick of `Exit`.
  - `a_heartbeat_410_exits_0_with_no_beat`: the plane answers a periodic beat with C07's `Gone`.
    `run()` returns 0, and the plane receives no beat after the refused one.
- **Mutants:** send the final beat through the periodic `try_send` slot (expected red: the full
  channel test); return from `run()` without waiting for the flush (expected red: the fatal-start
  test); map C07's `Gone` to `HeartbeatFailed` (expected red: the 410 test).

**Fatal errors carry their text** (Owed 27, option a). Every `Event::Fatal` the driver decides
(today, a start `Err`) carries the error's own text, `Some(e.to_string())`, unredacted: C07 is the
one redactor, and an empty text reaches the wire as no `exitDetail`.

## Amendment 2026-10-19 (C07 PR #25 review: `ExitDetail`; the fallible plane)

- The fatal text is now `domain::health::ExitDetail` (Owed 31, closed in C07's PR #25). Every
  `Event::Fatal { detail: Some(e.to_string()) }` above reads
  `Event::Fatal { detail: Some(ExitDetail::new(e.to_string())) }`. Every test sketch's
  `Some("x")`/`Some("boom")` reads `Some(ExitDetail::new("x"))`. Its `Debug` prints the length
  alone, so the derived `Debug` on `ControlRequest`, `Inbound` and `Driver` is safe. Never log the
  text through `as_str` raw.
- (Amended 2026-10-19, the coordinator, C07 PR #25 confirm pass) A fatal text logged to stderr goes
  through a `Scrubber` built from the `SessionConfig` (its credentials, by value) AND `redact` (the
  patterns), never `redact` alone: a stream key in a GStreamer error follows no marker. The app
  crate never names the adapters crate (the layering guard), so the driver never logs a fatal text
  itself. It hands the text to the control port, whose wire scrubs it, and C12's binary logs it
  through that `Scrubber` (C12's amendment of this date). Before the session is fetched, only
  `redact` plus the job token's scrub apply.
- C07's final beat can still hang in DNS: ureq 2 never bounds a lookup (C07's amendment I1). The
  flush's own `recv_timeout` bound, and its mutant, stay as written (Owed 28).

## Amendment 2026-10-19 (C08 as built)

The implementer's choices, inside the brief. None is an owner ruling; each medium one is recorded in
`docs/_INDEX.md` (decision row "C08, as built") for the coordinator.

- **A log port.** C12's amendment of 2026-10-18 has the driver log `overlay hidden reason=<r> gen=<g>`
  and `cef ready|crashed|alive gen=<n>`, but this brief gave it no port, and the app crate reads no
  clock and never names the adapters crate. So `ports.rs` adds
  `pub trait LogSink: Send { fn info(&mut self, line: &str); }` and `Ports.log`. The driver writes the
  message alone; C12's adapter adds `ts=<rfc3339> level=info msg=` (C12's amendment of this date).
  The driver logs each hide's line before `hide_overlay()`, and each graphics event's line before it
  hands the event to `decide`, a stale one included. No line carries a credential or a fatal text.
- **`ControlQueue` gained `flush(timeout) -> bool`** (the 2026-10-18 amendment's ack): it waits on a
  `Condvar` for every final beat requested so far to post, bounded by its own clock. `run()` calls
  it once, after `Exit`, with `FINAL_BEAT_TIMEOUT_MS`. It returns false at once when a final beat
  found its lane gone (review round 1). It also gained `dropped() -> u64` (round 1, item 4).
- **The slots.** The final beat's slot has capacity 2 and a blocking `send` (a third final still
  posts; its `request` waits for room). The fetch's slot has capacity 1 and a blocking `send`. The
  periodic slot drops the OLDEST beat, not the newest: AGENTS.md §4 ("every queue and channel has a
  cap and drops the oldest"); the brief's `try_send`-and-count would drop the newest. The handle
  holds a clone of the slot's receiver to retire the pending beat, and counts it in `dropped`. A final
  beat also retires a pending periodic beat, so no periodic beat STARTS after a final one. (Corrected
  by review round 1: this said no `playing` beat reaches web after a `stopped` one, and that it made
  the final-over-periodic priority equivalent, mutant W12. W12 was killed by
  `a_final_beat_outranks_a_periodic_beat_requested_after_it` before round 1; since round 1 the lanes
  share no priority, and a periodic beat already IN FLIGHT runs on, so it may land after the final
  beat.)
- **The worker** (as corrected by review round 1, the coordinator's ruling, medium): two threads.
  `control` takes the fetch, then a periodic beat (`try_recv` in that order, then `Select::ready`
  over the open slots), and posts each result with a blocking `send`, so a full inbound channel holds
  it alone. `control-final` serves the final beats over a second plane instance,
  `ControlPlane::final_lane()`: its own connection, sharing every credential the first instance knows
  or later learns (`HttpControlPlane` shares its `Scrubber` behind a mutex held only while a text is
  scrubbed), so a final beat's text is scrubbed as before. The final lane acknowledges each beat for
  `flush` as its call returns, and selects between taking the next final beat and posting a result
  still waiting for room, so neither a periodic beat in flight, however long, nor a full inbound
  channel holds a final beat or `flush` up. (Before round 1 one thread served all three slots, and
  this said that acknowledging a final beat before posting its result meant a full inbound channel
  could not hold up `flush`. That was false: a final beat queued behind a periodic beat in flight, so
  a periodic POST stalled past 2 s lost it, and a periodic result blocked on a full channel held it
  for good, the reviewer's probes P1 and P2.) Both threads end when the handle drops; the final lane
  serves on if the loop has gone first. If either thread cannot start, it posts `Event::Fatal`. No
  result's text is kept or logged.
- **The counters reach the log** (review round 1, item 4, medium): `skipped_ticks` and the queue's
  `dropped()` were read only by tests. The driver logs `ticks skipped total=<n>` after a catch-up
  jump and `heartbeats dropped total=<n>` after a beat it requests, each when its count rose, at most
  one line a minute per counter (`TALLY_EVERY_MS`), and whatever rose since the last line at the
  exit, before it.
- **`run()` drops the ports as it returns** (review round 1, item 5): the driver, and with it every
  adapter, drops after the flush, so an adapter's `Drop` runs on the exit path. C09, C10 and C11 are
  amended: an adapter's `Drop` returns within a bound and never waits on a hung pipeline or renderer.
- **The tick grid is exact:** tick `i` is due at `origin + ⌈i·1000 / fps⌉` ms, not at a period rounded
  to the nanosecond. The rounded period drifts off the millisecond grid: at 60 fps tick 3 is due at
  exactly 50 ms, and `16 666 667 ns × 3` puts it at 51 ms. The catch-up cap is one second of ticks at
  the current rate (`fps` after the session, 10 before it), oldest first, and every tick in one call
  is decided at that call's `now`.
- **Refusals, each with a test:** a second `Session` changes nothing (the session is taken only
  while Booting). An inbound `Event::Tick` is refused (the driver owns the tick). An inbound
  `BootstrapOk` leaves the domain Running with no session; its `StartProgramme` (and any other wiring
  effect) then exits as a fatal error, "a session effect arrived before a session" (70), never a
  panic. A session whose `output.fps` is 0 cannot be paced and is refused as `WrongMode` (78);
  `parse_session` accepts only P720 and P1080, so it is a guard on the public type. The loop's
  inbound channel closing (every sender gone) exits as a fatal error, "inbound channel closed" (70),
  with its final beat.
- **`FakeProgramme.fail_start` is `Option<String>`, not `bool`:** the error's text is the fatal
  detail the test pins (`"boom"`).
- **The flush outcome is logged:** when `flush` returns false (a final beat hung, as in DNS, or its
  worker died), `run()` logs `final beat not confirmed within 2000 ms` through `Ports.log` before it
  returns. A final beat whose call returned an error is not logged here: the worker keeps no text
  (Owed 35 routes it to C12).
- **Tests as built:** `scenarios.rs` (32), `control_worker.rs` (15), `run.rs` (7) and `secrets.rs` (1).
  The scenario sequences the dispatch named: a crash then a reload (`a_crash_hide_leaves_the_reload_loading_1_crash`,
  `_2_a_hang_after_it_still_stops`, `_3_recycle`), a hang in Reconnecting, Final with the ladder
  stepped, SIGTERM in the hold, a 410 mid-drain (in the driver and through the worker), a fatal error
  during the drain (in the driver, and behind a full slot through `run()`), and the final beat under
  a full channel. `secrets.rs` plants made-up credentials and a fatal text, and checks `{:?}` and
  `{:#?}` of every C08 type and every port call, and every log line, with the positive pair (the
  values reached the ports and the beat).
- **Mutation:** 72 hand mutants of `driver.rs`, `control_worker.rs` and `run.rs` (plus one of C07's
  `InputSpec` `Debug`, to prove `secrets.rs` is not vacuous), each run against the whole `app` suite
  with its collected total pinned at the baseline: 70 red, 2 argued equivalent. The brief's five
  went red (the "apply effects after `Exit`" mutant in two forms, B2a and B2b), and so did the three
  of the 2026-10-18 amendment, the stop-on-every-hide and dropped-stop ones, and the drop-oldest form
  of "the final beat through the periodic slot" (killed only by the fatal-during-the-drain test and
  the flush). **Equivalent, argued:** B2c (`tick_due` ignores the exit latch): an `Exited`
  supervisor decides nothing on a tick (no effect, no frame, no phase change), so nothing reaches a
  port either way. W16 (the worker's `return None` when no slot is open, removed):
  `Select::ready()` then panics on an empty set (crossbeam: "no operations have been added"), which
  ends the thread and closes its sender just as the return does; the explicit return is the
  intended path, and the only difference is a panic line at shutdown. That holds only while a panic
  unwinds: it breaks if the workspace ever sets `panic = "abort"` or installs a panic hook that exits.
- **Review round 1 (tests and mutation):** 10 new tests (`scenarios.rs` 4: the crash at the count and
  the counters' three; `control_worker.rs` 3; `run.rs` 2; C07's `control_http.rs` 1, the lane's
  scrub), each red first but three (the crash at the count, the lane serving on after the loop has
  gone, and the counters' empty case), which mutants cover. The list was re-pinned to the lane and
  re-run whole against the app suite, its total pinned at 69 (and C07's suite at 59 for the scrub):
  91 hand mutants, 90 red, 1 argued equivalent (W16). M13 (the graphics line after `decide`) and
  every Owed-28 mutant (the final beat through the periodic slot in both forms, no flush, the
  flush's bounds, the ack after the post, before the call and dropped, the lost beat ignored and
  never recorded, and five new on the lane: a blocking post, no take while a result waits, the lane
  ending when the loop has gone, a result dropped, results newest first) went red. B2c is now red: a
  late tick past an unlatched exit logs a skip. W12 is gone (the lanes share no priority).

## Amendment 2026-10-20 (C11b: CEF's health)

- **The port:** `GraphicsRenderer` gains `fn health(&self) -> CefHealth` (the domain's type): counters only, never
  blocking. `testkit`'s `FakeGraphics` reads a `CefKnob` (`ports_with_cef`), which counts the reads.
- **The read:** once per `tick_due` that ran a tick, after its ticks (an exit inside them returns first, so nothing is
  read after `Exit`). Every read goes to `decide` as `Event::CefHealth`, a changed one or not: the domain alone
  decides the in-place restart, and a report read while booting (ignored there) must still restart on the first tick
  on air.
- **The line:** `cef health zygote_gone=<gen|-> leaked_teardowns=<n> gpu_process_deaths=<n>`, when the read differs
  from the last one (none for a healthy process).
- **The exit:** the decision's `Exit(75)` takes the existing arm: input and graphics stopped, the tallies' rest
  logged, `run()`'s bounded flush, then 75.

## Amendment 2026-10-20 (C10b: `InputSource::abandon`; Owed 50)

- **`InputSource::abandon(&mut self, gen: u64)`**: the domain's word that it gave generation `gen` up
  by its own rule. Before it returns, nothing more of that generation passes the adapter; a stale
  `gen`, one after `stop`, and a second call change nothing; it never blocks. The driver applies
  `Effect::AbandonInput { gen }` as `p.input.abandon(gen)` (no session needed). `FakeInput` logs
  `Call::InputAbandon(gen)`.
- Scenarios through the real driver: the stall rule and the connect timeout (each generation once,
  at the first tick at or after the rule's instant, before the next rebuild), reported losses (none),
  the hold (none), and a given-up generation's late frames (never on air). Frames in the scenarios
  carry their generation (`frame(gen)`); a walk that feeds frames past the 5 s connect timeout
  connects the generation first, as the adapter does.

## Amendment 2026-10-24 (Owed 86: the honest beat)

The coordinator's recommendation, approved by the owner 2026-10-24 (in chat, relayed by the
coordinator: "Ok"): the beat reports the programme's REAL output frame rate, measured from the
frames actually put out over the beat window and never the nominal configured fps, and raises a
starvation alarm when it falls below a floor; the ladder does not change (`docs/_INDEX.md` Owed 86,
decision log "Owed 86: the honest beat", where the floor, the clear level and the minimum window are
recorded as the implementer's medium choices).
- **The domain** gains (additions; no existing API changes): `config::OUTPUT_FPS_FLOOR_SHARE`
  (= `ENCODER_SPEED_REALTIME_MIN`, 0.95), `OUTPUT_FPS_RECOVERED_SHARE` (29.5/30, C00 Step 4 and C14
  cases 6 and 10), `OUTPUT_FPS_MIN_WINDOW_MS` (1 000), `OutputProfile::fps_floor` and `fps_recovered`,
  and `starvation::StarvationAlarm` (`on_reading(fps, profile) -> Option<Alarm>`: raised below the
  floor, cleared at or above the recovery level once raised, each change once; a reading that is not
  finite or is negative changes nothing).
- **The driver**, at `Effect::SendHeartbeat`, after the camera line: the `output fps` line and the
  alarm's, as the acceptance bullet of this date says, then the heartbeat with the sample unchanged.
  `Measured.fps` keeps its type (`app::ports` documents the new meaning). The ladder is not told: its
  input stays the encoder queue's overruns.

## Do not touch

The domain crate's API, and the adapters (except to keep them compiling). (Amended 2026-10-15) The
endpoint's HTTP code is C12's; this task adds no adapter.

## Output cap

Under 15 lines.
