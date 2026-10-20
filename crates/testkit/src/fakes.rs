//! A fake for every port (house rule: fakes before live adapters). Each records every call in one
//! shared `Log`, a cloneable handle, so a test reads the calls after moving the boxed fakes into
//! `Ports`, and sees the order across ports.
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use app::ports::{
    redacted_origin, ControlPlane, ControlQueue, ControlRequest, FetchError, GraphicsRenderer,
    HeartbeatError, Inbound, InputSource, InputSpec, LogSink, Measured, Metrics, PhasePublisher, Ports,
    ProgrammeOutput, SessionConfig, TargetSpec,
};
use domain::cef_health::CefHealth;
use domain::config::OutputProfile;
use domain::health::HealthReport;
use domain::ladder::Rung;
use domain::programme::{FrameChoice, ProgrammePhase};
use domain::supervisor::{Desired, Event};

/// One port call. Its `Debug` is written by hand: `GraphicsRestart` carries the overlay URL, whose
/// query holds the page token, so it prints the URL's origin alone (AGENTS.md §7). Every other field
/// is safe to derive: `InputSpec` and `TargetSpec` write their own `Debug` (C07), and a report's fatal
/// text is an `ExitDetail`.
#[derive(Clone, PartialEq)]
pub enum Call {
    InputRebuild(u64, InputSpec),
    /// `InputSource::abandon(gen)` (C10b).
    InputAbandon(u64),
    InputStop,
    Start(OutputProfile),
    Push(u64, FrameChoice),
    Swap(u64, TargetSpec),
    StepDown(Rung),
    HideOverlay,
    Drain,
    GraphicsRestart(u64, String),
    GraphicsStop,
    Control(ControlRequest),
    /// `ControlQueue::flush` (only `run()` calls it).
    Flush,
    Phase(ProgrammePhase, u64),
    /// A `LogSink::info` line.
    Log(String),
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Call::InputRebuild(g, s) => f.debug_tuple("InputRebuild").field(g).field(s).finish(),
            Call::InputAbandon(g) => f.debug_tuple("InputAbandon").field(g).finish(),
            Call::InputStop => f.write_str("InputStop"),
            Call::Start(p) => f.debug_tuple("Start").field(p).finish(),
            Call::Push(i, c) => f.debug_tuple("Push").field(i).field(c).finish(),
            Call::Swap(g, t) => f.debug_tuple("Swap").field(g).field(t).finish(),
            Call::StepDown(r) => f.debug_tuple("StepDown").field(r).finish(),
            Call::HideOverlay => f.write_str("HideOverlay"),
            Call::Drain => f.write_str("Drain"),
            Call::GraphicsRestart(g, url) => f
                .debug_tuple("GraphicsRestart")
                .field(g)
                .field(&redacted_origin(url))
                .finish(),
            Call::GraphicsStop => f.write_str("GraphicsStop"),
            Call::Control(r) => f.debug_tuple("Control").field(r).finish(),
            Call::Flush => f.write_str("Flush"),
            Call::Phase(p, s) => f.debug_tuple("Phase").field(p).field(s).finish(),
            Call::Log(l) => f.debug_tuple("Log").field(l).finish(),
        }
    }
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A test that panicked while holding the lock must not hide the calls from the next reader.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The shared call log.
#[derive(Debug, Clone, Default)]
pub struct Log(pub Arc<Mutex<Vec<Call>>>);

impl Log {
    pub fn push(&self, c: Call) {
        locked(&self.0).push(c);
    }

    pub fn calls(&self) -> Vec<Call> {
        locked(&self.0).clone()
    }

    pub fn len(&self) -> usize {
        locked(&self.0).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The calls from position `from` on (a long scenario reads only what one step added).
    pub fn calls_from(&self, from: usize) -> Vec<Call> {
        let calls = locked(&self.0);
        calls.get(from..).map(<[Call]>::to_vec).unwrap_or_default()
    }

    pub fn count(&self, f: impl Fn(&Call) -> bool) -> usize {
        locked(&self.0).iter().filter(|c| f(c)).count()
    }

    /// The `LogSink` lines, in order.
    pub fn lines(&self) -> Vec<String> {
        locked(&self.0)
            .iter()
            .filter_map(|c| match c {
                Call::Log(l) => Some(l.clone()),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug)]
pub struct FakeInput(pub Log);

impl InputSource for FakeInput {
    fn rebuild(&mut self, gen: u64, spec: &InputSpec) {
        self.0.push(Call::InputRebuild(gen, spec.clone()));
    }

    fn abandon(&mut self, gen: u64) {
        self.0.push(Call::InputAbandon(gen));
    }

    fn stop(&mut self) {
        self.0.push(Call::InputStop);
    }
}

/// `fail_start: Some(text)` makes `start` fail with that text (the brief's `fail_start: true` and
/// `"boom"`, generalised so a test can plant a credential in the error).
#[derive(Debug)]
pub struct FakeProgramme {
    pub log: Log,
    pub fail_start: Option<String>,
}

impl ProgrammeOutput for FakeProgramme {
    fn start(&mut self, p: &OutputProfile) -> Result<(), String> {
        self.log.push(Call::Start(*p));
        match &self.fail_start {
            Some(text) => Err(text.clone()),
            None => Ok(()),
        }
    }

    fn push(&mut self, i: u64, c: FrameChoice) {
        self.log.push(Call::Push(i, c));
    }

    fn swap_sink(&mut self, gen: u64, t: &TargetSpec) {
        self.log.push(Call::Swap(gen, t.clone()));
    }

    fn step_down(&mut self, r: Rung) {
        self.log.push(Call::StepDown(r));
    }

    fn hide_overlay(&mut self) {
        self.log.push(Call::HideOverlay);
    }

    fn drain(&mut self) {
        self.log.push(Call::Drain);
    }
}

/// CEF's health as `FakeGraphics` reports it (C11b): a test sets it, and reads how many times the
/// driver asked. A cloneable handle, like `Log`, so it survives the move into `Ports`.
#[derive(Debug, Clone, Default)]
pub struct CefKnob(Arc<Mutex<(CefHealth, u64)>>);

impl CefKnob {
    pub fn set(&self, h: CefHealth) {
        locked(&self.0).0 = h;
    }

    /// `GraphicsRenderer::health` calls so far.
    pub fn reads(&self) -> u64 {
        locked(&self.0).1
    }
}

/// Records `restart` and `stop`; `health` reads the knob (a healthy process until a test sets it).
#[derive(Debug)]
pub struct FakeGraphics(pub Log, pub CefKnob);

impl FakeGraphics {
    /// The page's `seazn:alive` beat as C11 relays it (Owed 18), so a scenario sends it by name.
    pub fn alive(gen: u64) -> Inbound {
        Inbound::Event(Event::CefAlive { gen })
    }
}

impl GraphicsRenderer for FakeGraphics {
    fn restart(&mut self, gen: u64, url: &str) {
        self.0.push(Call::GraphicsRestart(gen, url.into()));
    }

    fn stop(&mut self) {
        self.0.push(Call::GraphicsStop);
    }

    fn health(&self) -> CefHealth {
        let mut k = locked(&(self.1).0);
        k.1 += 1;
        k.0
    }
}

#[derive(Debug, Default)]
pub struct FakeMetrics(pub Measured);

impl Metrics for FakeMetrics {
    fn sample(&mut self) -> Measured {
        self.0.clone()
    }
}

/// Records each request. `flush` has nothing to wait for and reports every final beat posted.
/// `dropped` reads `dropped`, a count the test sets (no beat is ever dropped here).
#[derive(Debug)]
pub struct FakeControl {
    pub log: Log,
    pub dropped: Arc<AtomicU64>,
}

impl ControlQueue for FakeControl {
    fn request(&mut self, r: ControlRequest) {
        self.log.push(Call::Control(r));
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }

    fn flush(&mut self, _timeout: Duration) -> bool {
        self.log.push(Call::Flush);
        true
    }
}

#[derive(Debug)]
pub struct FakePhase(pub Log);

impl PhasePublisher for FakePhase {
    fn publish(&mut self, p: ProgrammePhase, seq: u64) {
        self.0.push(Call::Phase(p, seq));
    }
}

#[derive(Debug)]
pub struct FakeLog(pub Log);

impl LogSink for FakeLog {
    fn info(&mut self, line: &str) {
        self.0.push(Call::Log(line.to_owned()));
    }
}

/// Every port faked onto one shared log.
pub fn ports(log: &Log, metrics: Measured) -> Ports {
    ports_with_cef(log, metrics, &CefKnob::default())
}

/// `ports`, with CEF's health read from `cef` (C11b).
pub fn ports_with_cef(log: &Log, metrics: Measured, cef: &CefKnob) -> Ports {
    Ports {
        input: Box::new(FakeInput(log.clone())),
        programme: Box::new(FakeProgramme {
            log: log.clone(),
            fail_start: None,
        }),
        graphics: Box::new(FakeGraphics(log.clone(), cef.clone())),
        metrics: Box::new(FakeMetrics(metrics)),
        control: Box::new(FakeControl {
            log: log.clone(),
            dropped: Arc::default(),
        }),
        phase: Box::new(FakePhase(log.clone())),
        log: Box::new(FakeLog(log.clone())),
    }
}

/// Every value is copied by hand from `contracts/fixtures/relay-job.v1/session-composed-720p.json`
/// (never computed). C12's `fake_session_matches_the_vendored_fixture` asserts equality with
/// `parse_session(fixture)`, so a re-vendor that changes the fixture reds there instead of drifting.
pub fn session_720p() -> SessionConfig {
    SessionConfig {
        session_id: "3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c".into(),
        fixture_id: Some("a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d".into()),
        input: InputSpec {
            url: "srt://live.cloudflare.com:778".into(),
            stream_id: "fixture-input-a-playback-stream-id-720p".into(),
            passphrase: "fixture-input-a-playback-passphrase-720p".into(),
        },
        target: TargetSpec {
            url: "rtmps://live.cloudflare.com:443/live/".into(),
            stream_key: "fixture-input-b-stream-key-720p".into(),
        },
        output: OutputProfile::P720,
        heartbeat_interval_ms: 5_000,
        overlay_url: "https://seazn.club/overlay/fixtures/a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d?sid=3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c&st=fixture-page-token-720p&host=compositor&delay=3100".into(),
        overlay_delay_ms: 3_100,
    }
}

/// What a `ScriptedPlane` saw. `BeatStarted` is recorded as a heartbeat call begins and `Beat` once
/// it returns, so "the plane received the beat" means its call completed.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaneCall {
    FetchStarted,
    Fetched,
    BeatStarted(HealthReport),
    Beat(HealthReport, Measured),
}

/// The scripted plane's record, readable from the test while the worker owns the plane.
#[derive(Debug, Clone, Default)]
pub struct PlaneLog(Arc<Mutex<Vec<PlaneCall>>>);

impl PlaneLog {
    pub fn calls(&self) -> Vec<PlaneCall> {
        locked(&self.0).clone()
    }

    /// The reports of every completed heartbeat, in order.
    pub fn beats(&self) -> Vec<HealthReport> {
        locked(&self.0)
            .iter()
            .filter_map(|c| match c {
                PlaneCall::Beat(h, _) => Some(h.clone()),
                _ => None,
            })
            .collect()
    }

    fn push(&self, c: PlaneCall) {
        locked(&self.0).push(c);
    }

    /// Waits until `pred` holds over the calls, polling every 2 ms, for at most `timeout`. A bounded
    /// wait: it returns false rather than hang, so the test fails on its own message.
    pub fn wait_for(&self, timeout: Duration, pred: impl Fn(&[PlaneCall]) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if pred(&locked(&self.0)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

type Reply = Box<dyn FnMut(&HealthReport) -> (Duration, Result<Desired, HeartbeatError>) + Send>;

/// A scripted plane's script, shared with its final-lane instance.
struct Script {
    session: SessionConfig,
    fetches: VecDeque<Result<SessionConfig, FetchError>>,
    fetch_delay: Duration,
    reply: Reply,
}

/// A `ControlPlane` for the control worker's tests and the binary's fake mode (C12: "a scripted
/// control plane that serves `session_720p()` and replies `live`"). Fetches answer from `fetches` in
/// order, then `Ok(session)` for ever. Each heartbeat calls `reply`, which says how long the call
/// takes and what it returns; the default answers `Live` at once. Its `final_lane` shares the script
/// and the record, and runs its calls apart: no lock is held while a call takes its time.
pub struct ScriptedPlane {
    script: Arc<Mutex<Script>>,
    log: PlaneLog,
}

impl fmt::Debug for ScriptedPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = locked(&self.script);
        f.debug_struct("ScriptedPlane")
            .field("session", &s.session)
            .field("fetches", &s.fetches.len())
            .finish_non_exhaustive()
    }
}

impl ScriptedPlane {
    pub fn new(session: SessionConfig) -> (Self, PlaneLog) {
        let log = PlaneLog::default();
        let script = Script {
            session,
            fetches: VecDeque::new(),
            fetch_delay: Duration::ZERO,
            reply: Box::new(|_| (Duration::ZERO, Ok(Desired::Live))),
        };
        let plane = Self {
            script: Arc::new(Mutex::new(script)),
            log: log.clone(),
        };
        (plane, log)
    }

    /// The first fetches' results, in order.
    pub fn fetches(self, results: Vec<Result<SessionConfig, FetchError>>) -> Self {
        locked(&self.script).fetches = results.into();
        self
    }

    pub fn fetch_delay(self, d: Duration) -> Self {
        locked(&self.script).fetch_delay = d;
        self
    }

    pub fn reply(
        self,
        f: impl FnMut(&HealthReport) -> (Duration, Result<Desired, HeartbeatError>) + Send + 'static,
    ) -> Self {
        locked(&self.script).reply = Box::new(f);
        self
    }
}

impl ControlPlane for ScriptedPlane {
    fn fetch_session(&mut self) -> Result<SessionConfig, FetchError> {
        self.log.push(PlaneCall::FetchStarted);
        let delay = locked(&self.script).fetch_delay;
        thread::sleep(delay);
        let r = {
            let mut s = locked(&self.script);
            let session = s.session.clone();
            s.fetches.pop_front().unwrap_or(Ok(session))
        };
        self.log.push(PlaneCall::Fetched);
        r
    }

    fn heartbeat(&mut self, report: &HealthReport, measured: &Measured) -> Result<Desired, HeartbeatError> {
        self.log.push(PlaneCall::BeatStarted(report.clone()));
        let (delay, result) = (locked(&self.script).reply)(report);
        thread::sleep(delay);
        self.log.push(PlaneCall::Beat(report.clone(), measured.clone()));
        result
    }

    fn final_lane(&self) -> Box<dyn ControlPlane> {
        Box::new(Self {
            script: Arc::clone(&self.script),
            log: self.log.clone(),
        })
    }
}
