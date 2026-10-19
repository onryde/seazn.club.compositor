//! The driver (C08) folding the REAL `Supervisor` through fakes for every port, on a manual clock.
//! Every duration comes from `Thresholds::default()` (or a named override), never from the driver.
use app::driver::Driver;
use app::ports::{ControlRequest, Inbound, Measured, SessionConfig};
use domain::config::{OutputProfile, Thresholds};
use domain::health::{ExitDetail, GraphicsStatus, HealthReport};
use domain::ladder::Rung;
use domain::lifecycle::ExitReason;
use domain::programme::{FrameChoice, ProgrammePhase, SlateKind};
use domain::supervisor::{Desired, Event, HeartbeatRefusal, Refusal, SignalKind};
use testkit::fakes::{ports, session_720p, Call, FakeGraphics, FakeProgramme, Log};

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

    /// The last beat requested at or before `at`.
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
        d.step(ev(Event::InputFrame), now);
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
    let frame = OutputProfile::P720.frame_period_ms().round() as u64;
    // Spec §5's walk: frames until Live, a stop past frozen_after_ms then slate_after_ms, frames past
    // resume_after_ms, an Ending reply, then the hold and the drain.
    let stop = SESSION_AT + t.resume_after_ms + 1_000;
    let back = stop + t.slate_after_ms + 1_000;
    let ending = back + t.resume_after_ms + 500;
    let back_until = ending + 500;
    let mut events = every(SESSION_AT, stop, frame, || ev(Event::InputFrame));
    events.extend(every(back, back_until, frame, || ev(Event::InputFrame)));
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

/// A hang hide: the line, then `HideOverlay`, then `GraphicsStop`, in one tick's call, at the first
/// tick at or after the page's last beat + `cef_hang_after_ms`. Returns that tick.
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
        Call::HideOverlay,
        "the line, then the hide (gen {gen})"
    );
    assert_eq!(
        calls[l + 2],
        Call::GraphicsStop,
        "the hide, then the stop (gen {gen})"
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
    // The negative half: the line, HideOverlay, then GraphicsStop, in that order, in that tick.
    assert_eq!(sim.times_of(|c| *c == Call::HideOverlay), vec![failed_at]);
    assert_eq!(sim.times_of(|c| *c == Call::GraphicsStop), vec![failed_at]);
    let calls = sim.calls();
    let l = calls
        .iter()
        .position(|c| *c == Call::Log(format!("overlay hidden reason=failed gen={n}")))
        .expect("the failed hide's line");
    assert_eq!(calls[l + 1..l + 3], [Call::HideOverlay, Call::GraphicsStop]);
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
    eprintln!("calls checked: {}", sim.calls().len());
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
    eprintln!("calls checked: {}", sim.calls().len());
}

/// A hang while the camera is away: the overlay's watch does not depend on the programme phase.
#[test]
fn a_hang_in_reconnecting_still_hides_and_restarts() {
    let t = th();
    let mut sim = Sim::new(t.clone(), session_720p());
    let frame = OutputProfile::P720.frame_period_ms().round() as u64;
    let stop = SESSION_AT + t.resume_after_ms + 1_000;
    let mut events = every(SESSION_AT, stop, frame, || ev(Event::InputFrame));
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
    for (at, f) in every(20, 1_500, 33, || ev(Event::InputFrame)) {
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
        ev(Event::InputFrame),
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
    d.step(ev(Event::InputFrame), 30);
    d.step(ev(Event::ScoreStale(true)), 31);
    assert!(log.calls_from(mark).iter().all(|c| !matches!(c, Call::Log(_))));
    eprintln!("graphics lines checked: {checked}");
    assert_eq!(checked, 5);
}
