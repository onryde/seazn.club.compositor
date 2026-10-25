//! The composition root's pieces (C12): the log lines C14, C15 and the C17 runbook read, in both
//! driver modes and through the stderr log sink's own format; the decorators; the lazy input; the phase
//! endpoint's origin. The driver runs on a manual clock over testkit's fakes; durations come from
//! `Thresholds` (a named override where a scenario needs one), never from the code under test.
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adapters::control_http::DECLARED_PROFILES;
use adapters::metrics_proc::ProcMetrics;
use adapters::phase_http::{origin_of, AllowedOrigin, PhaseHttp};
use adapters::stats::Stats;
use app::driver::Driver;
use app::ports::{
    CefPoll, CefPss, ControlPlane, ControlRequest, GraphicsRenderer, HeartbeatError, Inbound, InputSource,
    KindPss, Measured, ProgrammeOutput,
};
use compositor::clock::Clock;
use compositor::env::Drivers;
use compositor::logged::{rfc3339_ms, LoggedPlane, LoggedProgramme, Logger};
use compositor::wiring::{assemble, cef_fps, LazyInput, PageOrigin, Parts, ProfileSlot, ProfileTap};
use crossbeam_channel::{bounded, Receiver, Sender};
use domain::cef_health::CefHealth;
use domain::config::{OutputProfile, Thresholds};
use domain::health::{BeatState, ExitInfo, HealthReport};
use domain::lifecycle::ExitReason;
use domain::supervisor::{Event, Supervisor};
use testkit::fakes::{
    session_720p, Call, CefKnob, FakeControl, FakeGraphics, FakeInput, FakeMetrics, FakePhase, FakeProgramme,
    Log, ScriptedPlane,
};

const FAR: u64 = 100_000_000;
const SESSION_AT: u64 = 10;
const TOKEN: &str = "SECRET-TOKEN-VALUE-c12";

/// A test's stderr: everything the logger wrote.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    /// Each line's message, after checking the line's form: `ts=<RFC 3339 ms Z> level=<l> msg=<m>`.
    fn messages(&self) -> Vec<String> {
        self.text()
            .lines()
            .map(|l| {
                let (ts, rest) = l.split_once(' ').unwrap_or_else(|| panic!("{l:?}"));
                let ts = ts.strip_prefix("ts=").unwrap_or_else(|| panic!("{l:?}"));
                let b = ts.as_bytes();
                assert!(
                    b.len() == 24
                        && b[4] == b'-'
                        && b[7] == b'-'
                        && b[10] == b'T'
                        && b[13] == b':'
                        && b[16] == b':'
                        && b[19] == b'.'
                        && b[23] == b'Z',
                    "ts {ts:?} is not RFC 3339 with milliseconds"
                );
                let (level, msg) = rest.split_once(' ').unwrap_or_else(|| panic!("{l:?}"));
                assert!(
                    ["level=info", "level=warn", "level=error"].contains(&level),
                    "{l:?}"
                );
                msg.strip_prefix("msg=")
                    .unwrap_or_else(|| panic!("{l:?}"))
                    .to_owned()
            })
            .collect()
    }
}

fn logger() -> (Logger, Buf) {
    let buf = Buf::default();
    let log = Logger::to_writer(Arc::new(Clock::system()), Box::new(buf.clone()));
    log.learn_token(TOKEN);
    (log, buf)
}

fn fake_parts(calls: &Log, knob: &CefKnob) -> Parts {
    Parts {
        input: Box::new(FakeInput(calls.clone())),
        programme: Box::new(FakeProgramme {
            log: calls.clone(),
            fail_start: None,
        }),
        graphics: Box::new(FakeGraphics(calls.clone(), knob.clone())),
        metrics: Box::new(FakeMetrics::default()),
        control: Box::new(FakeControl {
            log: calls.clone(),
            dropped: Arc::default(),
        }),
        phase: Box::new(FakePhase(calls.clone())),
    }
}

/// A driver over the mode's assembled ports, booted with the session at `SESSION_AT`.
struct Run {
    d: Driver,
    now: u64,
    buf: Buf,
    _rx: Receiver<Inbound>,
}

impl Run {
    fn new(mode: Drivers, t: Thresholds, knob: &CefKnob) -> Run {
        let calls = Log::default();
        let (log, buf) = logger();
        let (tx, rx): (Sender<Inbound>, Receiver<Inbound>) = bounded(64);
        let ports = assemble(mode, fake_parts(&calls, knob), &log, &tx, None);
        let mut d = Driver::new(t, ports, FAR, 0);
        assert_eq!(d.tick_due(0), None);
        assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), None);
        Run {
            d,
            now: SESSION_AT,
            buf,
            _rx: rx,
        }
    }

    /// Ticks at each `next_tick_at` before `until`, each event delivered at its instant (before a
    /// later tick, after one due at the same instant). Fails, never hangs, on an exit.
    fn drive(&mut self, until: u64, mut events: Vec<(u64, Event)>) {
        events.sort_by_key(|e| e.0);
        let mut events = events.into_iter().peekable();
        loop {
            let tick_at = self.d.next_tick_at();
            while events.peek().is_some_and(|e| e.0 < tick_at.min(until)) {
                let (at, e) = events.next().unwrap();
                self.now = at;
                assert_eq!(self.d.step(Inbound::Event(e), at), None, "an exit at {at}");
            }
            if tick_at >= until {
                return;
            }
            self.now = tick_at;
            assert_eq!(self.d.tick_due(tick_at), None, "an exit at {tick_at}");
        }
    }
}

/// Generation `gen` readies at `ready` (its latest frame written at the ready and reported, C11 and
/// Owed 99) and beats every `cef_alive_every_ms` while before `until`.
fn page(t: &Thresholds, gen: u64, ready: u64, until: u64) -> Vec<(u64, Event)> {
    let mut v = vec![
        (ready, Event::CefReady { gen }),
        (ready, Event::CefPainted { gen }),
    ];
    let mut at = ready + t.cef_alive_every_ms;
    while at < until {
        v.push((at, Event::CefAlive { gen }));
        at += t.cef_alive_every_ms;
    }
    v
}

fn last_alive(events: &[(u64, Event)]) -> u64 {
    events
        .iter()
        .filter(|e| matches!(e.1, Event::CefAlive { .. }))
        .map(|e| e.0)
        .max()
        .expect("the page beat")
}

/// `want` appears in `lines` in this order (other lines may come between). Returns how many matched.
fn assert_in_order(lines: &[String], want: &[String], what: &str) -> usize {
    let mut from = 0;
    for w in want {
        let at = lines[from..]
            .iter()
            .position(|l| l == w)
            .unwrap_or_else(|| panic!("{what}: {w:?} not found after line {from} in {lines:#?}"));
        from += at + 1;
    }
    want.len()
}

/// Owed 87: each `sink swap gen=<g> target=…` line (the decorator's) has the driver's
/// `sink swap reason=<r> gen=<g>` before it and after the previous swap, `<r>` one of the domain's
/// three words (Owed 87 retired `stall`), and the first swap's reason is `first`; there is no other
/// `sink swap` line. Returns the swaps checked.
fn assert_each_swap_has_its_reason(lines: &[String], what: &str) -> usize {
    let mut pending: Option<(String, String)> = None;
    let mut checked = 0;
    for l in lines.iter().filter(|l| l.starts_with("sink swap ")) {
        if let Some(rest) = l.strip_prefix("sink swap reason=") {
            let (reason, gen) = rest
                .split_once(" gen=")
                .unwrap_or_else(|| panic!("{what}: {l:?}"));
            assert!(
                ["first", "error", "connect_timeout"].contains(&reason),
                "{what}: {l:?}"
            );
            assert!(pending.is_none(), "{what}: two reasons before one swap: {l:?}");
            pending = Some((reason.to_owned(), gen.to_owned()));
        } else if let Some(rest) = l.strip_prefix("sink swap gen=") {
            let gen = rest.split(' ').next().unwrap_or_default();
            let (reason, of) = pending
                .take()
                .unwrap_or_else(|| panic!("{what}: swap {gen} has no reason line before it"));
            assert_eq!(of, gen, "{what}: the reason line names another generation");
            if checked == 0 {
                assert_eq!((gen, reason.as_str()), ("1", "first"), "{what}");
            }
            checked += 1;
        } else {
            panic!("{what}: an unknown sink swap line {l:?}");
        }
    }
    assert!(pending.is_none(), "{what}: a reason line with no swap after it");
    eprintln!("{what}: swaps with their reason checked {checked}");
    checked
}

fn count(lines: &[String], line: &str) -> usize {
    lines.iter().filter(|l| *l == line).count()
}

/// A hang: the Ready page stops beating. The process's health also changes once, to a count below
/// every restart threshold, so the `cef health` line is in this run once and nothing restarts.
fn hang_run(mode: Drivers) -> Vec<String> {
    let t = Thresholds::default();
    let health = CefHealth {
        leaked_teardowns: 1,
        ..CefHealth::default()
    };
    assert_eq!(
        health.restart_cause(&t),
        None,
        "test setup: below every restart threshold"
    );
    let knob = CefKnob::default();
    knob.set(health);
    let mut r = Run::new(mode, t.clone(), &knob);
    let p = page(&t, 1, 200, 6_201);
    let last = last_alive(&p);
    r.drive(last + t.cef_hang_after_ms + 500, p);
    r.buf.messages()
}

/// A crash below the count; its reload never readies; the crashed page's held frame reaches its bound.
fn crash_run(mode: Drivers) -> Vec<String> {
    let t = Thresholds::default();
    let mut r = Run::new(mode, t.clone(), &CefKnob::default());
    let mut p = page(&t, 1, 200, 20_201);
    let b = last_alive(&p);
    p.push((b + t.cef_alive_every_ms / 4, Event::CefCrashed { gen: 1 }));
    let hide = b + t.cef_hang_after_ms;
    assert!(
        hide + 500 < b + t.cef_alive_every_ms / 4 + t.cef_backoff_base_ms + t.cef_load_timeout_ms,
        "test setup: the reload is still loading at the hide"
    );
    r.drive(hide + 500, p);
    r.buf.messages()
}

/// A recycle (a short `cef_recycle_every_ms`) whose reload never readies.
fn recycle_run(mode: Drivers) -> Vec<String> {
    let t = Thresholds {
        cef_recycle_every_ms: 60_000,
        ..Thresholds::default()
    };
    let mut r = Run::new(mode, t.clone(), &CefKnob::default());
    let ready = 200;
    let p = page(&t, 1, ready, ready + t.cef_recycle_every_ms);
    let b = last_alive(&p);
    r.drive(b + t.cef_hang_after_ms + 500, p);
    r.buf.messages()
}

/// A GPU-process death below the in-place restart's count (Owed 96; owner ruling 2026-10-25 "A"):
/// the process's health counts it a quarter-beat after the Ready page's last beat, the page is
/// reloaded, and the reload never readies, so the reloaded page's held frame reaches its bound.
fn reload_run(mode: Drivers) -> Vec<String> {
    let t = Thresholds::default();
    let knob = CefKnob::default();
    let mut r = Run::new(mode, t.clone(), &knob);
    let p = page(&t, 1, 200, 6_201);
    let b = last_alive(&p);
    let death = b + t.cef_alive_every_ms / 4;
    r.drive(death, p);
    let health = CefHealth {
        gpu_process_deaths: 1,
        ..CefHealth::default()
    };
    assert_eq!(
        health.restart_cause(&t),
        None,
        "test setup: below the in-place restart"
    );
    knob.set(health);
    // The reload restarts on the first tick at or after the death, so its load timeout is no earlier
    // than `death + cef_load_timeout_ms`.
    let hide = b + t.cef_hang_after_ms;
    assert!(
        hide + 200 < death + t.cef_load_timeout_ms,
        "test setup: the reload is still loading at the hide"
    );
    r.drive(hide + 200, vec![]);
    r.buf.messages()
}

/// Every generation times out loading, to the count-th: Failed.
fn failed_run(mode: Drivers) -> Vec<String> {
    let t = Thresholds::default();
    let mut r = Run::new(mode, t.clone(), &CefKnob::default());
    let n = t.cef_fail_count as u64;
    // Each generation's load timeout plus its back-off (capped), with a second of slack each.
    let mut until = SESSION_AT;
    for gen in 1..=n {
        until +=
            t.cef_load_timeout_ms + (t.cef_backoff_base_ms << (gen - 1)).min(t.cef_backoff_cap_ms) + 1_000;
    }
    r.drive(until, vec![]);
    r.buf.messages()
}

#[test]
fn graphics_log_lines_exist_in_both_driver_modes() {
    let t = Thresholds::default();
    let n = t.cef_fail_count as u64;
    let mut matched = 0;
    for mode in [Drivers::Live, Drivers::Fake] {
        let what = format!("{mode:?}");
        let hang = hang_run(mode);
        matched += assert_in_order(
            &hang,
            &[
                "cef health zygote_gone=- leaked_teardowns=1 gpu_process_deaths=0".into(),
                "programme started".into(),
                "graphics restart gen=1".into(),
                "cef ready gen=1".into(),
                "cef alive gen=1".into(),
                "overlay hidden reason=hang gen=1".into(),
                "graphics stop".into(),
            ],
            &format!("{what} hang"),
        );
        assert_eq!(count(&hang, "overlay hidden reason=hang gen=1"), 1, "{what}");
        assert_eq!(
            hang.iter().filter(|l| l.starts_with("cef health ")).count(),
            1,
            "{what}: the health line once"
        );
        assert_eq!(
            count(
                &hang,
                "sink swap gen=1 target=rtmps://live.cloudflare.com:443/live/****"
            ),
            1,
            "{what}: the swap, its URL redacted: {hang:#?}"
        );
        // Owed 87: the driver's reason line comes before the decorator's swap line (other threads'
        // lines may land between the two in the live mode).
        matched += assert_in_order(
            &hang,
            &[
                "sink swap reason=first gen=1".into(),
                "sink swap gen=1 target=rtmps://live.cloudflare.com:443/live/****".into(),
            ],
            &format!("{what} swap"),
        );
        // Every swap has its reason line before it, and no reason line lacks its swap. The live mode
        // swaps again when its real sink fails (a made-up key at Cloudflare), so the count is the run's.
        let swaps = assert_each_swap_has_its_reason(&hang, &what);
        assert!(swaps >= 1, "{what}: no swap checked");
        let crash = crash_run(mode);
        matched += assert_in_order(
            &crash,
            &[
                "graphics restart gen=1".into(),
                "cef ready gen=1".into(),
                "cef crashed gen=1".into(),
                "graphics restart gen=2".into(),
                "overlay hidden reason=crash gen=1".into(),
            ],
            &format!("{what} crash"),
        );
        assert_eq!(count(&crash, "overlay hidden reason=crash gen=1"), 1, "{what}");
        let recycle = recycle_run(mode);
        matched += assert_in_order(
            &recycle,
            &[
                "graphics restart gen=1".into(),
                "cef ready gen=1".into(),
                "graphics restart gen=2".into(),
                "overlay hidden reason=recycle gen=1".into(),
            ],
            &format!("{what} recycle"),
        );
        assert_eq!(
            count(&recycle, "overlay hidden reason=recycle gen=1"),
            1,
            "{what}"
        );
        let reload = reload_run(mode);
        matched += assert_in_order(
            &reload,
            &[
                "graphics restart gen=1".into(),
                "cef ready gen=1".into(),
                "cef health zygote_gone=- leaked_teardowns=0 gpu_process_deaths=1".into(),
                "cef reload gen=2 reason=gpu_process_death deaths=1".into(),
                "graphics restart gen=2".into(),
                "overlay hidden reason=reload gen=1".into(),
            ],
            &format!("{what} reload"),
        );
        assert_eq!(
            count(&reload, "cef reload gen=2 reason=gpu_process_death deaths=1"),
            1,
            "{what}: one reload for one death"
        );
        assert_eq!(count(&reload, "overlay hidden reason=reload gen=1"), 1, "{what}");
        assert_eq!(
            count(&reload, "graphics stop"),
            0,
            "{what}: the reload was stopped"
        );
        let failed = failed_run(mode);
        let mut want: Vec<String> = (1..=n).map(|g| format!("graphics restart gen={g}")).collect();
        want.push(format!("overlay hidden reason=failed gen={n}"));
        want.push("graphics stop".into());
        matched += assert_in_order(&failed, &want, &format!("{what} failed"));
        assert_eq!(
            count(&failed, &format!("overlay hidden reason=failed gen={n}")),
            1
        );
        for lines in [&hang, &crash, &recycle, &reload, &failed] {
            for l in lines.iter().filter(|l| l.starts_with("overlay hidden")) {
                assert!(
                    l.contains(" reason=") && l.contains(" gen="),
                    "{what}: a bare hide line {l:?}"
                );
            }
            assert!(lines.iter().all(|l| !l.contains(TOKEN)), "{what}: the token");
        }
    }
    eprintln!("log lines matched in order: {matched}");
    assert!(matched > 0, "anti-vacuity");
}

/// Fake mode drains for real (`DrainingFake`): `drain` queues `OutputDrained`; the live mode's
/// programme reports its own (the GStreamer one, on EOS), so nothing is queued for it here.
#[test]
fn fake_mode_drain_reports_output_drained() {
    let (log, _buf) = logger();
    for (mode, want) in [(Drivers::Fake, 1), (Drivers::Live, 0)] {
        let calls = Log::default();
        let (tx, rx) = bounded(4);
        let mut ports = assemble(mode, fake_parts(&calls, &CefKnob::default()), &log, &tx, None);
        ports.programme.drain();
        let got: Vec<Inbound> = rx.try_iter().collect();
        assert_eq!(got.len(), want, "{mode:?}: {got:?}");
        if want == 1 {
            assert_eq!(got[0], Inbound::Event(Event::OutputDrained));
        }
        assert_eq!(
            calls.count(|c| *c == Call::Drain),
            1,
            "{mode:?}: the fake's drain ran"
        );
    }
}

/// `GstInput` is sized from the programme's profile, which only the session names: the lazy input
/// builds it at its first rebuild after a successful start, once, and never after `stop`.
#[test]
fn lazy_input_is_built_once_from_the_started_profile() {
    let (log, buf) = logger();
    let calls = Log::default();
    let built: Arc<Mutex<Vec<OutputProfile>>> = Arc::default();
    let slot = ProfileSlot::default();
    let (b, c) = (Arc::clone(&built), calls.clone());
    let mut input = LazyInput::new(
        Box::new(move |p: &OutputProfile| -> Box<dyn InputSource> {
            b.lock().unwrap().push(*p);
            Box::new(FakeInput(c.clone()))
        }),
        slot.clone(),
        log,
    );
    let spec = session_720p().input;
    input.rebuild(1, &spec);
    assert!(built.lock().unwrap().is_empty(), "no profile, no build");
    assert!(
        buf.messages()
            .iter()
            .any(|l| l.starts_with("input rebuild gen=1 refused")),
        "{:?}",
        buf.messages()
    );
    let mut failing = ProfileTap::new(
        Box::new(FakeProgramme {
            log: calls.clone(),
            fail_start: Some("boom".into()),
        }),
        slot.clone(),
    );
    assert!(failing.start(&OutputProfile::P1080).is_err());
    assert_eq!(slot.get(), None, "a failed start records nothing");
    let mut prog = ProfileTap::new(
        Box::new(FakeProgramme {
            log: calls.clone(),
            fail_start: None,
        }),
        slot.clone(),
    );
    prog.start(&OutputProfile::P1080).unwrap();
    input.rebuild(2, &spec);
    input.abandon(2);
    input.rebuild(3, &spec);
    assert_eq!(
        *built.lock().unwrap(),
        vec![OutputProfile::P1080],
        "built once, at 1080p"
    );
    input.stop();
    input.rebuild(4, &spec);
    assert_eq!(
        calls
            .calls()
            .into_iter()
            .filter(|c| !matches!(c, Call::Start(_)))
            .collect::<Vec<_>>(),
        vec![
            Call::InputRebuild(2, spec.clone()),
            Call::InputAbandon(2),
            Call::InputRebuild(3, spec.clone()),
            Call::InputStop,
        ],
        "every call after the build reaches it; none after stop"
    );
}

/// The phase endpoint answers the page's own origin: the overlay URL's, moved in before each load.
/// The seam: a real endpoint behind `PageOrigin`, driven by the job facts' overlay URL.
#[test]
fn the_phase_endpoint_answers_the_overlay_urls_origin() {
    let (log, buf) = logger();
    let app_origin = "https://app.example".to_owned();
    let p = PhaseHttp::bind("127.0.0.1:0".parse().unwrap(), app_origin.clone()).unwrap();
    let addr = p.local_addr();
    let calls = Log::default();
    let mut g = PageOrigin::new(
        Box::new(FakeGraphics(calls.clone(), CefKnob::default())),
        Some(p.allowed_origin()),
        log,
    );
    let status = |origin: &str| -> u16 {
        let r = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build()
            .request("OPTIONS", &format!("http://{addr}/phase"))
            .set("Origin", origin)
            .call();
        match r {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(c, _)) => c,
            Err(e) => panic!("{e}"),
        }
    };
    assert_eq!(status(&app_origin), 204, "before any page: APP_URL's origin");
    let overlay = session_720p().overlay_url;
    let page_origin = origin_of(&overlay).expect("the fixture's overlay URL has an origin");
    assert_eq!(page_origin, "https://seazn.club");
    g.restart(1, &overlay);
    assert_eq!(status(&page_origin), 204, "the page's own origin");
    assert_eq!(status(&app_origin), 403, "APP_URL's, no longer");
    g.restart(2, "not a url");
    assert_eq!(
        status(&page_origin),
        204,
        "a URL with no origin keeps the previous one"
    );
    assert!(
        buf.messages().iter().any(|l| l.contains("has no origin")),
        "{:?}",
        buf.messages()
    );
    assert_eq!(
        calls.count(|c| matches!(c, Call::GraphicsRestart(..))),
        2,
        "every restart reaches the renderer"
    );
    let _: SocketAddr = addr;
}

fn final_report(reason: ExitReason) -> HealthReport {
    let s = Supervisor::new(Thresholds::default()).decide(Event::Tick, 0).next;
    HealthReport {
        state: BeatState::Stopped,
        exit: Some(ExitInfo { reason, detail: None }),
        ..s.health(0)
    }
}

/// A final beat whose call failed is logged with its text, scrubbed of the token and of the fetched
/// session's credentials, on the final lane the worker posts it on (C08 review round 1); a periodic
/// beat's failure is not logged here (the domain counts it).
#[test]
fn a_failed_final_beat_is_logged_scrubbed_on_the_final_lane() {
    let (log, buf) = logger();
    let key = session_720p().target.stream_key;
    let text = format!("heartbeat transport: bearer {TOKEN} rejected for stream {key}");
    let failing = text.clone();
    let (plane, _) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(move |h| {
        let r = if h.exit.is_some() {
            Err(HeartbeatError::Transport(failing.clone()))
        } else {
            Err(HeartbeatError::Transport("periodic".into()))
        };
        (Duration::ZERO, r)
    });
    let mut lp = LoggedPlane::new(Box::new(plane), log.clone());
    lp.fetch_session().expect("the session");
    let mut lane = lp.final_lane();
    let periodic = Supervisor::new(Thresholds::default())
        .decide(Event::Tick, 0)
        .next
        .health(0);
    assert!(lp.heartbeat(&periodic, &Measured::default()).is_err());
    assert!(
        buf.messages().is_empty(),
        "a periodic failure is not logged: {:?}",
        buf.messages()
    );
    assert!(lane
        .heartbeat(&final_report(ExitReason::Ended), &Measured::default())
        .is_err());
    let lines = buf.messages();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("final beat failed: heartbeat transport: bearer "),
        "{lines:?}"
    );
    assert!(!lines[0].contains(TOKEN) && !lines[0].contains(&key), "{lines:?}");
    assert!(
        text.contains(TOKEN) && text.contains(&key),
        "test setup: the text named both"
    );
    // Gone is named.
    let (gone, _) = ScriptedPlane::new(session_720p());
    let gone = gone.reply(|_| (Duration::ZERO, Err(HeartbeatError::Gone)));
    let mut lane = LoggedPlane::new(Box::new(gone), log).final_lane();
    assert_eq!(
        lane.heartbeat(&final_report(ExitReason::Signalled), &Measured::default()),
        Err(HeartbeatError::Gone)
    );
    assert!(buf.messages()[1].contains("410"), "{:?}", buf.messages());
}

/// The programme's lines: `programme started`, the swap with its URL redacted, and a failed start's
/// text scrubbed of a credential that follows no marker once the session is known.
#[test]
fn logged_programme_logs_start_swap_and_a_scrubbed_failure() {
    let (log, buf) = logger();
    log.learn_session(&session_720p());
    let key = session_720p().target.stream_key;
    let calls = Log::default();
    let mut ok = LoggedProgramme::new(
        Box::new(FakeProgramme {
            log: calls.clone(),
            fail_start: None,
        }),
        log.clone(),
    );
    ok.start(&OutputProfile::P720).unwrap();
    ok.swap_sink(3, &session_720p().target);
    ok.hide_overlay();
    let failing_text = format!("rtmp2sink: stream {key} refused");
    let mut bad = LoggedProgramme::new(
        Box::new(FakeProgramme {
            log: calls.clone(),
            fail_start: Some(failing_text.clone()),
        }),
        log,
    );
    assert_eq!(
        bad.start(&OutputProfile::P720),
        Err(failing_text),
        "the domain gets it verbatim"
    );
    let lines = buf.messages();
    assert_eq!(
        lines,
        vec![
            "programme started".to_owned(),
            "sink swap gen=3 target=rtmps://live.cloudflare.com:443/live/****".to_owned(),
            "programme start failed: rtmp2sink: stream **** refused".to_owned(),
        ],
        "and nothing on hide_overlay"
    );
    assert_eq!(
        calls.count(|c| *c == Call::HideOverlay),
        1,
        "the hide is delegated"
    );
}

/// CEF is built before the session names its profile (Owed 53), so its frame cap covers every
/// declared profile.
#[test]
fn cef_fps_covers_every_declared_profile() {
    let mut checked = 0;
    for p in DECLARED_PROFILES {
        assert!(p.fps <= cef_fps(), "{p:?}");
        checked += 1;
    }
    assert!(
        DECLARED_PROFILES.iter().any(|p| p.fps == cef_fps()),
        "a declared rate, not a made-up one"
    );
    eprintln!("profiles checked: {checked}");
    assert_eq!(checked, DECLARED_PROFILES.len());
    assert!(checked > 0);
}

#[test]
fn log_lines_are_rfc3339_ms_one_per_message_and_masked() {
    // R1's example instant, written from the calendar in tests/env.rs.
    assert_eq!(rfc3339_ms(1_792_002_600_250), "2026-10-14T18:30:00.250Z");
    assert_eq!(rfc3339_ms(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(rfc3339_ms(1_792_002_600_007), "2026-10-14T18:30:00.007Z");
    let (log, buf) = logger();
    log.info(&format!("a\nfake line with {TOKEN}\r"));
    log.warn("w");
    log.error("e");
    let text = buf.text();
    assert_eq!(text.lines().count(), 3, "{text:?}");
    let msgs = buf.messages();
    assert_eq!(
        msgs,
        vec!["a\\nfake line with ****\\r".to_owned(), "w".into(), "e".into()]
    );
    assert!(text.contains(" level=warn msg=w") && text.contains(" level=error msg=e"));
}

/// `GraphicsRenderer::health` passes through every decorator: the domain reads CEF's own.
#[test]
fn health_passes_through_the_graphics_decorators() {
    let (log, _) = logger();
    let knob = CefKnob::default();
    let calls = Log::default();
    let ports = assemble(
        Drivers::Live,
        fake_parts(&calls, &knob),
        &log,
        &bounded(1).0,
        Some(AllowedOrigin::default()),
    );
    let h = CefHealth {
        unrecoverable: Some(4),
        leaked_teardowns: 2,
        gpu_process_deaths: 1,
    };
    knob.set(h);
    assert_eq!(ports.graphics.health(), h);
    assert_eq!(knob.reads(), 1);
}

/// The two sides of the stop seam agree (F-016): every signal `docker/supervise.sh` traps is
/// forwarded under a name the binary reads as the same stop, the entrypoint sources that file before
/// any other command (so its traps exist from the start: the PR #36 review's m1), the image installs
/// it where the entrypoint looks, and the entrypoint never `exec`s the compositor (an `exec` would
/// hand SIGINT and SIGTERM straight to CEF's handlers). The meanings come from R1 (SIGINT is its stop:
/// the Final hold) and the platform (SIGTERM: the drain), not the table.
#[test]
fn the_entrypoint_forwards_each_stop_signal_as_the_binary_reads_it() {
    use domain::supervisor::SignalKind;
    use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
    let read = |rel: &str| {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(rel),
        )
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
    };
    let supervise = read("docker/supervise.sh");
    let entrypoint = read("docker/entrypoint.sh");
    let dockerfile = read("docker/Dockerfile");
    let installed = "/usr/local/lib/seazn/supervise.sh";
    let number = |name: &str| match name {
        "INT" => SIGINT,
        "TERM" => SIGTERM,
        "USR1" => SIGUSR1,
        "USR2" => SIGUSR2,
        other => panic!("an unexpected signal name {other}"),
    };
    let kind_of = |sig: i32| {
        compositor::wiring::STOP_SIGNALS
            .iter()
            .find(|(s, _)| *s == sig)
            .map(|(_, k)| *k)
    };
    let mut checked = 0;
    for (trapped, want) in [("INT", SignalKind::Int), ("TERM", SignalKind::Term)] {
        let line = supervise
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("trap '_sv_forward ") && l.split_whitespace().last() == Some(trapped))
            .unwrap_or_else(|| panic!("supervise.sh traps no {trapped}"));
        let sent = line
            .strip_prefix("trap '_sv_forward ")
            .and_then(|r| r.split('\'').next())
            .unwrap_or_else(|| panic!("{line:?} forwards nothing"));
        assert_ne!(
            sent, trapped,
            "forwarding {trapped} as itself reaches CEF's handler"
        );
        assert_eq!(
            kind_of(number(trapped)),
            Some(want),
            "the binary run alone reads {trapped}"
        );
        assert_eq!(kind_of(number(sent)), Some(want), "{trapped} forwarded as {sent}");
        checked += 1;
    }
    let first = entrypoint
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("set "))
        .expect("an entrypoint command");
    assert_eq!(first, format!(". {installed}"), "the entrypoint's first command");
    assert!(
        dockerfile
            .lines()
            .any(|l| l.trim() == format!("COPY docker/supervise.sh {installed}")),
        "the image installs supervise.sh at {installed}"
    );
    assert!(
        !entrypoint
            .lines()
            .any(|l| l.trim_start().starts_with("exec ") && l.contains("compositor")),
        "the entrypoint execs the compositor"
    );
    eprintln!("stop signals checked: {checked}");
    assert_eq!(checked, 2);
}

/// Owed 86, the seam end to end: a real `Stats` (fed as the programme's encoder sink feeds it, one
/// call a video frame), the real `ProcMetrics` (what `run` hands the driver), and the real driver over
/// `assemble`'s ports and the stderr log sink. A beat's `output fps` line names the rate the beat
/// itself carries; a starved window raises the alarm on the log and a healthy one clears it. The
/// sampler reads the wall clock and the driver a manual one, so the windows are real sleeps, and each
/// expected rate is the test's frame count over the window its own clock brackets.
#[test]
fn the_real_output_fps_reaches_the_log_and_the_beat_through_the_real_sampler() {
    use std::time::Instant;
    let calls = Log::default();
    let (log, buf) = logger();
    let (tx, _rx): (Sender<Inbound>, Receiver<Inbound>) = bounded(64);
    let stats = Arc::new(Stats::default());
    let mut parts = fake_parts(&calls, &CefKnob::default());
    parts.metrics = Box::new(ProcMetrics::new(Arc::clone(&stats)));
    let ports = assemble(Drivers::Live, parts, &log, &tx, None);
    let mut d = Driver::new(Thresholds::default(), ports, FAR, 0);
    let profile = session_720p().output;
    assert_eq!(d.tick_due(0), None);
    assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), None);
    let beat_fps = |from: usize| -> Vec<Option<f64>> {
        calls
            .calls_from(from)
            .iter()
            .filter_map(|c| match c {
                Call::Control(ControlRequest::Heartbeat(_, m)) => Some(m.fps),
                _ => None,
            })
            .collect()
    };
    // Ticks to the next beat on the manual clock; the sampler's instants around it, and its `fps`.
    let next_beat = |d: &mut Driver| {
        let mark = calls.len();
        let before = Instant::now();
        loop {
            let at = d.next_tick_at();
            assert!(at < FAR, "no beat");
            assert_eq!(d.tick_due(at), None);
            let sent = beat_fps(mark);
            if !sent.is_empty() {
                assert_eq!(sent.len(), 1, "one beat");
                return (before, sent[0], Instant::now());
            }
        }
    };
    let lines = || {
        buf.messages()
            .into_iter()
            .filter(|l| l.starts_with("output fps") || l.starts_with("output starvation"))
            .collect::<Vec<_>>()
    };
    let mut pts = Duration::ZERO;
    let mut encode = |n: u64| {
        for _ in 0..n {
            stats.on_encoded_video(pts);
            pts += Duration::from_millis(33);
        }
    };
    let (_, fps, _) = next_beat(&mut d);
    assert_eq!(fps, None, "nothing encoded yet");
    encode(1);
    let (mut before, fps, mut after) = next_beat(&mut d);
    assert_eq!(fps, None, "the first total opens the window");
    assert!(lines().is_empty(), "no reading, no line: {:?}", lines());
    // A window past the minimum (OUTPUT_FPS_MIN_WINDOW_MS); a starved one (a sixth of the floor's frames), then a
    // healthy one (twice the profile's rate, so a slow runner's longer window still clears).
    let wait = Duration::from_millis(domain::config::OUTPUT_FPS_MIN_WINDOW_MS + 300);
    let floor_frames = (profile.fps_floor() * wait.as_secs_f64()) as u64;
    let healthy_frames = 2 * u64::from(profile.fps) * wait.as_millis() as u64 / 1_000;
    let mut want = Vec::new();
    let mut checked = 0;
    for (frames, alarm) in [(floor_frames / 6, "raised"), (healthy_frames, "cleared")] {
        encode(frames);
        std::thread::sleep(wait);
        let (b, fps, a) = next_beat(&mut d);
        let fps = fps.expect("a window past the minimum");
        let (lo, hi) = (
            frames as f64 / (a - before).as_secs_f64(),
            frames as f64 / (b - after).as_secs_f64(),
        );
        eprintln!("{frames} frames: the beat carried {fps:.3} fps ({lo:.3}..{hi:.3})");
        assert!((lo..=hi).contains(&fps), "{frames} frames read {fps}");
        want.push(format!(
            "output fps beat={fps:.2} floor={:.2}",
            profile.fps_floor()
        ));
        want.push(match alarm {
            "raised" => format!(
                "output starvation alarm raised fps={fps:.2} floor={:.2}",
                profile.fps_floor()
            ),
            _ => format!(
                "output starvation alarm cleared fps={fps:.2} recovered={:.2}",
                profile.fps_recovered()
            ),
        });
        assert_eq!(lines(), want, "after the {alarm} beat");
        (before, after) = (b, a);
        checked += 1;
    }
    eprintln!("output fps beats checked through the real sampler: {checked}");
    assert_eq!(checked, 2);
}

/// The soak's memory split, the seam from the sampler to the log: the real `ProcMetrics` (a CEF reader
/// handed in as `run` hands it `CefGraphics::memory`) through the real driver over `assemble`'s ports
/// and the stderr log sink. The first beat writes one `memory split` line carrying this process's
/// own `VmRSS` (a number) and the CEF poll's fields as the reader gave them; off Linux there is no
/// `/proc`, so no split and no line.
#[test]
fn the_memory_split_reaches_the_log_through_the_real_sampler() {
    let calls = Log::default();
    let (log, buf) = logger();
    let (tx, _rx): (Sender<Inbound>, Receiver<Inbound>) = bounded(64);
    let mut parts = fake_parts(&calls, &CefKnob::default());
    let poll = CefPoll {
        poll: 7,
        pss: CefPss {
            renderer: Some(KindPss {
                kb: 61_000,
                n: 1,
                listed: 1,
            }),
            ..CefPss::default()
        },
    };
    parts.metrics = Box::new(ProcMetrics::new(Arc::new(Stats::default())).with_cef(move || Some(poll)));
    let ports = assemble(Drivers::Live, parts, &log, &tx, None);
    let mut d = Driver::new(Thresholds::default(), ports, FAR, 0);
    assert_eq!(d.tick_due(0), None);
    assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), None);
    let beats = || {
        calls
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::Control(ControlRequest::Heartbeat(..))))
            .count()
    };
    while beats() == 0 {
        let at = d.next_tick_at();
        assert!(at < FAR, "no beat");
        assert_eq!(d.tick_due(at), None);
    }
    let lines: Vec<String> = buf
        .messages()
        .into_iter()
        .filter(|l| l.starts_with("memory split"))
        .collect();
    eprintln!("memory split lines after the first beat: {lines:?}");
    if cfg!(target_os = "linux") {
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        let rss = line
            .split(' ')
            .find_map(|f| f.strip_prefix("rss_kb="))
            .expect("an rss_kb field");
        assert!(rss.parse::<u64>().is_ok_and(|kb| kb > 0), "{line}");
        assert!(
            line.contains(
                " cef_poll=7 cef_renderer_kb=61000 cef_renderer_n=1 cef_renderer_listed=1 cef_gpu_process_kb=- \
                 cef_gpu_process_n=0 cef_gpu_process_listed=0 "
            ),
            "{line}"
        );
    } else {
        assert!(lines.is_empty(), "no /proc, no split: {lines:?}");
    }
}
