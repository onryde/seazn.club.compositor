//! C11b through the real driver: CEF's health, read from the graphics port, folded through the real
//! `Supervisor`, to the in-place restart's final beat and exit (owner ruling 2026-10-19 "B"). Each
//! trigger reaches the exit exactly once; a report below its threshold, one that counts back down,
//! and one in the final hold do not. Every threshold comes from `Thresholds::default()`, every text
//! is written out, and the exit code is sysexits' EX_TEMPFAIL, written here independently of
//! `ExitReason::code`.
use app::driver::Driver;
use app::ports::{ControlRequest, Inbound, Measured};
use domain::cef_health::CefHealth;
use domain::config::{Thresholds, CHROMIUM_GPU_DEATHS_ABORT};
use domain::health::{BeatState, ExitDetail, ExitInfo, HealthReport};
use domain::lifecycle::ExitReason;
use domain::supervisor::{Desired, Event};
use testkit::fakes::{ports_with_cef, session_720p, Call, CefKnob, Log};

const FAR: u64 = 100_000_000;
const SESSION_AT: u64 = 10;
/// sysexits.h EX_TEMPFAIL: non-zero, so the host restarts the process in place; not 70 or 78.
const EX_TEMPFAIL: i32 = 75;

fn th() -> Thresholds {
    Thresholds::default()
}

struct Rig {
    d: Driver,
    log: Log,
    cef: CefKnob,
    /// Every exit `tick_due` or `step` returned, with its instant.
    exits: Vec<(u64, i32)>,
}

impl Rig {
    /// Booted, the session in, its first tick run: on air with the page loading as generation 1.
    fn on_air() -> Rig {
        let log = Log::default();
        let cef = CefKnob::default();
        let mut d = Driver::new(th(), ports_with_cef(&log, Measured::default(), &cef), FAR, 0);
        assert_eq!(d.tick_due(0), None);
        assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), None);
        assert_eq!(d.tick_due(SESSION_AT), None);
        assert!(log
            .calls()
            .contains(&Call::GraphicsRestart(1, session_720p().overlay_url)));
        Rig {
            d,
            log,
            cef,
            exits: Vec::new(),
        }
    }

    /// Every tick due up to `until`, one `tick_due` per tick.
    fn tick_to(&mut self, until: u64) {
        loop {
            let at = self.d.next_tick_at();
            if at > until {
                return;
            }
            if let Some(c) = self.d.tick_due(at) {
                self.exits.push((at, c));
            }
            if !self.exits.is_empty() && self.d.next_tick_at() == at {
                return; // exited: the grid stops
            }
        }
    }

    fn step(&mut self, msg: Inbound, at: u64) {
        if let Some(c) = self.d.step(msg, at) {
            self.exits.push((at, c));
        }
    }

    fn finals(&self) -> Vec<HealthReport> {
        self.log
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Control(ControlRequest::Heartbeat(h, _)) if h.state == BeatState::Stopped => Some(h),
                _ => None,
            })
            .collect()
    }
}

fn health(unrecoverable: Option<u64>, leaked: usize, gpu: usize) -> CefHealth {
    CefHealth {
        unrecoverable,
        leaked_teardowns: leaked,
        gpu_process_deaths: gpu,
    }
}

fn health_line(h: CefHealth) -> String {
    let z = h.unrecoverable.map_or("-".to_owned(), |g| g.to_string());
    format!(
        "cef health zygote_gone={z} leaked_teardowns={} gpu_process_deaths={}",
        h.leaked_teardowns, h.gpu_process_deaths
    )
}

/// From the first tick after `h` is set, the restart: one final beat naming `detail`, then one
/// `Exit(75)`, with the input and the page stopped and nothing after the exit, however long the loop
/// keeps calling. The health is read once per tick until then, and never after.
fn assert_restarts(h: CefHealth, detail: &str) {
    let mut r = Rig::on_air();
    r.tick_to(1_000);
    assert!(r.exits.is_empty(), "healthy: {:?}", r.exits);
    let reads_before = r.cef.reads();
    assert!(reads_before > 0, "the driver never read CEF's health");
    r.cef.set(h);
    let mark = r.log.len();
    let at = r.d.next_tick_at();
    r.tick_to(at);
    assert_eq!(
        r.exits,
        vec![(at, EX_TEMPFAIL)],
        "{h:?}: the exit, on the next tick"
    );
    let after = r.log.calls_from(mark);
    let fin = r.finals();
    assert_eq!(fin.len(), 1, "{h:?}: one final beat");
    assert_eq!(
        fin[0].exit,
        Some(ExitInfo {
            reason: ExitReason::Restart,
            detail: Some(ExitDetail::new(detail)),
        }),
        "{h:?}"
    );
    let stop_input = after.iter().position(|c| *c == Call::InputStop);
    let stop_page = after.iter().position(|c| *c == Call::GraphicsStop);
    let beat = after
        .iter()
        .position(|c| matches!(c, Call::Control(ControlRequest::Heartbeat(h, _)) if h.exit.is_some()));
    assert!(
        beat.is_some() && stop_input.is_some() && stop_page.is_some() && beat < stop_input,
        "{h:?}: the final beat, then the stops: {after:?}"
    );
    assert!(after.contains(&Call::Log(health_line(h))), "{h:?}: {after:?}");
    let reads_at_exit = r.cef.reads();
    assert_eq!(
        reads_at_exit,
        reads_before + 1,
        "{h:?}: one read for the one tick"
    );
    // The second call, and every one after: nothing reaches a port, no second exit, no read.
    let len = r.log.len();
    for k in 1..=30 {
        assert_eq!(r.d.tick_due(at + k * 100), None, "{h:?}");
        assert_eq!(r.d.step(Inbound::Event(Event::Tick), at + k * 100), None);
        r.cef.set(health(Some(9), 9, 9));
    }
    assert_eq!(r.log.len(), len, "{h:?}: a port call after the exit");
    assert_eq!(r.cef.reads(), reads_at_exit, "{h:?}: a read after the exit");
}

#[test]
fn a_dead_zygote_restarts_in_place_through_the_driver() {
    assert_restarts(
        health(Some(1), 0, 0),
        "restart in place: a CEF zygote is gone (gen 1)",
    );
}

#[test]
fn leaked_teardowns_at_the_live_count_restart_through_the_driver() {
    let n = th().cef_leaks_to_restart;
    assert_restarts(
        health(None, n, 0),
        &format!("restart in place: {n} CEF teardowns leaked"),
    );
}

#[test]
fn gpu_process_deaths_short_of_the_abort_restart_through_the_driver() {
    let n = th().cef_gpu_deaths_to_restart;
    assert!(n < CHROMIUM_GPU_DEATHS_ABORT);
    assert_restarts(
        health(None, 0, n),
        &format!("restart in place: {n} CEF GPU-process deaths"),
    );
}

// Every read is handed to the domain, not only a changed one: a verdict read while booting (where
// the domain ignores it) and unchanged since still restarts on the first tick on air. Its line is
// written once, when it was first read.
#[test]
fn a_report_read_before_the_session_restarts_on_the_first_tick_on_air() {
    let log = Log::default();
    let cef = CefKnob::default();
    let h = health(Some(0), 0, 0);
    cef.set(h);
    let mut d = Driver::new(th(), ports_with_cef(&log, Measured::default(), &cef), FAR, 0);
    assert_eq!(d.tick_due(0), None, "booting ignores it");
    assert_eq!(d.tick_due(100), None, "booting ignores it");
    assert!(cef.reads() >= 2, "{}", cef.reads());
    assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT + 100), None);
    assert_eq!(d.tick_due(SESSION_AT + 100), Some(EX_TEMPFAIL));
    assert_eq!(log.count(|c| *c == Call::Log(health_line(h))), 1);
    let fin: Vec<_> = log
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::Control(ControlRequest::Heartbeat(b, _)) => b.exit,
            _ => None,
        })
        .collect();
    assert_eq!(
        fin,
        vec![ExitInfo {
            reason: ExitReason::Restart,
            detail: Some(ExitDetail::new("restart in place: a CEF zygote is gone (gen 0)")),
        }]
    );
}

// The empty case: a healthy process is read every tick and asks for nothing, and logs nothing.
#[test]
fn a_healthy_process_is_read_every_tick_and_logs_nothing() {
    let mut r = Rig::on_air();
    let reads0 = r.cef.reads();
    let ticks_before = r.d.next_tick_at();
    r.tick_to(60_000);
    let ticks = r.log.count(|c| matches!(c, Call::Push(..)));
    assert!(ticks > 1_000, "{ticks} ticks");
    assert!(r.exits.is_empty() && r.finals().is_empty());
    assert_eq!(
        r.cef.reads() - reads0,
        ticks as u64 - 1,
        "one read per tick (from {ticks_before})"
    );
    assert!(
        r.log.lines().iter().all(|l| !l.starts_with("cef health")),
        "{:?}",
        r.log.lines()
    );
}

// Below each threshold, up and back down: never a restart, and one line per change, in order.
#[test]
fn reports_below_their_thresholds_log_each_change_and_never_restart() {
    let t = th();
    let (l, g) = (t.cef_leaks_to_restart, t.cef_gpu_deaths_to_restart);
    let steps = [
        health(None, 1, 0),
        health(None, l - 1, 0),
        health(None, l - 1, 0), // unchanged: no line
        health(None, 1, 0),     // a teardown returned after all
        health(None, l - 1, g - 1),
        health(None, 0, g - 1),
    ];
    let mut r = Rig::on_air();
    let mut at = 1_000;
    for h in steps {
        r.cef.set(h);
        at += 500;
        r.tick_to(at);
    }
    r.tick_to(at + 10_000);
    assert!(r.exits.is_empty(), "{:?}", r.exits);
    let want: Vec<String> = [steps[0], steps[1], steps[3], steps[4], steps[5]]
        .into_iter()
        .map(health_line)
        .collect();
    let got: Vec<String> = r
        .log
        .lines()
        .into_iter()
        .filter(|l| l.starts_with("cef health"))
        .collect();
    assert_eq!(got, want);
}

// The final hold already ends the process (exit 0): a report there changes nothing, and the session
// ends `Ended` with exit 0, the Final card held to its end.
#[test]
fn the_final_hold_ends_its_own_way_whatever_the_health() {
    let t = th();
    for h in [
        health(Some(1), 0, 0),
        health(None, t.cef_leaks_to_restart, 0),
        health(None, 0, t.cef_gpu_deaths_to_restart),
    ] {
        let mut r = Rig::on_air();
        r.tick_to(1_000);
        r.step(Inbound::Event(Event::HeartbeatReply(Desired::Ending)), 1_001);
        r.cef.set(h);
        r.tick_to(1_001 + t.final_hold_ms - 100);
        assert!(
            r.exits.is_empty() && r.finals().is_empty(),
            "{h:?}: {:?}",
            r.exits
        );
        r.tick_to(1_001 + t.final_hold_ms + t.drain_timeout_ms + 1_000);
        assert_eq!(r.exits.iter().map(|e| e.1).collect::<Vec<_>>(), vec![0], "{h:?}");
        let fin = r.finals();
        assert_eq!(fin.len(), 1, "{h:?}");
        assert_eq!(
            fin[0].exit.as_ref().map(|x| x.reason),
            Some(ExitReason::Ended),
            "{h:?}"
        );
    }
}
