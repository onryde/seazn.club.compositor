//! `run()` (C08) on a wall clock: the real driver, the real control worker and a scripted plane. The
//! final beat is posted before `run()` returns, on a full channel and on a fatal error, and the flush
//! is bounded by `FINAL_BEAT_TIMEOUT_MS` (Owed 28). Every wait is bounded and FAILS rather than hang.
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use app::control_worker::spawn;
use app::driver::Driver;
use app::ports::{HeartbeatError, Inbound, Measured, SessionConfig, FINAL_BEAT_TIMEOUT_MS};
use app::run::run;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use domain::config::Thresholds;
use domain::health::{ExitDetail, HealthReport};
use domain::lifecycle::ExitReason;
use domain::supervisor::{Desired, Event, SignalKind};
use testkit::fakes::{ports, session_720p, Call, FakeProgramme, Log, PlaneCall, PlaneLog, ScriptedPlane};

const FAR: u64 = 100_000_000;
const SLOW: Duration = Duration::from_millis(300);

struct Ran {
    code: i32,
    at: Instant,
    /// The plane's record at the instant `run()` returned.
    plane: Vec<PlaneCall>,
}

struct Rig {
    tx: Sender<Inbound>,
    done: Receiver<Ran>,
    plane: PlaneLog,
    started: Instant,
    /// The driver's port calls, its log lines included.
    log: Log,
}

/// `run()`'s line when a final beat was not confirmed within the bound (C08 as built; Owed 35).
fn unconfirmed() -> String {
    format!("final beat not confirmed within {FINAL_BEAT_TIMEOUT_MS} ms")
}

/// Wires a driver to a real control worker over `plane`, and runs `run()` on its own thread.
fn launch(plane: ScriptedPlane, plane_log: PlaneLog, programme_fails: Option<&str>) -> Rig {
    let started = Instant::now();
    let clock = move || started.elapsed().as_millis() as u64;
    let (tx, rx) = unbounded();
    let log = Log::default();
    let mut p = ports(&log, Measured::default());
    p.control = Box::new(spawn(Box::new(plane), tx.clone()));
    if let Some(e) = programme_fails {
        p.programme = Box::new(FakeProgramme {
            log: log.clone(),
            fail_start: Some(e.to_owned()),
        });
    }
    let d = Driver::new(Thresholds::default(), p, FAR, clock());
    let (done_tx, done) = bounded(1);
    let snap = plane_log.clone();
    thread::spawn(move || {
        let code = run(d, rx, &clock);
        let at = Instant::now();
        let _ = done_tx.send(Ran {
            code,
            at,
            plane: snap.calls(),
        });
    });
    Rig {
        tx,
        done,
        plane: plane_log,
        started,
        log,
    }
}

fn finished(rig: &Rig, within: Duration) -> Ran {
    rig.done
        .recv_timeout(within)
        .unwrap_or_else(|_| panic!("run() did not return within {within:?}"))
}

fn finals(calls: &[PlaneCall]) -> Vec<(ExitReason, Option<String>)> {
    calls
        .iter()
        .filter_map(|c| match c {
            PlaneCall::Beat(h, _) => h.exit.clone(),
            _ => None,
        })
        .map(|e| (e.reason, e.detail.map(|d| d.as_str().to_owned())))
        .collect()
}

fn count(calls: &[PlaneCall], f: impl Fn(&PlaneCall) -> bool) -> usize {
    calls.iter().filter(|c| f(c)).count()
}

fn send(rig: &Rig, e: Event) {
    rig.tx.send(Inbound::Event(e)).expect("the loop is gone");
}

/// A session that beats every 50 ms, against a plane whose periodic beats take `SLOW`: the periodic
/// slot fills within a few beats.
fn fast_beating(final_reply: Result<Desired, HeartbeatError>) -> (ScriptedPlane, PlaneLog) {
    let session = SessionConfig {
        heartbeat_interval_ms: 50,
        ..session_720p()
    };
    let (plane, log) = ScriptedPlane::new(session);
    let plane = plane.reply(move |h| {
        if h.exit.is_some() {
            (Duration::ZERO, final_reply.clone())
        } else {
            (SLOW, Ok(Desired::Live))
        }
    });
    (plane, log)
}

/// Waits until a second periodic beat is in flight, then long enough for the slot behind it to fill.
fn fill_the_periodic_slot(rig: &Rig) {
    let n = |c: &[PlaneCall]| count(c, |c| matches!(c, PlaneCall::BeatStarted(h) if h.exit.is_none()));
    assert!(
        rig.plane.wait_for(Duration::from_secs(5), |c| n(c) >= 2),
        "no second periodic beat"
    );
    thread::sleep(Duration::from_millis(120));
}

#[test]
fn the_final_beat_is_posted_under_a_full_heartbeat_channel() {
    let (plane, log) = fast_beating(Ok(Desired::Live));
    let rig = launch(plane, log, None);
    fill_the_periodic_slot(&rig);
    send(
        &rig,
        Event::Fatal {
            detail: Some(ExitDetail::new("x")),
        },
    );
    let ran = finished(&rig, Duration::from_secs(5));
    assert_eq!(ran.code, ExitReason::Fatal.code());
    assert_eq!(
        finals(&ran.plane),
        vec![(ExitReason::Fatal, Some("x".to_owned()))]
    );
}

/// C06 review m2 (Owed 30), behind a full slot: the drain's beat and the fatal error's both post, in
/// that order, before `run()` returns 70.
#[test]
fn a_fatal_during_the_drain_posts_both_final_beats_behind_a_full_slot() {
    let (plane, log) = fast_beating(Ok(Desired::Live));
    let rig = launch(plane, log, None);
    fill_the_periodic_slot(&rig);
    send(&rig, Event::Signal(SignalKind::Term));
    send(
        &rig,
        Event::Fatal {
            detail: Some(ExitDetail::new("x")),
        },
    );
    let ran = finished(&rig, Duration::from_secs(5));
    assert_eq!(ran.code, ExitReason::Fatal.code());
    assert_eq!(
        finals(&ran.plane),
        vec![
            (ExitReason::Signalled, None),
            (ExitReason::Fatal, Some("x".to_owned()))
        ]
    );
    let last_final = ran
        .plane
        .iter()
        .rposition(|c| matches!(c, PlaneCall::Beat(h, _) if h.exit.is_some()))
        .expect("a final beat");
    assert!(
        !ran.plane[last_final..]
            .iter()
            .any(|c| matches!(c, PlaneCall::BeatStarted(h) if h.exit.is_none())),
        "no periodic beat after a final one"
    );
}

#[test]
fn the_final_beat_is_posted_on_a_fatal_start() {
    // The final beat takes 200 ms, so a `run()` that does not wait for it returns before it posts.
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(|_| (Duration::from_millis(200), Ok(Desired::Live)));
    let rig = launch(plane, log, Some("boom"));
    let ran = finished(&rig, Duration::from_secs(5));
    assert_eq!(ran.code, ExitReason::Fatal.code());
    assert_eq!(
        finals(&ran.plane),
        vec![(ExitReason::Fatal, Some("boom".to_owned()))]
    );
    assert!(
        !rig.log.lines().contains(&unconfirmed()),
        "a posted final beat logs nothing"
    );
}

#[test]
fn a_hung_final_beat_delays_exit_at_most_2_s() {
    let began: Arc<Mutex<Option<Instant>>> = Arc::default();
    let seen = Arc::clone(&began);
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(move |h: &HealthReport| {
        if h.exit.is_some() {
            *seen.lock().expect("lock") = Some(Instant::now());
            (Duration::from_secs(10), Ok(Desired::Live))
        } else {
            (Duration::ZERO, Ok(Desired::Live))
        }
    });
    let rig = launch(plane, log, Some("boom"));
    let ran = finished(&rig, Duration::from_secs(8));
    assert_eq!(ran.code, ExitReason::Fatal.code());
    let began = began
        .lock()
        .expect("lock")
        .expect("the final beat was never sent");
    let waited = ran.at.duration_since(began);
    let bound = Duration::from_millis(FINAL_BEAT_TIMEOUT_MS);
    assert!(
        waited + Duration::from_millis(50) >= bound,
        "returned before the bound: {waited:?}"
    );
    // 2 s + one boot tick (100 ms), and scheduling slack.
    assert!(
        waited <= bound + Duration::from_millis(400),
        "returned late: {waited:?}"
    );
    let lines = rig.log.lines();
    assert_eq!(
        lines.iter().filter(|l| **l == unconfirmed()).count(),
        1,
        "{lines:?}"
    );
    assert_eq!(
        lines.last(),
        Some(&unconfirmed()),
        "the line is the last thing run() does"
    );
}

#[test]
fn a_heartbeat_410_exits_0_with_no_beat() {
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(|_| (Duration::ZERO, Err(HeartbeatError::Gone)));
    let rig = launch(plane, log, None);
    let ran = finished(&rig, Duration::from_secs(3));
    assert_eq!(ran.code, ExitReason::SessionGone.code());
    // Nothing to flush: it returns at once (the session beats every 5 s, so one beat was sent).
    assert!(ran.at.duration_since(rig.started) < Duration::from_secs(1));
    thread::sleep(Duration::from_millis(200));
    let after = rig.plane.calls();
    assert_eq!(
        count(&after, |c| matches!(c, PlaneCall::BeatStarted(_))),
        1,
        "{after:?}"
    );
    assert!(finals(&after).is_empty(), "no final beat after a 410");
    assert!(
        !rig.log.lines().contains(&unconfirmed()),
        "nothing to flush, nothing logged"
    );
}

/// The drain's own beat draws the 410: the drain is cut short, well inside `drain_timeout_ms`.
#[test]
fn a_410_on_the_drains_beat_exits_0_before_the_drain_timeout() {
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(|h| {
        let r = if h.exit.is_some() {
            Err(HeartbeatError::Gone)
        } else {
            Ok(Desired::Live)
        };
        (Duration::ZERO, r)
    });
    let rig = launch(plane, log, None);
    let one = |c: &[PlaneCall]| count(c, |c| matches!(c, PlaneCall::Beat(..))) >= 1;
    assert!(rig.plane.wait_for(Duration::from_secs(3), one), "no first beat");
    let term = Instant::now();
    send(&rig, Event::Signal(SignalKind::Term));
    let ran = finished(&rig, Duration::from_secs(5));
    assert_eq!(ran.code, ExitReason::Signalled.code());
    let took = ran.at.duration_since(term);
    let drain_timeout = Duration::from_millis(Thresholds::default().drain_timeout_ms);
    assert!(
        took < drain_timeout / 4,
        "exited at the 410, not the drain timeout: {took:?}"
    );
    assert_eq!(finals(&ran.plane), vec![(ExitReason::Signalled, None)]);
}

/// Every sender gone: no event can ever arrive, so the loop exits as a fatal error, with its beat.
#[test]
fn a_closed_inbound_channel_is_fatal_70() {
    let log = Log::default();
    let (tx, rx) = unbounded::<Inbound>();
    drop(tx);
    let d = Driver::new(Thresholds::default(), ports(&log, Measured::default()), FAR, 0);
    let started = Instant::now();
    let clock = move || started.elapsed().as_millis() as u64;
    let (done_tx, done) = bounded(1);
    thread::spawn(move || {
        let _ = done_tx.send(run(d, rx, &clock));
    });
    let code = done
        .recv_timeout(Duration::from_secs(3))
        .expect("run() did not return");
    assert_eq!(code, ExitReason::Fatal.code());
    let fin: Vec<Option<String>> = log
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::Control(app::ports::ControlRequest::Heartbeat(h, _)) => h.exit,
            _ => None,
        })
        .map(|e| e.detail.map(|d| d.as_str().to_owned()))
        .collect();
    assert_eq!(fin, vec![Some("inbound channel closed".to_owned())]);
    assert_eq!(log.count(|c| *c == Call::Flush), 1, "run() flushes once");
}
