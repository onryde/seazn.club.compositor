//! Builds every port for a driver mode and runs the loop (C12).
//!
//! | `COMPOSITOR_DRIVERS` | Ports |
//! |---|---|
//! | `live` | `HttpControlPlane`, `GstInput`, `GstProgramme`, `CefGraphics`, `ProcMetrics` |
//! | `fake` | testkit's fakes and its scripted plane (`session_720p()`, replies `live`), or with `COMPOSITOR_FAKE_CONTROL=http` the real plane against `APP_URL` |
//!
//! Both modes get the same decorators and the same stderr log sink, so every line a test reads
//! exists in both (`assemble`). Fake mode drains for real: `DrainingFake` answers `drain` with
//! `OutputDrained`, so a fake run leaves through the drain, never the drain timeout.
//!
//! The order in `run` is load-bearing:
//! - the signal thread first, so a stop during boot is never the default action (a kill);
//! - the plane next: a refused configuration (a token with a CR, an empty one) exits 78 before any
//!   adapter is built and before the loop (C07 amendment 2026-10-19), sending no beat;
//! - the phase endpoint before the graphics adapter, so the page's first poll cannot race the bind;
//! - `CefGraphics` before every other adapter (Owed 53: it installs the process's GStreamer log
//!   handler, which keeps a tokened URL out of GStreamer's default output);
//! - `GstInput` last of all, at its first `rebuild`: it scales every camera frame into the
//!   programme's size, which only the session names (`LazyInput`, sized from the profile the
//!   programme started with).
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use adapters::cef_graphics::CefGraphics;
use adapters::control_http::{HttpControlPlane, DECLARED_PROFILES};
use adapters::frame_store::FrameStore;
use adapters::gst_input::GstInput;
use adapters::gst_programme::GstProgramme;
use adapters::metrics_proc::ProcMetrics;
use adapters::phase_http::{origin_of, AllowedOrigin, PhaseHttp, RELAY_PHASE_PORT};
use adapters::stats::Stats;
use app::control_worker;
use app::driver::Driver;
use app::ports::{
    ControlPlane, ControlQueue, GraphicsRenderer, Inbound, InputSource, InputSpec, Metrics, PhasePublisher,
    Ports, ProgrammeOutput, TargetSpec,
};
use crossbeam_channel::{bounded, Receiver, Sender};
use domain::cef_health::CefHealth;
use domain::config::{OutputProfile, Thresholds};
use domain::ladder::Rung;
use domain::programme::{FrameChoice, ProgrammePhase};
use domain::supervisor::{Event, SignalKind};
use testkit::fakes::{session_720p, CefKnob, FakeGraphics, FakeInput, FakeProgramme, Log, ScriptedPlane};

use crate::clock::Clock;
use crate::env::{Drivers, Env};
use crate::logged::{LoggedGraphics, LoggedPlane, LoggedProgramme, Logger};

/// The inbound channel's bound (AGENTS.md §4; C08 as built, Owed 35). Every sender is bounded by it,
/// and none drops a worker result: the control worker and the signal thread `send` (blocking: a full
/// channel holds that thread up, never loses a `Session`, a refusal or a 410), and every adapter and
/// `DrainingFake` `try_send` (each counts or logs what it drops; the loop is the one consumer and
/// must never block on its own channel). At a tick rate of 30 a second and a handful of events per
/// tick, 1 024 is more than a second of backlog.
pub const INBOUND_CAPACITY: usize = 1_024;

/// CEF's `max-video-framerate`: one CEF per process (F-013), built before the session names its
/// profile (Owed 53), so it is the fastest declared profile's (today both are 30). A cap, not a rate:
/// the programme samples the overlay at its own.
pub fn cef_fps() -> u32 {
    DECLARED_PROFILES.iter().map(|p| p.fps).max().unwrap_or(30)
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The phase publisher when the endpoint could not bind: the run goes on (the page falls back to B
/// after 3 failures; a graphics hint never ends a run).
#[derive(Debug)]
pub struct NoopPhase;

impl PhasePublisher for NoopPhase {
    fn publish(&mut self, _phase: ProgrammePhase, _seq: u64) {}
}

/// Fake mode's programme: `drain` delegates, then reports `OutputDrained`, as the live programme does
/// once its EOS reaches the sink.
pub struct DrainingFake {
    inner: Box<dyn ProgrammeOutput>,
    tx: Sender<Inbound>,
    log: Logger,
}

impl fmt::Debug for DrainingFake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DrainingFake")
    }
}

impl DrainingFake {
    pub fn new(inner: Box<dyn ProgrammeOutput>, tx: Sender<Inbound>, log: Logger) -> Self {
        Self { inner, tx, log }
    }
}

impl ProgrammeOutput for DrainingFake {
    fn start(&mut self, profile: &OutputProfile) -> Result<(), String> {
        self.inner.start(profile)
    }

    fn push(&mut self, index: u64, choice: FrameChoice) {
        self.inner.push(index, choice);
    }

    fn swap_sink(&mut self, gen: u64, target: &TargetSpec) {
        self.inner.swap_sink(gen, target);
    }

    fn step_down(&mut self, rung: Rung) {
        self.inner.step_down(rung);
    }

    fn hide_overlay(&mut self) {
        self.inner.hide_overlay();
    }

    fn drain(&mut self) {
        self.inner.drain();
        // The loop's own channel: never block on it (the loop is the one reader).
        if self.tx.try_send(Inbound::Event(Event::OutputDrained)).is_err() {
            self.log
                .warn("fake drain: OutputDrained not queued; the drain timeout ends the run");
        }
    }
}

/// The profile the programme started with, for the input built after it.
#[derive(Debug, Clone, Default)]
pub struct ProfileSlot(Arc<Mutex<Option<OutputProfile>>>);

impl ProfileSlot {
    pub fn get(&self) -> Option<OutputProfile> {
        *locked(&self.0)
    }

    fn set(&self, p: OutputProfile) {
        *locked(&self.0) = Some(p);
    }
}

/// Records the profile a successful `start` ran with, and delegates every call.
pub struct ProfileTap {
    inner: Box<dyn ProgrammeOutput>,
    slot: ProfileSlot,
}

impl fmt::Debug for ProfileTap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileTap")
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

impl ProfileTap {
    pub fn new(inner: Box<dyn ProgrammeOutput>, slot: ProfileSlot) -> Self {
        Self { inner, slot }
    }
}

impl ProgrammeOutput for ProfileTap {
    fn start(&mut self, profile: &OutputProfile) -> Result<(), String> {
        let r = self.inner.start(profile);
        if r.is_ok() {
            self.slot.set(*profile);
        }
        r
    }

    fn push(&mut self, index: u64, choice: FrameChoice) {
        self.inner.push(index, choice);
    }

    fn swap_sink(&mut self, gen: u64, target: &TargetSpec) {
        self.inner.swap_sink(gen, target);
    }

    fn step_down(&mut self, rung: Rung) {
        self.inner.step_down(rung);
    }

    fn hide_overlay(&mut self) {
        self.inner.hide_overlay();
    }

    fn drain(&mut self) {
        self.inner.drain();
    }
}

type InputBuilder = Box<dyn FnMut(&OutputProfile) -> Box<dyn InputSource> + Send>;

/// The input, built at its first `rebuild` from the profile the programme started with (`GstInput`
/// scales every camera frame into the programme's size; no profile exists before the session). The
/// domain always starts the programme before it builds an input generation (`StartProgramme` comes
/// with `BootstrapOk`, the first `RebuildInput` on a later Running tick); a rebuild before that is
/// refused with a line, and the domain's connect timeout gives the generation up as for any input
/// that never connects. A rebuild after `stop` builds nothing.
pub struct LazyInput {
    build: InputBuilder,
    profile: ProfileSlot,
    built: Option<Box<dyn InputSource>>,
    stopped: bool,
    log: Logger,
}

impl fmt::Debug for LazyInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyInput")
            .field("built", &self.built.is_some())
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl LazyInput {
    pub fn new(build: InputBuilder, profile: ProfileSlot, log: Logger) -> Self {
        Self {
            build,
            profile,
            built: None,
            stopped: false,
            log,
        }
    }
}

impl InputSource for LazyInput {
    fn rebuild(&mut self, gen: u64, spec: &InputSpec) {
        if self.stopped {
            return;
        }
        if self.built.is_none() {
            let Some(profile) = self.profile.get() else {
                self.log.warn(&format!(
                    "input rebuild gen={gen} refused: the programme has not started, so no frame size"
                ));
                return;
            };
            self.built = Some((self.build)(&profile));
        }
        if let Some(input) = &mut self.built {
            input.rebuild(gen, spec);
        }
    }

    fn abandon(&mut self, gen: u64) {
        if let Some(input) = &mut self.built {
            input.abandon(gen);
        }
    }

    fn stop(&mut self) {
        self.stopped = true;
        if let Some(input) = &mut self.built {
            input.stop();
        }
    }
}

/// Moves the phase endpoint's allowed origin to each page's own before the page loads (F-009; PR #20
/// review M6: stg serves `/overlay` on two hosts, and the page's `Origin` is the host its URL names,
/// which web builds from its capture origin, not APP_URL). A URL with no origin keeps the previous one.
pub struct PageOrigin {
    inner: Box<dyn GraphicsRenderer>,
    origin: Option<AllowedOrigin>,
    log: Logger,
}

impl fmt::Debug for PageOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageOrigin")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl PageOrigin {
    pub fn new(inner: Box<dyn GraphicsRenderer>, origin: Option<AllowedOrigin>, log: Logger) -> Self {
        Self { inner, origin, log }
    }
}

impl GraphicsRenderer for PageOrigin {
    fn restart(&mut self, gen: u64, url: &str) {
        if let Some(allowed) = &self.origin {
            match origin_of(url) {
                Some(o) => allowed.set(o),
                None => self.log.warn(&format!(
                    "graphics restart gen={gen}: the overlay URL has no origin; the phase endpoint keeps {}",
                    allowed.get()
                )),
            }
        }
        self.inner.restart(gen, url);
    }

    fn stop(&mut self) {
        self.inner.stop();
    }

    fn health(&self) -> CefHealth {
        self.inner.health()
    }
}

/// Every port before the decorators: what differs between the modes.
pub struct Parts {
    pub input: Box<dyn InputSource>,
    pub programme: Box<dyn ProgrammeOutput>,
    pub graphics: Box<dyn GraphicsRenderer>,
    pub metrics: Box<dyn Metrics>,
    pub control: Box<dyn ControlQueue>,
    pub phase: Box<dyn PhasePublisher>,
}

impl fmt::Debug for Parts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Parts { input, programme, graphics, metrics, control, phase }")
    }
}

/// The ports the driver runs on: the same decorators and the same log sink in both modes, and in fake
/// mode `DrainingFake` around the programme.
pub fn assemble(
    drivers: Drivers,
    parts: Parts,
    log: &Logger,
    tx: &Sender<Inbound>,
    origin: Option<AllowedOrigin>,
) -> Ports {
    let programme: Box<dyn ProgrammeOutput> = match drivers {
        Drivers::Live => parts.programme,
        Drivers::Fake => Box::new(DrainingFake::new(parts.programme, tx.clone(), log.clone())),
    };
    Ports {
        input: parts.input,
        programme: Box::new(LoggedProgramme::new(programme, log.clone())),
        graphics: Box::new(LoggedGraphics::new(
            Box::new(PageOrigin::new(parts.graphics, origin, log.clone())),
            log.clone(),
        )),
        metrics: parts.metrics,
        control: parts.control,
        phase: parts.phase,
        log: log.log_sink(),
    }
}

/// The control plane for the mode, or the refusal's exit code (78, with its line: the token is never
/// in it, `PlaneConfigError` carries no value).
fn plane(env: &Env, t: &Thresholds, log: &Logger) -> Result<Box<dyn ControlPlane>, i32> {
    let http = match env.drivers {
        Drivers::Live => true,
        Drivers::Fake => env.fake.control_http,
    };
    if !http {
        return Ok(Box::new(ScriptedPlane::new(session_720p()).0));
    }
    // Each request's bound is the beat interval: a beat never outlives the next one's turn. The final
    // beat's own is C07's FINAL_BEAT_TIMEOUT_MS.
    match HttpControlPlane::new(
        &env.app_url,
        &env.session_id,
        env.job_token.clone(),
        t.heartbeat_interval_ms,
    ) {
        Ok(p) => Ok(Box::new(p)),
        Err(e) => {
            log.error(&format!("control plane refused its configuration: {e}"));
            Err(78)
        }
    }
}

/// SIGINT and SIGTERM, onto the loop's channel (`send`: a signal is never dropped).
/// The stop signals the process reads, and what each means to the domain. SIGINT (R1's stop: the
/// Final hold) and SIGTERM (the platform's: the drain) when the binary runs on its own; SIGUSR1 and
/// SIGUSR2 are the same two from the image's entrypoint, which forwards them under those names
/// because CEF takes over SIGINT and SIGTERM in this process once it starts (F-015).
pub const STOP_SIGNALS: [(i32, SignalKind); 4] = [
    (signal_hook::consts::SIGINT, SignalKind::Int),
    (signal_hook::consts::SIGTERM, SignalKind::Term),
    /*MUT*/ (signal_hook::consts::SIGINT, SignalKind::Int),
    (signal_hook::consts::SIGUSR2, SignalKind::Term),
];

fn spawn_signals(tx: Sender<Inbound>) -> std::io::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new(STOP_SIGNALS.map(|(sig, _)| sig))?;
    thread::Builder::new().name("signals".to_owned()).spawn(move || {
        for sig in signals.forever() {
            let Some(kind) = STOP_SIGNALS.iter().find(|(s, _)| *s == sig).map(|(_, k)| *k) else {
                continue;
            };
            if tx.send(Inbound::Event(Event::Signal(kind))).is_err() {
                return;
            }
        }
    })?;
    Ok(())
}

/// The phase endpoint on `127.0.0.1:RELAY_PHASE_PORT`, or `NoopPhase` and one warn line.
fn phase_endpoint(env: &Env, log: &Logger) -> (Box<dyn PhasePublisher>, Option<AllowedOrigin>) {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, RELAY_PHASE_PORT));
    match PhaseHttp::bind(addr, env.app_origin.clone()) {
        Ok(p) => {
            let origin = p.allowed_origin();
            (Box::new(p), Some(origin))
        }
        Err(e) => {
            log.warn(&format!("phase endpoint unavailable: {e}"));
            (Box::new(NoopPhase), None)
        }
    }
}

/// The mode's media ports (not yet decorated).
struct Media {
    input: Box<dyn InputSource>,
    programme: Box<dyn ProgrammeOutput>,
    graphics: Box<dyn GraphicsRenderer>,
    stats: Arc<Stats>,
}

fn live_media(t: &Thresholds, tx: &Sender<Inbound>, log: &Logger) -> Media {
    let frames = FrameStore::default();
    // First (Owed 53): its `new` installs the process's GStreamer log handler.
    let graphics = CefGraphics::new(frames.clone(), tx.clone(), cef_fps(), t.cef_rss_cap_mb)
        .with_log(log.adapter_sink());
    let programme = GstProgramme::new(frames.clone(), tx.clone()).with_log(log.adapter_sink());
    let stats = programme.stats();
    let slot = ProfileSlot::default();
    let (input_tx, input_log, input_t) = (tx.clone(), log.clone(), t.clone());
    let build: InputBuilder = Box::new(move |p: &OutputProfile| -> Box<dyn InputSource> {
        Box::new(
            GstInput::new(frames.clone(), input_tx.clone(), &input_t, p.width, p.height)
                .with_log(input_log.adapter_sink()),
        )
    });
    Media {
        input: Box::new(LazyInput::new(build, slot.clone(), log.clone())),
        programme: Box::new(ProfileTap::new(Box::new(programme), slot)),
        graphics: Box::new(graphics),
        stats,
    }
}

fn fake_media(env: &Env) -> Media {
    let calls = Log::default();
    let knob = CefKnob::default();
    if env.fake.zygote_gone {
        knob.set(CefHealth {
            unrecoverable: Some(1),
            ..CefHealth::default()
        });
    }
    Media {
        input: Box::new(FakeInput(calls.clone())),
        programme: Box::new(FakeProgramme {
            log: calls.clone(),
            fail_start: env.fake.start_fail.clone(),
        }),
        graphics: Box::new(FakeGraphics(calls, knob)),
        stats: Arc::new(Stats::default()),
    }
}

/// Builds every port for `env`'s mode and runs the loop; the process's exit code.
pub fn run(env: &Env, clock: &Arc<Clock>, log: &Logger) -> i32 {
    let t = Thresholds::default();
    let (tx, rx): (Sender<Inbound>, Receiver<Inbound>) = bounded(INBOUND_CAPACITY);
    if let Err(e) = spawn_signals(tx.clone()) {
        log.error(&format!("signal handlers not installed: {e}"));
        return 70;
    }
    let plane = match plane(env, &t, log) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (phase, origin) = phase_endpoint(env, log);
    let media = match env.drivers {
        Drivers::Live => live_media(&t, &tx, log),
        Drivers::Fake => fake_media(env),
    };
    let control = control_worker::spawn(Box::new(LoggedPlane::new(plane, log.clone())), tx.clone());
    let parts = Parts {
        input: media.input,
        programme: media.programme,
        graphics: media.graphics,
        metrics: Box::new(ProcMetrics::new(media.stats)),
        control: Box::new(control),
        phase,
    };
    let ports = assemble(env.drivers, parts, log, &tx, origin);
    log.info(&format!(
        "compositor started drivers={} deadline={}",
        match env.drivers {
            Drivers::Live => "live",
            Drivers::Fake => "fake",
        },
        crate::logged::rfc3339_ms(env.deadline_ms)
    ));
    drop(tx);
    let driver = Driver::new(t, ports, env.deadline_ms, clock.now_ms());
    app::run::run(driver, rx, &|| clock.now_ms())
}
