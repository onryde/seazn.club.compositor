//! C11b through the real driver: CEF's health, read from the graphics port, folded through the real
//! `Supervisor`, to the in-place restart's final beat and exit (owner ruling 2026-10-19 "B"). Each
//! trigger reaches the exit exactly once; a report below its threshold, one that counts back down,
//! and one in the final hold do not. A GPU-process death below the restart's count reloads the page
//! through the graphics port, its line first (Owed 96; owner ruling 2026-10-25 "A"). Every threshold comes from `Thresholds::default()`, every text
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

/// The reload's line (Owed 96), written from the brief's text: the generation it builds and the
/// process's GPU-process death count.
fn reload_line(gen: u64, deaths: usize) -> String {
    format!("cef reload gen={gen} reason=gpu_process_death deaths={deaths}")
}

fn reload_lines(log: &Log) -> Vec<String> {
    log.lines()
        .into_iter()
        .filter(|l| l.starts_with("cef reload"))
        .collect()
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
    // The GPU-process deaths rose once (to `g - 1`, on air at generation 1): one reload, of the next
    // generation; the same count read again on every later tick reloads nothing.
    assert_eq!(reload_lines(&r.log), vec![reload_line(2, g - 1)]);
}

// Owed 96 through the driver: each GPU-process death below the restart's count, read from the port
// on the next tick, reloads the page exactly once: the line, then `restart(next gen, the session's
// page URL)`, in that order, and nothing more however many ticks read the same count. The death AT
// the count exits 75 and reloads nothing. The number of reloads is the threshold's, from its
// declaration.
#[test]
fn each_gpu_death_below_the_count_reloads_the_page_once_through_the_driver() {
    let n = th().cef_gpu_deaths_to_restart;
    let url = session_720p().overlay_url;
    let mut r = Rig::on_air();
    r.tick_to(1_000);
    r.step(Inbound::Event(Event::CefReady { gen: 1 }), 1_001);
    r.step(Inbound::Event(Event::CefPainted { gen: 1 }), 1_001);
    let mut at = 1_001;
    let mut checked = 0;
    for deaths in 1..n {
        let gen = 1 + deaths as u64;
        let mark = r.log.len();
        r.cef.set(health(None, 0, deaths));
        at += 2_000;
        r.tick_to(at);
        let after = r.log.calls_from(mark);
        let line = after
            .iter()
            .position(|c| *c == Call::Log(reload_line(gen, deaths)));
        let restarts: Vec<usize> = after
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, Call::GraphicsRestart(..)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(restarts.len(), 1, "death {deaths}: one restart: {after:?}");
        assert_eq!(after[restarts[0]], Call::GraphicsRestart(gen, url.clone()));
        assert!(
            line.is_some_and(|l| l < restarts[0]),
            "death {deaths}: the line, then the restart: {after:?}"
        );
        assert!(
            !after.contains(&Call::GraphicsStop),
            "death {deaths}: a reload stops nothing"
        );
        // Its page gets ready, so the next death reloads a Ready page again.
        r.step(Inbound::Event(Event::CefReady { gen }), at + 1);
        r.step(Inbound::Event(Event::CefPainted { gen }), at + 1);
        checked += 1;
    }
    assert!(r.exits.is_empty(), "{:?}", r.exits);
    let want: Vec<String> = (1..n).map(|k| reload_line(1 + k as u64, k)).collect();
    assert_eq!(reload_lines(&r.log), want);
    // The count: the in-place restart, and no reload with it.
    let mark = r.log.len();
    r.cef.set(health(None, 0, n));
    let next = r.d.next_tick_at();
    r.tick_to(next);
    assert_eq!(r.exits, vec![(next, EX_TEMPFAIL)]);
    let after = r.log.calls_from(mark);
    assert!(
        !after.iter().any(|c| matches!(c, Call::GraphicsRestart(..))
            || matches!(c, Call::Log(l) if l.starts_with("cef reload"))),
        "the restart reloaded too: {after:?}"
    );
    eprintln!("deaths reloaded through the driver: {checked}");
    assert_eq!(checked, n - 1);
}

// A Ready page reloaded whose reload never signals ready: its last frame comes off air at its bound,
// `cef_hang_after_ms` after its ready (its last proof), logged as `reload` naming generation 1, with
// `hide_overlay()` alone: the reload still loading is not stopped (a hang or Failed hide stops it).
#[test]
fn a_reload_that_never_readies_hides_as_reload_and_keeps_loading() {
    let t = th();
    let mut r = Rig::on_air();
    r.tick_to(1_000);
    r.step(Inbound::Event(Event::CefReady { gen: 1 }), 1_001);
    r.step(Inbound::Event(Event::CefPainted { gen: 1 }), 1_001);
    r.cef.set(health(None, 0, 1));
    r.tick_to(1_500);
    assert_eq!(reload_lines(&r.log), vec![reload_line(2, 1)], "setup");
    let bound = 1_001 + t.cef_hang_after_ms;
    let mark = r.log.len();
    r.tick_to(bound - 1);
    assert!(
        !r.log.calls_from(mark).contains(&Call::HideOverlay),
        "hidden before its bound"
    );
    r.tick_to(bound + 100);
    let after = r.log.calls_from(mark);
    let hidden = Call::Log("overlay hidden reason=reload gen=1".to_owned());
    assert_eq!(after.iter().filter(|c| **c == hidden).count(), 1, "{after:?}");
    assert_eq!(
        after.iter().filter(|c| **c == Call::HideOverlay).count(),
        1,
        "{after:?}"
    );
    assert!(
        !after.contains(&Call::GraphicsStop),
        "the reload was stopped: {after:?}"
    );
    assert!(r.exits.is_empty(), "{:?}", r.exits);
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
        // Below the count: a reload would rebuild in the hold, where nothing is rebuilt.
        health(None, 0, 1),
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
        assert_eq!(reload_lines(&r.log), Vec::<String>::new(), "{h:?}");
    }
}

/// The blank page's reload line (Owed 99), written from the brief's text: the generation it builds and
/// the process's count of blank pages.
fn blank_line(gen: u64, blanks: usize) -> String {
    format!("cef reload gen={gen} reason=no_frames blank_pages={blanks}")
}

// Owed 99 through the driver: a Ready page that beats but never has a frame reported is reloaded on
// the first tick at or after its ready + `cef_hang_after_ms`: one line naming the cause, then
// `restart(next gen, the session's page URL)`, in that order, with nothing stopped or hidden (it was
// the first page: nothing was on air). Each blank page after it the same, up to one short of the
// count; the count's page exits 75 with the final beat naming the blank pages (the exit stops the page
// and the input, as every in-place restart does), and reloads nothing.
#[test]
fn blank_pages_reload_through_the_driver_and_the_count_restarts_in_place() {
    let t = th();
    let n = t.cef_gpu_deaths_to_restart;
    let url = session_720p().overlay_url;
    let mut r = Rig::on_air();
    r.tick_to(1_000);
    let mut gen = 1;
    let mut checked = 0;
    for k in 1..=n {
        let ready = r.d.next_tick_at();
        r.tick_to(ready);
        r.step(Inbound::Event(Event::CefReady { gen }), ready);
        let bound = ready + t.cef_hang_after_ms;
        let mark = r.log.len();
        let mut beat = ready;
        loop {
            let at = r.d.next_tick_at();
            if at >= bound {
                break;
            }
            r.tick_to(at);
            if at >= beat + t.cef_alive_every_ms {
                r.step(Inbound::Event(Event::CefAlive { gen }), at);
                beat = at;
            }
        }
        assert!(
            reload_lines(&r.log).len() == k - 1 && r.exits.is_empty(),
            "page {k}: acted before its bound"
        );
        let at = r.d.next_tick_at();
        r.tick_to(at);
        assert!(
            at >= bound && at < bound + 34,
            "the first tick at or after the bound: {at}"
        );
        let after = r.log.calls_from(mark);
        if k < n {
            assert!(
                !after.contains(&Call::GraphicsStop),
                "page {k}: stopped: {after:?}"
            );
            assert!(!after.contains(&Call::HideOverlay), "page {k}: hidden: {after:?}");
            let line = after.iter().position(|c| *c == Call::Log(blank_line(gen + 1, k)));
            let restarts: Vec<usize> = after
                .iter()
                .enumerate()
                .filter(|(_, c)| matches!(c, Call::GraphicsRestart(..)))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(restarts.len(), 1, "page {k}: one restart: {after:?}");
            assert_eq!(after[restarts[0]], Call::GraphicsRestart(gen + 1, url.clone()));
            assert!(
                line.is_some_and(|l| l < restarts[0]),
                "page {k}: the line, then the restart: {after:?}"
            );
            assert!(r.exits.is_empty(), "{:?}", r.exits);
            gen += 1;
        } else {
            assert_eq!(r.exits, vec![(at, EX_TEMPFAIL)]);
            assert!(
                !after.iter().any(|c| matches!(c, Call::GraphicsRestart(..))
                    || matches!(c, Call::Log(l) if l.starts_with("cef reload"))),
                "the restart reloaded too: {after:?}"
            );
            let fin = r.finals();
            assert_eq!(fin.len(), 1);
            let text = format!("restart in place: {n} blank CEF pages, 0 GPU-process deaths");
            assert_eq!(
                fin[0].exit,
                Some(ExitInfo {
                    reason: ExitReason::Restart,
                    detail: Some(ExitDetail::new(text)),
                })
            );
        }
        checked += 1;
    }
    let want: Vec<String> = (1..n).map(|k| blank_line(1 + k as u64, k)).collect();
    assert_eq!(reload_lines(&r.log), want);
    eprintln!("blank pages through the driver: {checked}");
    assert_eq!(checked, n);
}
