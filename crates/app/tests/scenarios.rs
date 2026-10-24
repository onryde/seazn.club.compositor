//! The driver (C08) folding the REAL `Supervisor` through fakes for every port, on a manual clock.
//! Every duration comes from `Thresholds::default()` (or a named override), never from the driver.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use app::driver::Driver;
use app::ports::{ControlRequest, Inbound, Measured, Metrics, SessionConfig};
use domain::config::{OutputProfile, Thresholds, OUTPUT_SINK_TIMEOUT_MS};
use domain::health::{ExitDetail, GraphicsStatus, HealthReport};
use domain::input_link::LossReason;
use domain::ladder::Rung;
use domain::lifecycle::ExitReason;
use domain::output_link::Encoded;
use domain::programme::{FrameChoice, ProgrammePhase, SlateKind};
use domain::supervisor::{Desired, Event, HeartbeatRefusal, Refusal, SignalKind};
use testkit::fakes::{ports, session_720p, Call, FakeControl, FakeGraphics, FakeProgramme, Log};

const FAR: u64 = 100_000_000;
/// The scenario's session arrives here (the brief's `boot`); its tick 0 is due at this instant.
const SESSION_AT: u64 = 10;

fn th() -> Thresholds {
    Thresholds::default()
}

fn metrics() -> Measured {
    Measured {
        cpu_pct: Some(40.0),
        ..Default::default()
    }
}

fn boot_with(log: &Log, t: Thresholds, session: SessionConfig) -> Driver {
    let mut d = Driver::new(t, ports(log, metrics()), FAR, 0);
    assert_eq!(d.tick_due(0), None);
    assert_eq!(
        log.count(|c| matches!(c, Call::Control(ControlRequest::FetchSession))),
        1
    );
    assert_eq!(d.step(Inbound::Session(session), SESSION_AT), None);
    d
}

fn boot(log: &Log) -> Driver {
    boot_with(log, th(), session_720p())
}

fn pushes(log: &Log) -> Vec<(u64, FrameChoice)> {
    log.calls()
        .into_iter()
        .filter_map(|c| {
            if let Call::Push(i, f) = c {
                Some((i, f))
            } else {
                None
            }
        })
        .collect()
}

fn beats_in(calls: &[Call]) -> Vec<HealthReport> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Control(ControlRequest::Heartbeat(h, _)) => Some(h.clone()),
            _ => None,
        })
        .collect()
}

fn ev(e: Event) -> Inbound {
    Inbound::Event(e)
}

/// `msg` every `every` ms from `from` while before `to`.
fn every(from: u64, to: u64, every: u64, msg: impl Fn() -> Inbound) -> Vec<(u64, Inbound)> {
    (0..)
        .map(|k| from + k * every)
        .take_while(|&t| t < to)
        .map(|t| (t, msg()))
        .collect()
}

/// A driver under a manual clock, with a record of when each call was made: the instant of every
/// `step`/`tick_due`, and the log's length after it, so a test can ask when a port call happened.
struct Sim {
    d: Driver,
    log: Log,
    /// (instant, log length after the call), in call order.
    marks: Vec<(u64, usize)>,
    /// The instant of every `tick_due` call (each runs exactly one tick in `drive`).
    tick_times: Vec<u64>,
    now: u64,
    exit: Option<(u64, i32)>,
}

impl Sim {
    fn new(t: Thresholds, session: SessionConfig) -> Sim {
        let log = Log::default();
        let d = boot_with(&log, t, session);
        let marks = vec![(SESSION_AT, log.len())];
        Sim {
            d,
            log,
            marks,
            tick_times: Vec::new(),
            now: SESSION_AT,
            exit: None,
        }
    }

    fn step(&mut self, msg: Inbound, at: u64) -> Option<i32> {
        let r = self.d.step(msg, at);
        self.after(at, r);
        r
    }

    fn tick(&mut self, at: u64) -> Option<i32> {
        let r = self.d.tick_due(at);
        self.tick_times.push(at);
        self.after(at, r);
        r
    }

    fn after(&mut self, at: u64, r: Option<i32>) {
        self.now = at;
        self.marks.push((at, self.log.len()));
        if let Some(c) = r {
            assert!(self.exit.is_none(), "a second exit at {at}");
            self.exit = Some((at, c));
        }
    }

    /// One tick per `tick_due` call, at each `next_tick_at()`, from now until `until` (exclusive).
    /// Each event is delivered at its instant: before any later tick, after a tick due at the same
    /// instant. Stops at an exit. Fails, never hangs, if the clock stops advancing.
    fn drive(&mut self, until: u64, mut events: Vec<(u64, Inbound)>) {
        events.sort_by_key(|e| e.0);
        assert!(
            events.iter().all(|e| e.0 >= self.now && e.0 < until),
            "an event outside [{}, {until})",
            self.now
        );
        let mut events = events.into_iter().peekable();
        loop {
            if self.exit.is_some() {
                return;
            }
            let tick_at = self.d.next_tick_at();
            assert!(
                tick_at >= self.now,
                "next_tick_at {tick_at} is behind the clock {}",
                self.now
            );
            while events.peek().is_some_and(|e| e.0 < tick_at.min(until)) {
                let (at, msg) = events.next().expect("peeked");
                if self.step(msg, at).is_some() {
                    return;
                }
            }
            if tick_at >= until {
                return;
            }
            self.tick(tick_at);
            assert!(
                self.d.next_tick_at() > tick_at || self.exit.is_some(),
                "the tick at {tick_at} did not advance the next tick ({})",
                self.d.next_tick_at()
            );
        }
    }

    /// One tick at a time until a port call made from now on matches `pred`; returns that tick's
    /// instant. Fails, never hangs, past `limit`.
    fn tick_until(&mut self, limit: u64, pred: impl Fn(&Call) -> bool) -> u64 {
        let mark = self.log.len();
        loop {
            let at = self.d.next_tick_at();
            assert!(at < limit, "nothing matched by {limit}");
            self.drive(at + 1, vec![]);
            if self.log.calls_from(mark).iter().any(&pred) {
                return at;
            }
        }
    }

    /// `drive` to `x`, then run the first tick at or after `x`, and return its instant.
    fn drive_to_tick(&mut self, x: u64, events: Vec<(u64, Inbound)>) -> u64 {
        self.drive(x, events);
        let at = self.d.next_tick_at();
        self.drive(at + 1, vec![]);
        assert_eq!(self.tick_times.last(), Some(&at));
        at
    }

    fn calls(&self) -> Vec<Call> {
        self.log.calls()
    }

    /// The instant of the driver call that made port call `i` (the first mark past it).
    fn at(&self, i: usize) -> u64 {
        let m = self.marks.partition_point(|&(_, len)| len <= i);
        self.marks
            .get(m)
            .unwrap_or_else(|| panic!("call {i} has no mark"))
            .0
    }

    /// The instant of the driver call that made each matching port call, in order.
    fn times_of(&self, pred: impl Fn(&Call) -> bool) -> Vec<u64> {
        let calls = self.calls();
        calls
            .iter()
            .enumerate()
            .filter(|(_, c)| pred(c))
            .map(|(i, _)| self.at(i))
            .collect()
    }

    /// The first tick the driver ran at or after `x`.
    fn first_tick_at_or_after(&self, x: u64) -> u64 {
        *self
            .tick_times
            .iter()
            .find(|&&t| t >= x)
            .unwrap_or_else(|| panic!("no tick at or after {x}"))
    }

    /// The first beat requested after `at` (strictly).
    fn beat_after(&self, at: u64) -> HealthReport {
        let calls = self.calls();
        let times = self.times_of(|c| matches!(c, Call::Control(ControlRequest::Heartbeat(..))));
        let beats = beats_in(&calls);
        let i = times
            .iter()
            .position(|&t| t > at)
            .unwrap_or_else(|| panic!("no beat after {at}"));
        beats[i].clone()
    }
}

fn restart(gen: u64) -> impl Fn(&Call) -> bool {
    move |c| matches!(c, Call::GraphicsRestart(g, _) if *g == gen)
}

fn line(text: String) -> impl Fn(&Call) -> bool {
    move |c| *c == Call::Log(text.clone())
}

// ---------------------------------------------------------------------------------------------
// The brief's Step 2
// ---------------------------------------------------------------------------------------------

#[test]
fn boot_starts_programme_then_first_tick_wires_input_sink_and_overlay_from_the_session() {
    let log = Log::default();
    let mut d = boot(&log);
    let cfg = session_720p();
    assert!(log.calls().contains(&Call::Start(OutputProfile::P720)));
    d.tick_due(SESSION_AT);
    let calls = log.calls();
    assert!(calls.contains(&Call::InputRebuild(1, cfg.input.clone())));
    assert!(calls.contains(&Call::Swap(1, cfg.target.clone())));
    assert!(calls.contains(&Call::GraphicsRestart(1, cfg.overlay_url.clone())));
    assert_eq!(pushes(&log), vec![(0, FrameChoice::Slate(SlateKind::Waiting))]);
}

/// The other mode (ADR 0005): a 1080p session starts the 1080p profile.
#[test]
fn a_1080p_session_starts_the_1080p_profile() {
    let log = Log::default();
    let session = SessionConfig {
        output: OutputProfile::P1080,
        ..session_720p()
    };
    let _d = boot_with(&log, th(), session);
    assert_eq!(log.count(|c| *c == Call::Start(OutputProfile::P1080)), 1);
    assert_eq!(log.count(|c| matches!(c, Call::Start(_))), 1);
}

#[test]
fn heartbeat_carries_the_metrics_sample() {
    let log = Log::default();
    let mut d = boot(&log);
    d.tick_due(SESSION_AT);
    let beat = log.calls().into_iter().find_map(|c| {
        if let Call::Control(ControlRequest::Heartbeat(_, m)) = c {
            Some(m)
        } else {
            None
        }
    });
    assert_eq!(beat.expect("no heartbeat").cpu_pct, Some(40.0));
}

/// The final beat is sampled too (`SendHeartbeat` maps the same way whatever the beat carries).
#[test]
fn the_final_beat_carries_the_metrics_sample_too() {
    let log = Log::default();
    let mut d = boot(&log);
    d.tick_due(SESSION_AT);
    let code = d.step(
        ev(Event::Fatal {
            detail: Some(ExitDetail::new("x")),
        }),
        20,
    );
    assert_eq!(code, Some(ExitReason::Fatal.code()));
    let fin = log.calls().into_iter().find_map(|c| match c {
        Call::Control(ControlRequest::Heartbeat(h, m)) if h.exit.is_some() => Some(m),
        _ => None,
    });
    assert_eq!(fin.expect("no final beat"), metrics());
}

#[test]
fn ticks_are_indexed_and_catch_up_after_a_stall() {
    let log = Log::default();
    let mut d = boot(&log);
    let p = OutputProfile::P720.frame_period_ms();
    d.tick_due(SESSION_AT);
    d.tick_due(SESSION_AT + (5.0 * p) as u64 + 1); // loop stalled 5 frames
    let idx: Vec<u64> = pushes(&log).iter().map(|x| x.0).collect();
    assert_eq!(
        idx,
        vec![0, 1, 2, 3, 4, 5],
        "every missed index is pushed once, in order"
    );
}

#[test]
fn catch_up_is_capped_at_one_second_and_counted() {
    let log = Log::default();
    let mut d = boot(&log);
    let fps = u64::from(OutputProfile::P720.fps);
    d.tick_due(SESSION_AT);
    let stall = 10_000;
    d.tick_due(SESSION_AT + stall); // 10 s stall
    let idx: Vec<u64> = pushes(&log).iter().map(|x| x.0).collect();
    assert_eq!(idx.len() as u64, 1 + fps, "at most fps catch-up ticks");
    assert_eq!(
        idx,
        (0..=fps).collect::<Vec<_>>(),
        "the oldest missed indices run first"
    );
    // Due by then: every index i with i·(1000/fps) ≤ the stall. Past the cap, the rest are skipped.
    let due = stall * fps / 1_000;
    assert_eq!(d.skipped_ticks(), due - fps, "the jump is counted exactly");
    // The index jumped past every skipped one: the next tick pushes the first index not yet due.
    let next = d.next_tick_at();
    assert!(
        next > SESSION_AT + stall,
        "the next tick ({next}) is in the future"
    );
    d.tick_due(next);
    assert_eq!(pushes(&log).last().map(|x| x.0), Some(due + 1));
    assert_eq!(d.skipped_ticks(), due - fps, "a tick on time skips nothing");
}

#[test]
fn camera_drop_repeats_then_slates_through_the_real_driver() {
    let log = Log::default();
    let mut d = boot(&log);
    let mut now = SESSION_AT;
    while now < 3_000 {
        d.step(frame(1), now);
        d.tick_due(now);
        now += 33;
    }
    let stop = now;
    while now < stop + 4_000 {
        d.tick_due(now);
        now += 33;
    }
    let p = pushes(&log);
    assert!(p.iter().any(|x| x.1 == FrameChoice::Emit));
    assert!(p.iter().any(|x| x.1 == FrameChoice::Repeat));
    assert_eq!(p.last().unwrap().1, FrameChoice::Slate(SlateKind::Reconnecting));
}

#[test]
fn programme_start_failure_is_fatal_70() {
    let log = Log::default();
    let mut p = ports(&log, Measured::default());
    p.programme = Box::new(FakeProgramme {
        log: log.clone(),
        fail_start: Some("boom".into()),
    });
    let mut d = Driver::new(th(), p, FAR, 0);
    d.tick_due(0);
    assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), Some(70));
    // Owed 27 (a): the error's own text, unredacted, rides the final beat (C07 redacts it).
    let fin: Vec<HealthReport> = beats_in(&log.calls())
        .into_iter()
        .filter(|h| h.exit.is_some())
        .collect();
    assert_eq!(fin.len(), 1, "one final beat");
    let exit = fin[0].exit.clone().expect("exit");
    assert_eq!(exit.reason, ExitReason::Fatal);
    assert_eq!(exit.detail.as_ref().map(ExitDetail::as_str), Some("boom"));
    // The exit stops both children, and nothing was wired.
    let calls = log.calls();
    assert!(calls.contains(&Call::InputStop) && calls.contains(&Call::GraphicsStop));
    assert!(!calls
        .iter()
        .any(|c| matches!(c, Call::InputRebuild(..) | Call::Push(..))));
}

// ---------------------------------------------------------------------------------------------
// Amendment 2026-10-15: phase publication and the final hold
// ---------------------------------------------------------------------------------------------

#[test]
fn phase_is_published_on_change_only_with_a_rising_seq() {
    let t = th();
    let log = Log::default();
    let mut d = boot(&log);
    let period = OutputProfile::P720.frame_period_ms().round() as u64;
    // Spec §5's walk: frames until Live, a stop past frozen_after_ms then slate_after_ms, frames past
    // resume_after_ms, an Ending reply, then the hold and the drain.
    let stop = SESSION_AT + t.resume_after_ms + 1_000;
    let back = stop + t.slate_after_ms + 1_000;
    let ending = back + t.resume_after_ms + 500;
    let back_until = ending + 500;
    // The camera connects first (C10b: frames of an input given up at its connect timeout count for
    // nothing, and `back` is past it).
    let mut events = vec![(SESSION_AT, ev(Event::InputConnected { gen: 1 }))];
    events.extend(every(SESSION_AT, stop, period, || frame(1)));
    events.extend(every(back, back_until, period, || frame(1)));
    events.push((ending, ev(Event::HeartbeatReply(Desired::Ending))));
    events.sort_by_key(|e| e.0);
    let end = ending + t.final_hold_ms + 1_000;

    let (mut ticks, mut checked, mut seen) = (0usize, 0usize, 0usize);
    let mut published = ProgrammePhase::Waiting;
    let mut check = |d: &Driver, log: &Log, seen: &mut usize, checked: &mut usize| {
        for c in log.calls_from(*seen) {
            if let Call::Phase(p, _) = c {
                published = p;
            }
        }
        *seen = log.len();
        assert_eq!(
            published,
            d.programme_phase(),
            "the change is published in the same call as the decide that made it"
        );
        *checked += 1;
    };
    let mut events = events.into_iter().peekable();
    let mut drained = false;
    loop {
        // One tick per tick_due call: each call is checked on its own.
        let at = d.next_tick_at();
        while events.peek().is_some_and(|e| e.0 < at) {
            let (when, msg) = events.next().expect("peeked");
            assert_eq!(d.step(msg, when), None);
            check(&d, &log, &mut seen, &mut checked);
        }
        if at >= end {
            break;
        }
        assert_eq!(d.tick_due(at), None);
        ticks += 1;
        check(&d, &log, &mut seen, &mut checked);
        if !drained && log.calls_from(0).contains(&Call::Drain) {
            drained = true;
            assert_eq!(d.step(ev(Event::OutputDrained), at + 1), Some(0));
            check(&d, &log, &mut seen, &mut checked);
            break;
        }
    }
    let phases: Vec<(ProgrammePhase, u64)> = log
        .calls()
        .into_iter()
        .filter_map(|c| {
            if let Call::Phase(p, s) = c {
                Some((p, s))
            } else {
                None
            }
        })
        .collect();
    use ProgrammePhase::{Final, Frozen, Live, Reconnecting};
    assert_eq!(
        phases,
        vec![(Live, 1), (Frozen, 2), (Reconnecting, 3), (Live, 4), (Final, 5)]
    );
    assert!(drained, "the walk reached the drain");
    eprintln!(
        "ticks driven: {ticks}, calls checked: {checked}, phases published: {}",
        phases.len()
    );
    assert!(ticks > 0 && checked > 0);
}

/// Drives a booted 720p session through an `Ending` reply at 20, one tick per call, until the drain.
/// Returns the instant of the drain's tick, the indices pushed in the hold, and the log length at 20.
fn hold_until_drain(d: &mut Driver, log: &Log) -> (u64, Vec<u64>, usize) {
    let t = th();
    d.tick_due(SESSION_AT);
    assert_eq!(d.step(ev(Event::HeartbeatReply(Desired::Ending)), 20), None);
    assert_eq!(log.count(|c| *c == Call::Drain), 0, "no drain at the reply");
    let mark = log.len();
    let hold_end = 20 + t.final_hold_ms;
    let mut held = Vec::new();
    for _ in 0..10_000 {
        let at = d.next_tick_at();
        let before = log.len();
        assert_eq!(d.tick_due(at), None);
        let added = log.calls_from(before);
        if added.contains(&Call::Drain) {
            assert!(
                at >= hold_end,
                "the drain at {at} is before the hold's end {hold_end}"
            );
            return (at, held, mark);
        }
        assert!(
            at < hold_end,
            "no drain on the first tick at or after the hold's end ({at})"
        );
        for c in added {
            if let Call::Push(i, f) = c {
                assert_eq!(
                    f,
                    FrameChoice::Slate(SlateKind::Final),
                    "the hold pushes the Final card"
                );
                held.push(i);
            }
        }
    }
    panic!("no drain within 10 000 ticks");
}

#[test]
fn ending_holds_final_then_drains_stops_children_and_exits_zero() {
    let log = Log::default();
    let mut d = boot(&log);
    let (drain_at, held, _) = hold_until_drain(&mut d, &log);
    assert!(!held.is_empty(), "the hold pushed frames");
    // The drain's order and its beat: Drain, then both children stopped, and a Stopped beat (Ended).
    let calls = log.calls();
    let at = calls.iter().position(|c| *c == Call::Drain).expect("drain");
    assert_eq!(calls[at + 1..at + 3], [Call::InputStop, Call::GraphicsStop]);
    let fin: Vec<HealthReport> = beats_in(&calls)
        .into_iter()
        .filter(|h| h.exit.is_some())
        .collect();
    assert_eq!(fin.len(), 1);
    assert_eq!(fin[0].exit.as_ref().map(|e| e.reason), Some(ExitReason::Ended));
    assert_eq!(d.step(ev(Event::OutputDrained), drain_at + 5), Some(0));
    assert!(log.count(|c| *c == Call::InputStop) >= 1 && log.count(|c| *c == Call::GraphicsStop) >= 1);
    let before = log.calls().len();
    assert_eq!(d.tick_due(drain_at + 1_000), None);
    assert_eq!(log.calls().len(), before, "nothing is applied after exit");
}

/// The hold never pauses the programme clock: a frozen output would trip YouTube's gap timer (F-004).
#[test]
fn holding_ticks_still_push_frames_at_the_profile_rate() {
    let t = th();
    let log = Log::default();
    let mut d = boot(&log);
    let (_, held, _) = hold_until_drain(&mut d, &log);
    let first = held[0];
    assert_eq!(
        held,
        (first..first + held.len() as u64).collect::<Vec<_>>(),
        "consecutive indices through the hold"
    );
    let per_hold = t.final_hold_ms * u64::from(OutputProfile::P720.fps) / 1_000;
    assert!(
        held.len() as u64 + 1 >= per_hold,
        "{} pushes in a {} ms hold",
        held.len(),
        t.final_hold_ms
    );
    assert_eq!(d.skipped_ticks(), 0);
}

// ---------------------------------------------------------------------------------------------
// The dispatch's sequences: SIGTERM in the hold, a 410 mid-drain, a Fatal during the drain
// ---------------------------------------------------------------------------------------------

#[test]
fn sigterm_in_the_hold_drains_at_once_without_the_rest_of_the_hold() {
    let t = th();
    let log = Log::default();
    let mut d = boot(&log);
    d.tick_due(SESSION_AT);
    assert_eq!(d.step(ev(Event::HeartbeatReply(Desired::Ending)), 20), None);
    let term_at = 20 + t.final_hold_ms / 2;
    let mut at = d.next_tick_at();
    while at < term_at {
        assert_eq!(d.tick_due(at), None);
        at = d.next_tick_at();
    }
    assert_eq!(log.count(|c| *c == Call::Drain), 0, "the hold is still on");
    let mark = log.len();
    assert_eq!(d.step(ev(Event::Signal(SignalKind::Term)), term_at), None);
    let added = log.calls_from(mark);
    assert_eq!(
        added[..3],
        [Call::Drain, Call::InputStop, Call::GraphicsStop],
        "{added:?}"
    );
    let fin = beats_in(&added);
    assert_eq!(fin.len(), 1);
    assert_eq!(
        fin[0].exit.as_ref().map(|e| e.reason),
        Some(ExitReason::Signalled)
    );
    // No Final card after the drain started: the drain ticks push nothing.
    let mark = log.len();
    for _ in 0..30 {
        let at = d.next_tick_at();
        assert_eq!(d.tick_due(at), None);
    }
    assert!(!log.calls_from(mark).iter().any(|c| matches!(c, Call::Push(..))));
    assert_eq!(d.step(ev(Event::OutputDrained), d.next_tick_at()), Some(0));
}

#[test]
fn a_410_mid_drain_exits_0_at_once_with_no_beat_after() {
    let t = th();
    let log = Log::default();
    let mut d = boot(&log);
    let (drain_at, _, _) = hold_until_drain(&mut d, &log);
    let mark = log.len();
    let code = d.step(ev(Event::HeartbeatRefused(HeartbeatRefusal::Gone)), drain_at + 5);
    assert_eq!(code, Some(ExitReason::SessionGone.code()));
    assert!(
        drain_at + 5 < drain_at + t.drain_timeout_ms,
        "before the drain's own timeout"
    );
    let added = log.calls_from(mark);
    assert!(beats_in(&added).is_empty(), "no beat after the 410: {added:?}");
    assert_eq!(added, vec![Call::InputStop, Call::GraphicsStop]);
}

/// C06 review m2 (Owed 30): a fatal error during a drain sends a SECOND `Stopped` beat, its own, with
/// its text. Web tolerates it; recorded, not changed.
#[test]
fn a_fatal_during_the_drain_sends_a_second_stopped_beat() {
    let log = Log::default();
    let mut d = boot(&log);
    let (drain_at, _, _) = hold_until_drain(&mut d, &log);
    let code = d.step(
        ev(Event::Fatal {
            detail: Some(ExitDetail::new("x")),
        }),
        drain_at + 5,
    );
    assert_eq!(code, Some(ExitReason::Fatal.code()));
    let fin: Vec<(ExitReason, Option<String>)> = beats_in(&log.calls())
        .into_iter()
        .filter_map(|h| h.exit)
        .map(|e| (e.reason, e.detail.map(|x| x.as_str().to_owned())))
        .collect();
    assert_eq!(
        fin,
        vec![
            (ExitReason::Ended, None),
            (ExitReason::Fatal, Some("x".to_owned()))
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// Amendment 2026-10-18: the hang beat, the hides, the ladder
// ---------------------------------------------------------------------------------------------

/// Gen `gen`'s page comes ready at `ready`, beats every `cef_alive_every_ms` while before `until`.
fn page(gen: u64, ready: u64, until: u64) -> Vec<(u64, Inbound)> {
    let t = th();
    let mut v = vec![(ready, ev(Event::CefReady { gen }))];
    v.extend(every(
        ready + t.cef_alive_every_ms,
        until,
        t.cef_alive_every_ms,
        || FakeGraphics::alive(gen),
    ));
    v
}

fn last_beat(events: &[(u64, Inbound)]) -> u64 {
    events
        .iter()
        .filter(|e| matches!(e.1, Inbound::Event(Event::CefAlive { .. })))
        .map(|e| e.0)
        .max()
        .expect("the page beat")
}

/// A hang hide: the line, then `GraphicsStop`, then `HideOverlay`, in one tick's call, at the first
/// tick at or after the page's last beat + `cef_hang_after_ms`. Returns that tick. The stop comes
/// first (PR #32 review I2, Owed 57): a sample being written as the slot is cleared lands after the
/// clear unless the stop, which waits that write out, came before it.
fn assert_hang_hide(sim: &Sim, gen: u64, last_alive: u64) -> u64 {
    let t = th();
    let want = sim.first_tick_at_or_after(last_alive + t.cef_hang_after_ms);
    let calls = sim.calls();
    let text = format!("overlay hidden reason=hang gen={gen}");
    let l = calls
        .iter()
        .position(|c| *c == Call::Log(text.clone()))
        .unwrap_or_else(|| panic!("no line {text:?}"));
    assert_eq!(
        calls[l + 1],
        Call::GraphicsStop,
        "the line, then the stop (gen {gen})"
    );
    assert_eq!(
        calls[l + 2],
        Call::HideOverlay,
        "the stop, then the hide (gen {gen})"
    );
    assert!(
        !(0..l).any(|i| sim.at(i) == want && matches!(calls[i], Call::Push(..))),
        "the hide comes before its tick's frame (gen {gen})"
    );
    assert!(
        (l..calls.len()).any(|i| sim.at(i) == want && matches!(calls[i], Call::Push(..))),
        "its tick pushed a frame (gen {gen})"
    );
    assert_eq!(
        sim.at(l),
        want,
        "gen {gen}'s hide at the first tick ≥ its last beat + the bound"
    );
    assert_eq!(sim.at(l + 2), want, "all three in the one tick (gen {gen})");
    want
}

#[test]
fn hung_page_is_restarted_through_the_port() {
    let t = th();
    let n = t.cef_fail_count as u64;
    assert!(n >= 2, "the walk needs a hang below the count");
    let mut sim = Sim::new(t.clone(), session_720p());
    // Generation 1 is built on the first tick.
    sim.drive(SESSION_AT + 1, vec![]);
    assert_eq!(sim.times_of(restart(1)), vec![SESSION_AT]);
    let mut hides_at = Vec::new();
    let mut ready_at = 1_000;
    for gen in 1..=n {
        // Ready, beats for 29 s (gen 1: to t = 30 s), then silence.
        let p = page(gen, ready_at, ready_at + 29_000);
        let last = last_beat(&p);
        let hide = sim.drive_to_tick(last + t.cef_hang_after_ms, p);
        assert_eq!(assert_hang_hide(&sim, gen, last), hide);
        hides_at.push(hide);
        if gen < n {
            // Below the count: one restart, the back-off (base · 2^(failures − 1)) after the hide.
            let wait = (t.cef_backoff_base_ms << (gen - 1)).min(t.cef_backoff_cap_ms);
            let again = sim.drive_to_tick(hide + wait, vec![]);
            assert_eq!(sim.times_of(restart(gen + 1)), vec![again], "gen {}", gen + 1);
            ready_at = again + 100;
        }
    }
    let first_hide = hides_at[0];
    let last_hide = *hides_at.last().expect("hides");
    assert!(
        last_hide - first_hide < t.cef_fail_window_ms,
        "every hang inside one failure window"
    );
    // The count-th hang entered Failed: no restart until retry_at, then one.
    let retry_at = last_hide + t.cef_fail_window_ms;
    sim.drive(retry_at, vec![]);
    assert!(
        sim.times_of(restart(n + 1)).is_empty(),
        "no restart before retry_at"
    );
    assert_eq!(sim.beat_after(last_hide).graphics, GraphicsStatus::Failed);
    let retried = sim.drive_to_tick(retry_at, vec![]);
    assert_eq!(sim.times_of(restart(n + 1)), vec![retried]);
    let restarts = sim.times_of(|c| matches!(c, Call::GraphicsRestart(..)));
    assert_eq!(
        restarts.len() as u64,
        n + 1,
        "one restart per generation, none extra"
    );
    assert_eq!(
        sim.times_of(|c| *c == Call::HideOverlay),
        hides_at,
        "one hide per hang, none extra"
    );
    eprintln!(
        "hang hides checked: {}, restarts checked: {}",
        hides_at.len(),
        restarts.len()
    );
    assert!(!hides_at.is_empty());
}

/// Owed 17: `cef_fail_count` load timeouts (`?noready=1`, as the domain sees it). Returns the tick of
/// the count-th, which entered Failed.
fn load_timeouts_to_failed(sim: &mut Sim) -> u64 {
    let t = th();
    let n = t.cef_fail_count as u64;
    assert!(n >= 1);
    sim.drive(SESSION_AT + 1, vec![]);
    for gen in 1..n {
        let built = sim.times_of(restart(gen))[0];
        let fail = sim.drive_to_tick(built + t.cef_load_timeout_ms, vec![]);
        let wait = (t.cef_backoff_base_ms << (gen - 1)).min(t.cef_backoff_cap_ms);
        let again = sim.drive_to_tick(fail + wait, vec![]);
        assert_eq!(sim.times_of(restart(gen + 1)), vec![again], "gen {}", gen + 1);
        assert!(
            sim.times_of(|c| *c == Call::HideOverlay).is_empty(),
            "no hide below the count"
        );
    }
    let built = sim.times_of(restart(n))[0];
    sim.drive_to_tick(built + t.cef_load_timeout_ms, vec![])
}

#[test]
fn hide_stops_the_hidden_page_and_the_retry_reaches_air() {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    let failed_at = load_timeouts_to_failed(&mut sim);
    let n = t.cef_fail_count as u64;
    let mut checked = 0;
    // The negative half: the line, GraphicsStop, then HideOverlay, in that order, in that tick.
    assert_eq!(sim.times_of(|c| *c == Call::HideOverlay), vec![failed_at]);
    assert_eq!(sim.times_of(|c| *c == Call::GraphicsStop), vec![failed_at]);
    let calls = sim.calls();
    let l = calls
        .iter()
        .position(|c| *c == Call::Log(format!("overlay hidden reason=failed gen={n}")))
        .expect("the failed hide's line");
    assert_eq!(calls[l + 1..l + 3], [Call::GraphicsStop, Call::HideOverlay]);
    assert_eq!(sim.at(l), failed_at);
    checked += 3;
    // A ready from the stopped generation, after the hide, changes nothing.
    let stale_at = sim.now + 100;
    let retry_at = failed_at + t.cef_fail_window_ms;
    sim.drive(retry_at, vec![(stale_at, ev(Event::CefReady { gen: n }))]);
    assert!(
        sim.times_of(restart(n + 1)).is_empty(),
        "no restart before retry_at"
    );
    let calls = sim.calls();
    let beat_times = sim.times_of(|c| matches!(c, Call::Control(ControlRequest::Heartbeat(..))));
    let failed_beats: Vec<HealthReport> = beats_in(&calls)
        .into_iter()
        .zip(beat_times)
        .filter(|(_, at)| *at > stale_at)
        .map(|(h, _)| h)
        .collect();
    assert!(!failed_beats.is_empty());
    assert!(failed_beats.iter().all(|h| h.graphics == GraphicsStatus::Failed));
    checked += failed_beats.len();
    // The positive pair: the retry at retry_at, its ready reads ready, and no hide follows while it beats.
    let retried = sim.drive_to_tick(retry_at, vec![]);
    assert_eq!(sim.times_of(restart(n + 1)), vec![retried]);
    let p = page(n + 1, retried + 100, retried + 60_000);
    sim.drive(retried + 60_000, p);
    assert_eq!(sim.beat_after(retried + 200).graphics, GraphicsStatus::Ready);
    assert_eq!(
        sim.times_of(|c| *c == Call::HideOverlay).len(),
        1,
        "no hide after the retry"
    );
    assert_eq!(sim.times_of(|c| *c == Call::GraphicsStop).len(), 1);
    checked += 3;
    eprintln!("calls checked: {checked}");
    assert!(checked > 0);
}

/// A crash below the count, then the reload that follows it.
/// Returns (the sim, the crashed page's last beat, the reload's restart tick).
fn crashed_then_reloading() -> (Sim, u64, u64) {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    sim.drive(SESSION_AT + 1, vec![]);
    let mut events = page(1, 200, 20_201);
    let b = last_beat(&events);
    let c = b + t.cef_alive_every_ms / 4;
    events.push((c, ev(Event::CefCrashed { gen: 1 })));
    let reload = sim.drive_to_tick(c + t.cef_backoff_base_ms, events);
    assert_eq!(sim.times_of(restart(2)), vec![reload]);
    assert!(
        sim.times_of(|c| *c == Call::HideOverlay).is_empty(),
        "no hide at a crash below the count"
    );
    (sim, b, reload)
}

#[test]
fn a_crash_hide_leaves_the_reload_loading_1_crash() {
    let t = th();
    let (mut sim, b, reload) = crashed_then_reloading();
    // Generation 2 sends nothing; the crashed page's frame comes off at its last beat + the bound.
    let hide = sim.drive_to_tick(b + t.cef_hang_after_ms, vec![]);
    assert!(
        hide < reload + t.cef_load_timeout_ms,
        "the reload is still loading at the hide"
    );
    assert_eq!(sim.times_of(|c| *c == Call::HideOverlay), vec![hide]);
    assert_eq!(
        sim.times_of(line("overlay hidden reason=crash gen=1".into())),
        vec![hide]
    );
    assert!(
        sim.times_of(|c| *c == Call::GraphicsStop).is_empty(),
        "the reload is left loading"
    );
    // Its ready 1 ms after the hide, inside its load timeout: ready, and no further hide while it beats.
    let p = page(2, hide + 1, hide + 40_000);
    sim.drive(hide + 40_000, p);
    assert_eq!(sim.beat_after(hide + 1).graphics, GraphicsStatus::Ready);
    assert_eq!(sim.times_of(|c| *c == Call::HideOverlay).len(), 1);
    assert!(sim.times_of(|c| *c == Call::GraphicsStop).is_empty());
    let scanned = sim.calls().len();
    eprintln!("calls scanned: {scanned}");
    assert!(scanned > 0);
}

/// Review round 1, item 3: at the count, the crash itself hides, inside the `decide` it is handed to.
/// Its line comes first: `cef crashed gen=<n>`, then the hide's line, the stop (a failed page is
/// stopped) and `HideOverlay`, in that order and at the crash's instant. Each generation crashes as it
/// loads, 1 ms after the tick that started it; the next starts on its back-off's tick.
#[test]
fn a_crash_at_the_count_logs_the_crash_before_its_hide() {
    let t = th();
    let count = t.cef_fail_count as u64;
    assert!(count >= 2, "the count is reached after a reload");
    let mut sim = Sim::new(t.clone(), session_720p());
    sim.drive(SESSION_AT + 1, vec![]);
    let mut started = sim.times_of(restart(1))[0];
    let mut crashes = 0;
    for gen in 1..count {
        sim.drive(started + 2, vec![(started + 1, ev(Event::CefCrashed { gen }))]);
        crashes += 1;
        started = sim.tick_until(started + t.cef_fail_window_ms, restart(gen + 1));
    }
    let at = started + 1;
    sim.drive(at + 1, vec![(at, ev(Event::CefCrashed { gen: count }))]);
    crashes += 1;
    let calls = sim.calls();
    let crash = Call::Log(format!("cef crashed gen={count}"));
    let i = calls
        .iter()
        .position(|c| *c == crash)
        .expect("the counting crash's line");
    assert_eq!(
        calls[i..i + 4],
        [
            crash,
            Call::Log(format!("overlay hidden reason=failed gen={count}")),
            Call::GraphicsStop,
            Call::HideOverlay,
        ],
        "the crash's line, then its hide"
    );
    assert_eq!(sim.at(i), at);
    assert_eq!(sim.at(i + 3), at);
    assert_eq!(
        sim.times_of(|c| *c == Call::HideOverlay),
        vec![at],
        "no hide below the count"
    );
    eprintln!("crashes driven: {crashes}");
    assert_eq!(crashes, count);
}

/// The positive pair: generation 2 then hangs, and that hide does stop it.
#[test]
fn a_crash_hide_leaves_the_reload_loading_2_a_hang_after_it_still_stops() {
    let t = th();
    let (mut sim, b, _) = crashed_then_reloading();
    let hide = sim.drive_to_tick(b + t.cef_hang_after_ms, vec![]);
    let p = page(2, hide + 1, hide + 10_000);
    let last = last_beat(&p);
    let hang = sim.drive_to_tick(last + t.cef_hang_after_ms, p);
    assert_eq!(assert_hang_hide(&sim, 2, last), hang);
    assert_eq!(
        sim.times_of(|c| *c == Call::GraphicsStop),
        vec![hang],
        "the one stop is the hang's"
    );
}

#[test]
fn a_crash_hide_leaves_the_reload_loading_3_recycle() {
    let t = Thresholds {
        cef_recycle_every_ms: 60_000,
        ..th()
    };
    let mut sim = Sim::new(t.clone(), session_720p());
    sim.drive(SESSION_AT + 1, vec![]);
    let ready = 200;
    let p = page(1, ready, ready + t.cef_recycle_every_ms);
    let b = last_beat(&p);
    let hide = sim.drive_to_tick(b + t.cef_hang_after_ms, p);
    let recycle = sim.first_tick_at_or_after(ready + t.cef_recycle_every_ms);
    assert_eq!(sim.times_of(restart(2)), vec![recycle], "the recycle's own tick");
    assert!(
        hide < recycle + t.cef_load_timeout_ms,
        "the reload is still loading at the hide"
    );
    assert_eq!(sim.times_of(|c| *c == Call::HideOverlay), vec![hide]);
    assert_eq!(
        sim.times_of(line("overlay hidden reason=recycle gen=1".into())),
        vec![hide]
    );
    assert!(
        sim.times_of(|c| *c == Call::GraphicsStop).is_empty(),
        "the reload is left loading"
    );
    let scanned = sim.calls().len();
    eprintln!("calls scanned: {scanned}");
    assert!(scanned > 0);
}

/// A hang while the camera is away: the overlay's watch does not depend on the programme phase.
#[test]
fn a_hang_in_reconnecting_still_hides_and_restarts() {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    let period = OutputProfile::P720.frame_period_ms().round() as u64;
    let stop = SESSION_AT + t.resume_after_ms + 1_000;
    let mut events = every(SESSION_AT, stop, period, || frame(1));
    let p = page(1, 200, stop + t.slate_after_ms + 2_000);
    let last = last_beat(&p);
    events.extend(p);
    let hide = sim.drive_to_tick(last + t.cef_hang_after_ms, events);
    assert_eq!(assert_hang_hide(&sim, 1, last), hide);
    assert!(hide > stop + t.slate_after_ms, "the camera is away at the hide");
    assert_eq!(sim.d.programme_phase(), ProgrammePhase::Reconnecting);
    let again = sim.drive_to_tick(hide + t.cef_backoff_base_ms, vec![]);
    assert_eq!(sim.times_of(restart(2)), vec![again]);
    let around: Vec<FrameChoice> = pushes(&sim.log)
        .into_iter()
        .zip(sim.times_of(|c| matches!(c, Call::Push(..))))
        .filter(|(_, at)| *at + 500 >= hide && *at <= hide + 500)
        .map(|(p, _)| p.1)
        .collect();
    assert!(!around.is_empty());
    assert!(around
        .iter()
        .all(|f| *f == FrameChoice::Slate(SlateKind::Reconnecting)));
}

fn overruns(from: u64, to: u64) -> Vec<(u64, Inbound)> {
    every(from, to, 100, || ev(Event::EncoderOverrun))
}

#[test]
fn sustained_overruns_step_down_once_through_the_port() {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    sim.drive(61_000, overruns(1_000, 61_000));
    assert_eq!(
        sim.calls()
            .into_iter()
            .filter(|c| matches!(c, Call::StepDown(_)))
            .collect::<Vec<_>>(),
        vec![Call::StepDown(Rung::OverlayHalfRate)]
    );
    // The step at the `ladder_overruns_to_step`-th overrun inside `ladder_window_ms`.
    let nth = 1_000 + 100 * (t.ladder_overruns_to_step as u64 - 1);
    assert_eq!(sim.times_of(|c| matches!(c, Call::StepDown(_))), vec![nth]);
    assert_eq!(sim.beat_after(nth).rung, Rung::OverlayHalfRate);
}

#[test]
fn final_with_the_ladder_stepped_holds_the_card_at_the_profile_rate() {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    sim.drive(10_000, overruns(1_000, 10_000));
    assert_eq!(sim.times_of(|c| matches!(c, Call::StepDown(_))).len(), 1);
    let ending = 10_000;
    let mut events = vec![(ending, ev(Event::HeartbeatReply(Desired::Ending)))];
    events.extend(overruns(ending + 50, ending + t.final_hold_ms));
    let mark = sim.log.len();
    sim.drive(ending + t.final_hold_ms + 200, events);
    let added = sim.log.calls_from(mark);
    assert_eq!(
        sim.times_of(|c| matches!(c, Call::StepDown(_))).len(),
        1,
        "no step in the hold"
    );
    let held: Vec<(u64, FrameChoice)> = added
        .iter()
        .filter_map(|c| {
            if let Call::Push(i, f) = c {
                Some((*i, *f))
            } else {
                None
            }
        })
        .collect();
    assert!(!held.is_empty());
    assert!(held.iter().all(|p| p.1 == FrameChoice::Slate(SlateKind::Final)));
    assert!(
        held.windows(2).all(|w| w[1].0 == w[0].0 + 1),
        "consecutive through the hold"
    );
    let hold_beats: Vec<HealthReport> = beats_in(&added);
    assert!(!hold_beats.is_empty());
    assert!(hold_beats.iter().all(|h| h.rung == Rung::OverlayHalfRate));
    assert!(added.contains(&Call::Drain));
    assert_eq!(sim.step(ev(Event::OutputDrained), sim.now + 1), Some(0));
}

// ---------------------------------------------------------------------------------------------
// The driver's own rules: the tick grid, the session, refusals, the exit, the log lines
// ---------------------------------------------------------------------------------------------

/// Before bootstrap there is no profile: ticks every 100 ms from `boot_ms`, and a failed fetch is
/// retried on the first such tick at or after its back-off.
#[test]
fn before_the_session_ticks_every_100_ms_from_boot() {
    let t = th();
    for boot_ms in [0, 50] {
        let log = Log::default();
        let mut d = Driver::new(t.clone(), ports(&log, metrics()), FAR, boot_ms);
        assert_eq!(d.next_tick_at(), boot_ms);
        assert_eq!(d.tick_due(boot_ms), None);
        let fetches = || log.count(|c| matches!(c, Call::Control(ControlRequest::FetchSession)));
        assert_eq!(fetches(), 1);
        assert_eq!(d.step(ev(Event::BootstrapFailed), boot_ms + 5), None);
        let retry = boot_ms + 5 + t.bootstrap_backoff_base_ms;
        let mut at = d.next_tick_at();
        let mut grid = Vec::new();
        while fetches() == 1 {
            assert!(at <= retry + 100, "no retry by {at}");
            grid.push(at);
            d.tick_due(at);
            at = d.next_tick_at();
        }
        let expect: Vec<u64> = (1..=grid.len() as u64).map(|i| boot_ms + 100 * i).collect();
        assert_eq!(grid, expect, "the boot grid is 100 ms from boot_ms");
        assert!(*grid.last().expect("a retry") >= retry && grid[grid.len() - 2] < retry);
    }
}

/// The period and the 1 s cap come from the session's profile, not from 30 fps: swept over rates the
/// two built-in profiles never use, so a hard-coded 33.3 ms or 30 goes red.
#[test]
fn the_tick_period_and_the_catch_up_cap_come_from_the_session_profile() {
    let mut checked = 0;
    for base in [OutputProfile::P720, OutputProfile::P1080] {
        for fps in [25u32, 30, 50, 60] {
            let log = Log::default();
            let session = SessionConfig {
                output: OutputProfile { fps, ..base },
                ..session_720p()
            };
            let mut d = boot_with(&log, th(), session);
            let fps = u64::from(fps);
            // Tick i is due at SESSION_AT + i·1000/fps exactly, run on the first ms at or after it.
            for i in 0..3 * fps {
                let due = SESSION_AT + (i * 1_000).div_ceil(fps);
                assert_eq!(d.next_tick_at(), due, "{base:?} at {fps} fps, tick {i}");
                d.tick_due(due);
                checked += 1;
            }
            let now = d.next_tick_at() + 5_000;
            let before = pushes(&log).len() as u64;
            d.tick_due(now);
            assert_eq!(
                pushes(&log).len() as u64 - before,
                fps,
                "the cap is one second of ticks"
            );
        }
    }
    eprintln!("grid ticks checked: {checked}");
    assert!(checked > 0);
}

#[test]
fn a_second_session_changes_nothing() {
    let log = Log::default();
    let mut d = boot(&log);
    d.tick_due(SESSION_AT);
    let at = d.next_tick_at();
    let mark = log.len();
    let other = SessionConfig {
        output: OutputProfile::P1080,
        heartbeat_interval_ms: 1,
        ..session_720p()
    };
    assert_eq!(d.step(Inbound::Session(other), at - 1), None);
    assert!(log.calls_from(mark).is_empty(), "no call for a second session");
    assert_eq!(d.next_tick_at(), at, "the grid is not restarted");
    // The first session still wires: an input rebuild reads its spec.
    d.tick_due(at);
    assert_eq!(pushes(&log).last().map(|p| p.0), Some(1));
}

/// The driver owns the tick: an inbound `Tick` would run a domain tick off the index grid. It is
/// sent where a domain tick WOULD act (a beat due and the camera long gone), so a tick let through
/// shows up as calls.
#[test]
fn an_inbound_tick_is_refused() {
    let t = th();
    let log = Log::default();
    let mut d = boot(&log);
    d.tick_due(SESSION_AT);
    for (at, f) in every(20, 1_500, 33, || frame(1)) {
        d.step(f, at);
        d.tick_due(at);
    }
    assert_eq!(d.programme_phase(), ProgrammePhase::Live);
    let mark = log.len();
    let next = d.next_tick_at();
    let late = SESSION_AT + t.heartbeat_interval_ms + t.slate_after_ms;
    assert_eq!(d.step(ev(Event::Tick), late), None);
    assert!(log.calls_from(mark).is_empty(), "an inbound tick decides nothing");
    assert_eq!(d.next_tick_at(), next);
    assert_eq!(d.programme_phase(), ProgrammePhase::Live);
    // The witness that the instant would act: the grid's own tick there beats and slates.
    d.tick_due(late);
    let added = log.calls_from(mark);
    assert!(!beats_in(&added).is_empty());
    assert!(
        added.contains(&Call::Phase(ProgrammePhase::Reconnecting, 2)),
        "{added:?}"
    );
}

/// Only a `Session` bootstraps: an inbound `BootstrapOk` would run the domain with no session to wire
/// from. The driver refuses its first wiring effect as a fatal error, never a panic.
#[test]
fn an_inbound_bootstrap_without_a_session_is_fatal_70() {
    let log = Log::default();
    let mut d = Driver::new(th(), ports(&log, metrics()), FAR, 0);
    d.tick_due(0);
    let code = d.step(
        ev(Event::BootstrapOk {
            heartbeat_interval_ms: 5_000,
            deadline_at: FAR,
        }),
        5,
    );
    assert_eq!(code, Some(ExitReason::Fatal.code()));
    let calls = log.calls();
    assert!(
        !calls.iter().any(|c| matches!(c, Call::Start(_))),
        "nothing started"
    );
    let fin: Vec<HealthReport> = beats_in(&calls)
        .into_iter()
        .filter(|h| h.exit.is_some())
        .collect();
    assert_eq!(fin.len(), 1);
    let detail = fin[0]
        .exit
        .as_ref()
        .and_then(|e| e.detail.clone())
        .expect("a detail");
    assert!(
        detail.as_str().contains("before a session"),
        "{}",
        detail.as_str()
    );
}

/// A session the driver cannot pace (0 fps) is refused like a session that is not ours (78).
#[test]
fn a_session_with_no_frame_rate_is_refused_78() {
    let log = Log::default();
    let mut d = Driver::new(th(), ports(&log, metrics()), FAR, 0);
    d.tick_due(0);
    let session = SessionConfig {
        output: OutputProfile {
            fps: 0,
            ..OutputProfile::P720
        },
        ..session_720p()
    };
    assert_eq!(
        d.step(Inbound::Session(session), 5),
        Some(ExitReason::Refused.code())
    );
    assert!(!log.calls().iter().any(|c| matches!(c, Call::Start(_))));
}

/// The session's interval paces the beats, and the deadline the driver was given is the domain's.
#[test]
fn the_session_interval_and_the_deadline_reach_the_supervisor() {
    let t = th();
    let log = Log::default();
    let interval = 2_000;
    let deadline = 9_000;
    let mut d = Driver::new(t.clone(), ports(&log, metrics()), deadline, 0);
    d.tick_due(0);
    let session = SessionConfig {
        heartbeat_interval_ms: interval,
        ..session_720p()
    };
    assert_eq!(d.step(Inbound::Session(session), SESSION_AT), None);
    let mut at = d.next_tick_at();
    let mut beat_ticks = Vec::new();
    while at < deadline {
        let mark = log.len();
        d.tick_due(at);
        if !beats_in(&log.calls_from(mark)).is_empty() {
            beat_ticks.push(at);
        }
        at = d.next_tick_at();
    }
    assert!(beat_ticks.len() >= 4);
    assert!(beat_ticks
        .windows(2)
        .all(|w| w[1] - w[0] >= interval && w[1] - w[0] < interval + 34));
    // The deadline tick starts the hold: the next push is the Final card.
    d.tick_due(at);
    assert_eq!(d.programme_phase(), ProgrammePhase::Final);
    assert_eq!(
        pushes(&log).last().map(|p| p.1),
        Some(FrameChoice::Slate(SlateKind::Final))
    );
}

/// A deadline already past at bootstrap exits 0 and never starts the programme (plan review focus 4).
#[test]
fn a_deadline_already_past_exits_0_and_starts_nothing() {
    let log = Log::default();
    let mut d = Driver::new(th(), ports(&log, metrics()), 5, 0);
    d.tick_due(0);
    assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), Some(0));
    assert!(!log.calls().iter().any(|c| matches!(c, Call::Start(_))));
}

/// After `Exit` the driver is inert: no inbound message or tick reaches a port, a log line included.
#[test]
fn nothing_is_applied_after_exit_whatever_arrives() {
    let log = Log::default();
    let mut d = boot(&log);
    d.tick_due(SESSION_AT);
    assert_eq!(d.step(ev(Event::Signal(SignalKind::Term)), 20), None);
    assert_eq!(d.step(ev(Event::OutputDrained), 30), Some(0));
    let mark = log.len();
    let late = [
        ev(Event::CefReady { gen: 1 }),
        ev(Event::CefAlive { gen: 1 }),
        ev(Event::CefCrashed { gen: 1 }),
        frame(1),
        ev(Event::EncoderOverrun),
        ev(Event::HeartbeatReply(Desired::Ending)),
        ev(Event::HeartbeatRefused(HeartbeatRefusal::Gone)),
        ev(Event::Fatal {
            detail: Some(ExitDetail::new("late")),
        }),
        ev(Event::BootstrapOk {
            heartbeat_interval_ms: 1,
            deadline_at: FAR,
        }),
        ev(Event::BootstrapRefused(Refusal::Gone)),
        Inbound::Session(session_720p()),
    ];
    let n = late.len();
    for (i, m) in late.into_iter().enumerate() {
        assert_eq!(d.step(m, 40 + i as u64), None);
    }
    for at in [1_000, 5_000, 60_000] {
        assert_eq!(d.tick_due(at), None);
    }
    assert!(log.calls_from(mark).is_empty(), "{:?}", log.calls_from(mark));
    eprintln!("late messages checked: {n}");
}

/// C12's amendment 2026-10-18: the driver logs each graphics event it hands to `decide`, a stale one
/// included, by name and generation.
#[test]
fn graphics_events_are_logged_by_name_as_they_are_decided() {
    let log = Log::default();
    let mut d = boot(&log);
    d.tick_due(SESSION_AT);
    let script = [
        (Event::CefReady { gen: 1 }, "cef ready gen=1"),
        (Event::CefAlive { gen: 1 }, "cef alive gen=1"),
        (Event::CefAlive { gen: 7 }, "cef alive gen=7"),
        (Event::CefCrashed { gen: 1 }, "cef crashed gen=1"),
        (Event::CefReady { gen: 9 }, "cef ready gen=9"),
    ];
    let mut checked = 0;
    for (i, (e, want)) in script.into_iter().enumerate() {
        let mark = log.len();
        d.step(ev(e), 20 + i as u64);
        assert_eq!(log.calls_from(mark).first(), Some(&Call::Log(want.to_owned())));
        checked += 1;
    }
    // Other events log nothing.
    let mark = log.len();
    d.step(frame(1), 30);
    d.step(ev(Event::ScoreStale(true)), 31);
    assert!(log.calls_from(mark).iter().all(|c| !matches!(c, Call::Log(_))));
    eprintln!("graphics lines checked: {checked}");
    assert_eq!(checked, 5);
}

// ---------------------------------------------------------------------------------------------
// Review round 1, item 4: the skipped ticks and the dropped beats reach the log
// ---------------------------------------------------------------------------------------------

/// The rate limit the lines are held to (the driver's own constant is private).
const TALLY_EVERY_MS: u64 = 60_000;

fn lines_starting(log: &Log, prefix: &str) -> Vec<String> {
    log.lines()
        .into_iter()
        .filter(|l| l.starts_with(prefix))
        .collect()
}

/// A `tick_due` at `now` with tick `next` the next to run, by the grid's rule (tick `i` is due at
/// `SESSION_AT + ⌈i·1000 / fps⌉`, a call runs at most `fps` of the due ticks and skips the rest):
/// the next index after the call, and how many were skipped.
fn stalled(next: u64, now: u64, fps: u64) -> (u64, u64) {
    let last_due = (now - SESSION_AT) * fps / 1_000;
    let due = last_due + 1 - next;
    (last_due + 1, due.saturating_sub(fps))
}

/// Skips reach the log as `ticks skipped total=<n>`: one line at the first, then at most one a minute
/// (a skip 30 s on is held; one exactly a minute on is logged, with the total so far), and the rest
/// at the exit, before it.
#[test]
fn skipped_ticks_are_logged_at_most_once_a_minute_and_at_the_exit() {
    let log = Log::default();
    let mut d = boot(&log);
    let fps = u64::from(OutputProfile::P720.fps);
    assert_eq!(d.tick_due(SESSION_AT), None);
    let (mut next, mut total) = (1, 0);
    let mut stall = |d: &mut Driver, now: u64| {
        let (n, k) = stalled(next, now, fps);
        assert!(k > 0, "the stall at {now} skips");
        next = n;
        total += k;
        assert_eq!(d.tick_due(now), None);
        assert_eq!(d.skipped_ticks(), total, "the grid's rule");
        total
    };
    let first = SESSION_AT + 10_000;
    let t1 = stall(&mut d, first);
    assert_eq!(
        lines_starting(&log, "ticks skipped"),
        vec![format!("ticks skipped total={t1}")]
    );
    stall(&mut d, first + TALLY_EVERY_MS / 2);
    assert_eq!(
        lines_starting(&log, "ticks skipped").len(),
        1,
        "held: 30 s after the line"
    );
    let t3 = stall(&mut d, first + TALLY_EVERY_MS);
    assert_eq!(
        lines_starting(&log, "ticks skipped"),
        vec![
            format!("ticks skipped total={t1}"),
            format!("ticks skipped total={t3}")
        ],
        "a minute after the line: logged, the held skips included"
    );
    let t4 = stall(&mut d, first + TALLY_EVERY_MS + 20_000);
    assert_eq!(lines_starting(&log, "ticks skipped").len(), 2, "held again");
    let at = first + TALLY_EVERY_MS + 20_001;
    assert_eq!(d.step(ev(Event::Signal(SignalKind::Term)), at), None);
    assert_eq!(
        lines_starting(&log, "ticks skipped").len(),
        2,
        "the drain is not the exit"
    );
    assert_eq!(d.step(ev(Event::OutputDrained), at + 1), Some(0));
    assert_eq!(
        lines_starting(&log, "ticks skipped"),
        vec![
            format!("ticks skipped total={t1}"),
            format!("ticks skipped total={t3}"),
            format!("ticks skipped total={t4}")
        ],
        "the held rest, at the exit"
    );
    assert!(t1 < t3 && t3 < t4);
}

/// Dropped periodic beats (the queue's count) reach the log as `heartbeats dropped total=<n>`, read
/// after each beat is requested: one line at the first rise, then at most one a minute, and the rest
/// at the exit. The session beats every 5 s, on the tick grid, so a beat falls exactly a minute after
/// the line.
#[test]
fn dropped_heartbeats_are_logged_at_most_once_a_minute_and_at_the_exit() {
    let log = Log::default();
    let dropped = Arc::new(AtomicU64::new(0));
    let mut p = ports(&log, metrics());
    p.control = Box::new(FakeControl {
        log: log.clone(),
        dropped: Arc::clone(&dropped),
    });
    let mut d = Driver::new(th(), p, FAR, 0);
    assert_eq!(d.tick_due(0), None);
    assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), None);
    let is_beat = |c: &Call| matches!(c, Call::Control(ControlRequest::Heartbeat(..)));
    // One tick at a time to the next beat; its instant.
    let next_beat = |d: &mut Driver| {
        let mark = log.len();
        loop {
            let at = d.next_tick_at();
            assert!(at < FAR, "no beat");
            assert_eq!(d.tick_due(at), None);
            if log.calls_from(mark).iter().any(is_beat) {
                return at;
            }
        }
    };
    let b0 = next_beat(&mut d);
    let dropped_lines = || lines_starting(&log, "heartbeats dropped");
    assert!(dropped_lines().is_empty(), "nothing dropped, nothing logged");
    dropped.store(1, Ordering::SeqCst);
    let line1 = next_beat(&mut d);
    assert!(line1 > b0);
    assert_eq!(dropped_lines(), vec!["heartbeats dropped total=1".to_owned()]);
    dropped.store(2, Ordering::SeqCst);
    let mut beats = 0;
    let mut at = line1;
    while at < line1 + TALLY_EVERY_MS {
        at = next_beat(&mut d);
        beats += 1;
        if at < line1 + TALLY_EVERY_MS {
            assert_eq!(dropped_lines().len(), 1, "held at the beat at {at}");
        }
    }
    assert_eq!(
        at,
        line1 + TALLY_EVERY_MS,
        "a beat falls exactly a minute after the line"
    );
    assert_eq!(beats, TALLY_EVERY_MS / session_720p().heartbeat_interval_ms);
    assert_eq!(
        dropped_lines(),
        vec![
            "heartbeats dropped total=1".to_owned(),
            "heartbeats dropped total=2".to_owned()
        ]
    );
    dropped.store(5, Ordering::SeqCst);
    assert_eq!(d.step(ev(Event::Signal(SignalKind::Term)), at + 1), None);
    assert_eq!(
        dropped_lines().len(),
        2,
        "the drain's beat is held: inside the minute"
    );
    assert_eq!(d.step(ev(Event::OutputDrained), at + 2), Some(0));
    assert_eq!(
        dropped_lines(),
        vec![
            "heartbeats dropped total=1".to_owned(),
            "heartbeats dropped total=2".to_owned(),
            "heartbeats dropped total=5".to_owned()
        ],
        "the held rest, at the exit"
    );
    eprintln!("beats inside the minute: {beats}");
}

/// A `Metrics` whose `camera_frames_replaced` the test sets between beats (the rest stays
/// `metrics()`'s).
struct ScriptedReplaced(Arc<Mutex<Option<u64>>>);

impl Metrics for ScriptedReplaced {
    fn sample(&mut self) -> Measured {
        Measured {
            camera_frames_replaced: *self.0.lock().unwrap_or_else(PoisonError::into_inner),
            ..metrics()
        }
    }
}

/// Owed 81 (i): every beat whose metrics sample carries `camera_frames_replaced` writes one line,
/// `camera frames replaced beat=<n> total=<n>`, `beat` being the rise since the last beat's sample,
/// the final beat included. A sample without it (nothing measured yet) writes none.
/// Every expected line is the script's own difference. A total lower than the last (the adapter's
/// count only grows, so only a fault reaches it) reads `beat=0`, and the next beat counts from it.
#[test]
fn camera_frames_replaced_is_logged_once_a_beat_with_the_beats_rise() {
    let log = Log::default();
    let total = Arc::new(Mutex::new(None));
    let set = |n: Option<u64>| *total.lock().unwrap_or_else(PoisonError::into_inner) = n;
    let mut p = ports(&log, metrics());
    p.metrics = Box::new(ScriptedReplaced(Arc::clone(&total)));
    let mut d = Driver::new(th(), p, FAR, 0);
    assert_eq!(d.tick_due(0), None);
    assert_eq!(d.step(Inbound::Session(session_720p()), SESSION_AT), None);
    let is_beat = |c: &Call| matches!(c, Call::Control(ControlRequest::Heartbeat(..)));
    let next_beat = |d: &mut Driver| {
        let mark = log.len();
        loop {
            let at = d.next_tick_at();
            assert!(at < FAR, "no beat");
            assert_eq!(d.tick_due(at), None);
            if log.calls_from(mark).iter().any(is_beat) {
                return at;
            }
        }
    };
    let lines = || lines_starting(&log, "camera frames replaced");
    let mut at = next_beat(&mut d);
    assert!(lines().is_empty(), "not measured: no line");
    let script: [(u64, &str); 6] = [
        (0, "beat=0 total=0"),
        (6, "beat=6 total=6"),
        (6, "beat=0 total=6"),
        (13, "beat=7 total=13"),
        (4, "beat=0 total=4"),
        (9, "beat=5 total=9"),
    ];
    let mut want = Vec::new();
    for (n, line) in script {
        set(Some(n));
        at = next_beat(&mut d);
        want.push(format!("camera frames replaced {line}"));
        assert_eq!(lines(), want, "the beat at {at}");
    }
    // A SIGTERM drains at once with its one final beat; the exit after it sends none.
    set(Some(12));
    let beats_before = log.count(is_beat);
    assert_eq!(d.step(ev(Event::Signal(SignalKind::Term)), at + 1), None);
    assert_eq!(d.step(ev(Event::OutputDrained), at + 2), Some(0));
    assert_eq!(log.count(is_beat) - beats_before, 1, "the drain's final beat");
    want.push("camera frames replaced beat=3 total=12".to_owned());
    assert_eq!(lines(), want, "the final beat's line");
    eprintln!("camera frames replaced lines checked: {}", want.len());
    assert_eq!(want.len(), script.len() + 1);
}

/// A `Metrics` whose `fps` (the real output rate, Owed 86) the test sets between beats.
struct ScriptedFps(Arc<Mutex<Option<f64>>>);

impl Metrics for ScriptedFps {
    fn sample(&mut self) -> Measured {
        Measured {
            fps: *self.0.lock().unwrap_or_else(PoisonError::into_inner),
            ..metrics()
        }
    }
}

/// F-024 (Owed 86): what C14 case 6's starved windows carried, the dispatch run and the PR run.
const F024_STARVED_FPS: [f64; 2] = [11.5, 24.7];

/// The lines a beat that reads `fps` writes, against the session profile's own levels (never a
/// number typed here): the reading and the floor, then the alarm's change, if any.
type AlarmLine = fn(f64, &OutputProfile) -> String;

fn fps_line(fps: f64, p: &OutputProfile) -> String {
    format!("output fps beat={fps:.2} floor={:.2}", p.fps_floor())
}
fn raised_line(fps: f64, p: &OutputProfile) -> String {
    format!(
        "output starvation alarm raised fps={fps:.2} floor={:.2}",
        p.fps_floor()
    )
}
fn cleared_line(fps: f64, p: &OutputProfile) -> String {
    format!(
        "output starvation alarm cleared fps={fps:.2} recovered={:.2}",
        p.fps_recovered()
    )
}

/// Owed 86: every beat whose metrics sample carries the real output rate writes `output fps
/// beat=<fps> floor=<floor>`; a beat below the floor raises the starvation alarm once
/// (`output starvation alarm raised …`), a starved beat after it repeats nothing, a beat between the
/// floor and the recovery level keeps it raised, and the recovery level clears it (`… cleared …`);
/// then a second starvation raises it again, and the drain's final beat is judged too. A beat with
/// no reading, or one that is not a measurement, writes nothing. The beat that is sent carries the
/// same rate the line names (the heartbeat's `fps`, never the profile's). The ladder is untouched.
#[test]
fn the_real_output_fps_is_logged_each_beat_and_starvation_raises_then_clears_the_alarm() {
    let log = Log::default();
    let fps = Arc::new(Mutex::new(None));
    let set = |n: Option<f64>| *fps.lock().unwrap_or_else(PoisonError::into_inner) = n;
    let mut p = ports(&log, metrics());
    p.metrics = Box::new(ScriptedFps(Arc::clone(&fps)));
    let mut d = Driver::new(th(), p, FAR, 0);
    assert_eq!(d.tick_due(0), None);
    let session = session_720p();
    let profile = session.output;
    assert_eq!(d.step(Inbound::Session(session), SESSION_AT), None);
    let is_beat = |c: &Call| matches!(c, Call::Control(ControlRequest::Heartbeat(..)));
    let sent = |calls: &[Call]| -> Vec<Option<f64>> {
        calls
            .iter()
            .filter_map(|c| match c {
                Call::Control(ControlRequest::Heartbeat(_, m)) => Some(m.fps),
                _ => None,
            })
            .collect()
    };
    let next_beat = |d: &mut Driver| {
        let mark = log.len();
        loop {
            let at = d.next_tick_at();
            assert!(at < FAR, "no beat");
            assert_eq!(d.tick_due(at), None);
            let calls = log.calls_from(mark);
            if calls.iter().any(is_beat) {
                return (at, sent(&calls));
            }
        }
    };
    let lines = || {
        let mut l = lines_starting(&log, "output fps");
        l.extend(lines_starting(&log, "output starvation"));
        l.sort();
        l
    };
    let (band, recovered) = (
        (profile.fps_floor() + profile.fps_recovered()) / 2.0,
        profile.fps_recovered(),
    );
    let healthy = f64::from(profile.fps);
    let (mut at, first) = next_beat(&mut d);
    assert_eq!(first, vec![None]);
    assert!(lines().is_empty(), "not measured: no line");
    // (the reading, the alarm's change on that beat)
    let script: [(Option<f64>, Option<AlarmLine>); 10] = [
        (Some(healthy), None),
        (Some(F024_STARVED_FPS[0]), Some(raised_line)),
        (Some(F024_STARVED_FPS[1]), None),
        (Some(profile.fps_floor()), None),
        (Some(band), None),
        (Some(recovered), Some(cleared_line)),
        (Some(band), None),
        (None, None),
        (Some(f64::NAN), None),
        (Some(0.0), Some(raised_line)),
    ];
    let mut want = Vec::new();
    let mut raises = 0;
    for (reading, change) in script {
        set(reading);
        let (beat_at, carried) = next_beat(&mut d);
        at = beat_at;
        if let Some(r) = reading.filter(|r| r.is_finite()) {
            want.push(fps_line(r, &profile));
            if let Some(line) = change {
                want.push(line(r, &profile));
                raises += usize::from(r < profile.fps_floor());
            }
        }
        want.sort();
        assert_eq!(lines(), want, "the beat at {at}, reading {reading:?}");
        assert_eq!(carried.len(), 1, "one beat");
        assert_eq!(
            carried[0].map(f64::to_bits),
            reading.map(f64::to_bits),
            "the beat carries the reading itself"
        );
    }
    assert_eq!(raises, 2, "two starvations");
    // The drain's final beat is judged as well: a recovery clears the alarm on it.
    set(Some(healthy));
    let beats_before = log.count(is_beat);
    assert_eq!(d.step(ev(Event::Signal(SignalKind::Term)), at + 1), None);
    assert_eq!(d.step(ev(Event::OutputDrained), at + 2), Some(0));
    assert_eq!(log.count(is_beat) - beats_before, 1, "the drain's final beat");
    want.push(fps_line(healthy, &profile));
    want.push(cleared_line(healthy, &profile));
    want.sort();
    assert_eq!(lines(), want, "the final beat's lines");
    assert_eq!(
        log.count(|c| matches!(c, Call::StepDown(_))),
        0,
        "no ladder change"
    );
    eprintln!("output fps lines checked: {}", want.len());
    assert_eq!(
        want.len(),
        8 + 3 + 2,
        "8 readings, 3 alarm changes, the final beat's 2"
    );
}

/// Owed 86: a beat with a reading but no session (a refused bootstrap's final beat) has no profile
/// to judge it against, so it writes no line and raises nothing; the beat still carries the rate.
#[test]
fn a_beat_before_any_session_writes_no_output_fps_line() {
    let log = Log::default();
    let mut p = ports(&log, metrics());
    p.metrics = Box::new(ScriptedFps(Arc::new(Mutex::new(Some(F024_STARVED_FPS[0])))));
    let mut d = Driver::new(th(), p, FAR, 0);
    assert_eq!(d.tick_due(0), None);
    let code = d.step(ev(Event::BootstrapRefused(Refusal::Gone)), 5);
    assert!(code.is_some(), "a refused bootstrap exits");
    let beats: Vec<Option<f64>> = log
        .calls_from(0)
        .iter()
        .filter_map(|c| match c {
            Call::Control(ControlRequest::Heartbeat(_, m)) => Some(m.fps),
            _ => None,
        })
        .collect();
    assert_eq!(
        beats,
        vec![Some(F024_STARVED_FPS[0])],
        "the final beat, as sampled"
    );
    assert!(lines_starting(&log, "output fps").is_empty());
    assert!(lines_starting(&log, "output starvation").is_empty());
}

/// The empty case: a run that skipped no tick and dropped no beat logs neither line, at the exit
/// included.
#[test]
fn nothing_skipped_or_dropped_logs_no_tally() {
    let log = Log::default();
    let mut d = boot(&log);
    let mut ticks = 0;
    while d.next_tick_at() < SESSION_AT + 12_000 {
        assert_eq!(d.tick_due(d.next_tick_at()), None);
        ticks += 1;
    }
    let at = d.next_tick_at();
    assert_eq!(d.step(ev(Event::Signal(SignalKind::Term)), at), None);
    assert_eq!(d.step(ev(Event::OutputDrained), at + 1), Some(0));
    assert_eq!(d.skipped_ticks(), 0);
    let beats = log.count(|c| matches!(c, Call::Control(ControlRequest::Heartbeat(..))));
    assert!(lines_starting(&log, "ticks skipped").is_empty());
    assert!(lines_starting(&log, "heartbeats dropped").is_empty());
    eprintln!("ticks driven: {ticks}, beats requested: {beats}");
    assert!(ticks > 0 && beats > 1);
}

// ---------------------------------------------------------------------------------------------
// C10b (Owed 50): the domain gives the input up through the driver, `InputSource::abandon(gen)`
// ---------------------------------------------------------------------------------------------

/// A frame of input generation `gen`.
fn frame(gen: u64) -> Inbound {
    ev(Event::InputFrame { gen })
}

fn abandons(log: &Log) -> Vec<u64> {
    log.calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::InputAbandon(g) => Some(g),
            _ => None,
        })
        .collect()
}

fn rebuilt(gen: u64) -> impl Fn(&Call) -> bool {
    move |c| matches!(c, Call::InputRebuild(g, _) if *g == gen)
}

fn abandoned(gen: u64) -> impl Fn(&Call) -> bool {
    move |c| *c == Call::InputAbandon(gen)
}

/// The position of the first call matching `pred`.
fn position(log: &Log, pred: impl Fn(&Call) -> bool) -> usize {
    log.calls().iter().position(pred).expect("the call was made")
}

/// The stall rule, through the real driver: gen 1 connects, frames stop, and the driver calls
/// `abandon(1)` once, on the first tick at or after the last frame plus `input_stall_after_ms`, and
/// before it rebuilds gen 2; nothing else is abandoned before gen 2's own connect timeout.
#[test]
fn the_stall_rule_abandons_the_input_once_through_the_driver() {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    let step = OutputProfile::P720.frame_period_ms().round() as u64;
    let stop = SESSION_AT + 3_000;
    let mut events = vec![(SESSION_AT + 1, ev(Event::InputConnected { gen: 1 }))];
    events.extend(every(SESSION_AT + 1, stop, step, || frame(1)));
    let last = events.last().expect("frames").0;
    sim.drive(stop, events);
    let r2 = sim.tick_until(stop + t.input_stall_after_ms + t.input_backoff_cap_ms, rebuilt(2));
    let next_timeout = r2 + t.input_connect_timeout_ms;
    sim.drive(next_timeout - 1, vec![]);
    let at = sim.times_of(abandoned(1));
    eprintln!("stall: last frame {last}, abandon(1) at {at:?}, rebuild(2) at {r2}");
    assert_eq!(
        at,
        vec![sim.first_tick_at_or_after(last + t.input_stall_after_ms)]
    );
    assert_eq!(
        abandons(&sim.log),
        vec![1],
        "once, and nothing else before gen 2's timeout"
    );
    assert!(position(&sim.log, abandoned(1)) < position(&sim.log, rebuilt(2)));
}

/// The connect timeout, through the real driver: no generation ever connects, and each is
/// abandoned once, on the first tick at or after its build plus `input_connect_timeout_ms`, before
/// the next is built.
#[test]
fn a_connect_timeout_abandons_each_generation_once_through_the_driver() {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    let mut checked = 0;
    let mut built = SESSION_AT;
    for gen in 1..=3u64 {
        let next = sim.tick_until(
            built + t.input_connect_timeout_ms + t.input_backoff_cap_ms,
            rebuilt(gen + 1),
        );
        let at = sim.times_of(abandoned(gen));
        eprintln!(
            "connect timeout: gen {gen} built {built}, abandoned at {at:?}, gen {} built {next}",
            gen + 1
        );
        assert_eq!(
            at,
            vec![sim.first_tick_at_or_after(built + t.input_connect_timeout_ms)],
            "gen {gen}"
        );
        assert!(position(&sim.log, abandoned(gen)) < position(&sim.log, rebuilt(gen + 1)));
        built = next;
        checked += 1;
    }
    assert_eq!(abandons(&sim.log), vec![1, 2, 3], "each once");
    assert_eq!(checked, 3);
}

/// A loss the adapter reported owes no abandon: EOS, an error, or a stall it saw, while running.
/// The next generation is built, connects and runs, and the driver never calls `abandon`.
#[test]
fn a_reported_loss_is_never_abandoned_through_the_driver() {
    let t = th();
    let step = OutputProfile::P720.frame_period_ms().round() as u64;
    let mut checked = 0;
    for reason in [LossReason::Eos, LossReason::Error, LossReason::Stalled] {
        let mut sim = Sim::new(t.clone(), session_720p());
        let lose = SESSION_AT + 1_000;
        let mut events = vec![(SESSION_AT + 1, ev(Event::InputConnected { gen: 1 }))];
        events.extend(every(SESSION_AT + 1, lose, step, || frame(1)));
        events.push((lose, ev(Event::InputLost { gen: 1, reason })));
        sim.drive(lose + 1, events);
        let r2 = sim.tick_until(lose + t.input_backoff_cap_ms, rebuilt(2));
        let mut events = vec![(r2 + 1, ev(Event::InputConnected { gen: 2 }))];
        let end = r2 + 3 * t.input_stall_after_ms;
        events.extend(every(r2 + 1, end, step, || frame(2)));
        sim.drive(end, events);
        assert_eq!(abandons(&sim.log), Vec::<u64>::new(), "{reason:?}");
        assert_eq!(
            sim.log.count(|c| matches!(c, Call::InputRebuild(..))),
            2,
            "{reason:?}: gen 2 ran"
        );
        checked += 1;
    }
    eprintln!("reported losses checked: {checked}");
    assert_eq!(checked, 3);
}

/// The hold ticks no link: an input whose connect would time out on the tick after the end request
/// is not abandoned through the hold. Its pair without the request is, on that tick.
#[test]
fn the_hold_abandons_nothing_through_the_driver() {
    let t = th();
    let mut checked = 0;
    for ending in [false, true] {
        let mut sim = Sim::new(t.clone(), session_720p());
        let due = SESSION_AT + t.input_connect_timeout_ms;
        sim.drive(due - 1, vec![]);
        if ending {
            let at = sim.d.next_tick_at().min(due - 1);
            sim.drive(at + 1, vec![(at, ev(Event::HeartbeatReply(Desired::Ending)))]);
        }
        let tick = sim.drive_to_tick(due, vec![]);
        if ending {
            assert_eq!(sim.d.programme_phase(), ProgrammePhase::Final, "held at {tick}");
            sim.drive(due + t.final_hold_ms - 1, vec![]);
            assert_eq!(abandons(&sim.log), Vec::<u64>::new(), "the hold");
        } else {
            assert_eq!(sim.times_of(abandoned(1)), vec![tick], "the pair");
        }
        checked += 1;
    }
    eprintln!("hold pairs checked: {checked}");
    assert_eq!(checked, 2);
}

/// Frames of a generation the domain gave up never reach the programme through the driver: gen 1's
/// late connect and its frames, from its abandon to gen 2's build, leave every pushed frame a slate
/// and the phase Waiting. Gen 2's frames, once it connects, put the camera on air. The back-off is
/// overridden to three times `resume_after_ms`, so the late frames run long enough to go Live if
/// they counted.
#[test]
fn a_given_up_generations_frames_never_reach_the_programme_through_the_driver() {
    let t = Thresholds {
        input_backoff_base_ms: 3 * th().resume_after_ms,
        ..th()
    };
    let mut sim = Sim::new(t.clone(), session_720p());
    let step = OutputProfile::P720.frame_period_ms().round() as u64;
    let gave_up = sim.tick_until(SESSION_AT + t.input_connect_timeout_ms + step, abandoned(1));
    let mark = sim.log.len();
    let mut late = vec![(gave_up + 1, ev(Event::InputConnected { gen: 1 }))];
    late.extend(every(
        gave_up + 1,
        gave_up + t.input_backoff_base_ms - 1,
        step,
        || frame(1),
    ));
    let sent = late.len();
    sim.drive(gave_up + t.input_backoff_base_ms - 1, late);
    let slates = sim
        .log
        .calls_from(mark)
        .iter()
        .filter(|c| matches!(c, Call::Push(_, f) if *f != FrameChoice::Slate(SlateKind::Waiting)))
        .count();
    eprintln!("given-up gen 1: {sent} events after its abandon at {gave_up}; non-Waiting pushes {slates}");
    assert!(
        sent as u64 * step > t.resume_after_ms + step,
        "the late frames outrun resume_after_ms"
    );
    assert_eq!(slates, 0, "a given-up generation's frame reached the programme");
    assert_eq!(sim.d.programme_phase(), ProgrammePhase::Waiting);
    let r2 = sim.tick_until(gave_up + t.input_backoff_cap_ms, rebuilt(2));
    let mut back = vec![(r2 + 1, ev(Event::InputConnected { gen: 2 }))];
    back.extend(every(r2 + 1, r2 + t.resume_after_ms + 3 * step, step, || {
        frame(2)
    }));
    sim.drive(r2 + t.resume_after_ms + 3 * step, back);
    assert_eq!(
        sim.d.programme_phase(),
        ProgrammePhase::Live,
        "gen 2's frames are counted"
    );
}

// ---------------------------------------------------------------------------------------------
// Owed 87: every swap's log line names what ended the sink before it
// ---------------------------------------------------------------------------------------------

fn swapped(gen: u64) -> impl Fn(&Call) -> bool {
    move |c| matches!(c, Call::Swap(g, _) if *g == gen)
}

fn progress(gen: u64, total_bytes: u64) -> Inbound {
    ev(Event::OutputProgress {
        gen,
        total_bytes,
        encoded: Encoded::default(),
    })
}

/// A reading with both encoders at `k` samples (Owed 94: they deliver while `k` grows).
fn delivering(gen: u64, total_bytes: u64, k: u64) -> Inbound {
    ev(Event::OutputProgress {
        gen,
        total_bytes,
        encoded: Encoded { video: k, audio: k },
    })
}

fn swaps(sim: &Sim) -> usize {
    sim.calls().iter().filter(|c| matches!(c, Call::Swap(..))).count()
}

// Six swaps through the real driver and domain, one per way the module doc of
// `domain::output_link` ends a sink: the first build, the sink's own error (while connecting, while
// proving and once Connected), the connect timeout and the egress backstop. Each line comes just
// before its port call. Since Owed 87 (the owner, 2026-10-24, "A") a silent sink whose encoders are
// not read, proving or Connected, is swapped only after its own error (rtmp2sink's socket timeout,
// for a destination stall), however long it is silent: the silences here run three sink timeouts,
// past where the retired stall rule swapped. Owed 94 (the owner, 2026-10-24, "B"): a Connected sink
// that stops while both encoders keep delivering is swapped by the backstop.
#[test]
fn every_sink_swap_is_logged_with_its_reason_just_before_the_swap() {
    let t = th();
    let (quiet, slack) = (3 * OUTPUT_SINK_TIMEOUT_MS, t.output_backoff_cap_ms + 1_000);
    let mut sim = Sim::new(t.clone(), session_720p());
    let at1 = sim.tick_until(SESSION_AT + 1_000, swapped(1));

    let err = at1 + 100;
    sim.drive(err + 1, vec![(err, ev(Event::OutputError { gen: 1 }))]);
    let at2 = sim.tick_until(err + slack, swapped(2));

    // Generation 2 sends nothing.
    let at3 = sim.tick_until(at2 + t.output_connect_timeout_ms + slack, swapped(3));

    // Generation 3 sends its `connect` command (476 B, F-011), then nothing while it proves, until
    // its error.
    let first = at3 + 31;
    let e3 = first + quiet;
    sim.drive(e3, vec![(first, progress(3, 476))]);
    assert_eq!(swaps(&sim), 3, "a silent proving sink is not swapped");
    sim.drive(e3 + 1, vec![(e3, ev(Event::OutputError { gen: 3 }))]);
    let at4 = sim.tick_until(e3 + slack, swapped(4));

    // Generation 4 grows on the 250 ms poll until it is Connected, then stops, until its error.
    let first = at4 + 31;
    let growth: Vec<(u64, Inbound)> = (0..)
        .map(|k| (first + 250 * k, 476 + 50_000 * k))
        .take_while(|&(at, _)| at <= first + t.output_connected_after_ms + 250)
        .map(|(at, total)| (at, progress(4, total)))
        .collect();
    let last = growth.last().expect("growth").0;
    let e4 = last + quiet;
    sim.drive(e4, growth);
    assert_eq!(swaps(&sim), 4, "a silent Connected sink is not swapped");
    sim.drive(e4 + 1, vec![(e4, ev(Event::OutputError { gen: 4 }))]);
    let at5 = sim.tick_until(e4 + slack, swapped(5));
    let base = t.output_backoff_base_ms;
    assert!(
        (base..base + 100).contains(&(at5 - e4)),
        "a Connected sink's error is failure 1, the base back-off: swapped {} ms after it",
        at5 - e4
    );

    // Generation 5 grows on the poll until it is Connected, both encoders delivering on every
    // reading; then its total stops, the encoders keep delivering, and no error comes.
    let (first, flat) = (at5 + 31, t.output_egress_flat_ms);
    let grown = first + t.output_connected_after_ms + 250;
    let mut readings = Vec::new();
    let (mut total, mut last) = (0, first);
    for (k, at) in (0..).map(|k| (k, first + 250 * k)) {
        if at >= grown + flat - 1 {
            break;
        }
        if at <= grown {
            (total, last) = (476 + 50_000 * k, at);
        }
        readings.push((at, delivering(5, total, k + 1)));
    }
    sim.drive(last + flat - 1, readings);
    assert_eq!(swaps(&sim), 5, "swapped before the backstop's threshold");
    let at6 = sim.tick_until(last + flat + slack, swapped(6));
    assert!(
        (flat + base..flat + base + 100).contains(&(at6 - last)),
        "the backstop, then failure 1's base back-off: swapped {} ms after the last growth",
        at6 - last
    );

    let calls = sim.calls();
    let mut checked = 0;
    for (gen, reason) in [
        (1, "first"),
        (2, "error"),
        (3, "connect_timeout"),
        (4, "error"),
        (5, "error"),
        (6, "egress_stalled"),
    ] {
        let i = calls
            .iter()
            .position(swapped(gen))
            .unwrap_or_else(|| panic!("no swap {gen}"));
        assert_eq!(
            calls[i - 1],
            Call::Log(format!("sink swap reason={reason} gen={gen}")),
            "the line just before swap {gen}"
        );
        checked += 1;
    }
    let lines = calls
        .iter()
        .filter(|c| matches!(c, Call::Log(l) if l.starts_with("sink swap ")))
        .count();
    let swaps = calls.iter().filter(|c| matches!(c, Call::Swap(..))).count();
    eprintln!("swaps {swaps}, swap lines {lines}, checked {checked}");
    assert_eq!((checked, lines, swaps), (6, 6, 6), "one line per swap");
}
