//! Sweeps over C06's aggregate: every (lifecycle, event) pair against a matrix written from the brief,
//! every exit path's final beat, and the ladder's one step per session. They live here, not inline,
//! because the domain's crate root forbids `clippy::print_stderr`, and a sweep must print the count
//! it checked (AGENTS.md §5).
use domain::config::{OutputProfile, Thresholds};
use domain::health::{BeatState, ExitDetail, ExitInfo, HealthReport, PhaseChannel};
use domain::input_link::LossReason;
use domain::ladder::Rung;
use domain::lifecycle::{ExitReason, Lifecycle};
use domain::programme::{FrameChoice, ProgrammePhase, SlateKind};
use domain::supervisor::{
    Decision, Desired, Effect, Event, HeartbeatRefusal, Refusal, SignalKind, Supervisor,
};
use domain::time::Ms;

const FAR: Ms = 10_000_000;

fn t() -> Thresholds {
    Thresholds::default()
}

/// C08 ticks on the frame period: 33 ms in both modes (ADR 0005, 30 fps).
fn period() -> Ms {
    OutputProfile::P720.frame_period_ms().round() as Ms
}

/// The plan's Global Constraints, written independently of `ExitReason::code`: 0 for ended,
/// deadline, signal or session gone (410); 78 for a refusal (401/404/wrong mode); 70 for fatal.
fn contract_code(r: ExitReason) -> i32 {
    match r {
        ExitReason::Ended | ExitReason::Deadline | ExitReason::Signalled | ExitReason::SessionGone => 0,
        ExitReason::Refused => 78,
        ExitReason::Fatal => 70,
    }
}

fn booted(now: Ms, deadline_at: Ms) -> Supervisor {
    let s = Supervisor::new(t()).decide(Event::Tick, 0).next;
    let d = s.decide(
        Event::BootstrapOk {
            heartbeat_interval_ms: t().heartbeat_interval_ms,
            deadline_at,
        },
        now,
    );
    assert_eq!(d.next.lifecycle(), Lifecycle::Running, "setup: booted");
    d.next
}

/// The fatal error's text every `Kind::Fatal` sample carries (Owed 27, option a).
const FATAL_DETAIL: &str = "decoder: not-negotiated";

fn stopped_reason(h: &HealthReport) -> Option<ExitReason> {
    (h.state == BeatState::Stopped)
        .then(|| h.exit.as_ref().map(|e| e.reason))
        .flatten()
}

// ---- every (lifecycle, event) pair ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setup {
    /// A fresh supervisor: a fetch is due.
    BootingDue,
    BootingInFlight,
    /// A fetch failed; the retry is not yet due.
    BootingBackoff,
    Running,
    Holding,
    Draining,
    Exited,
}

const SETUPS: [Setup; 7] = [
    Setup::BootingDue,
    Setup::BootingInFlight,
    Setup::BootingBackoff,
    Setup::Running,
    Setup::Holding,
    Setup::Draining,
    Setup::Exited,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tick,
    BootstrapOk,
    BootstrapFailed,
    RefusedGone,
    RefusedOther,
    InputConnected,
    InputFrame,
    InputLost,
    OutputProgress,
    OutputError,
    OutputDrained,
    EncoderOverrun,
    CefReady,
    CefCrashed,
    CefAlive,
    ScoreStale,
    PhaseChannel,
    Live,
    Ending,
    HeartbeatFailed,
    /// A heartbeat 410 (wave gate 1 review I5).
    HeartbeatGone,
    Term,
    Int,
    Fatal,
}

const KINDS: [Kind; 24] = [
    Kind::Tick,
    Kind::BootstrapOk,
    Kind::BootstrapFailed,
    Kind::RefusedGone,
    Kind::RefusedOther,
    Kind::InputConnected,
    Kind::InputFrame,
    Kind::InputLost,
    Kind::OutputProgress,
    Kind::OutputError,
    Kind::OutputDrained,
    Kind::EncoderOverrun,
    Kind::CefReady,
    Kind::CefCrashed,
    Kind::CefAlive,
    Kind::ScoreStale,
    Kind::PhaseChannel,
    Kind::Live,
    Kind::Ending,
    Kind::HeartbeatFailed,
    Kind::HeartbeatGone,
    Kind::Term,
    Kind::Int,
    Kind::Fatal,
];

/// An exhaustive `match`, so a new `Event` variant is a compile error here and `KINDS` cannot go
/// stale behind a sweep that still reports its old count.
fn kind_of(e: &Event) -> Kind {
    match e {
        Event::Tick => Kind::Tick,
        Event::BootstrapOk { .. } => Kind::BootstrapOk,
        Event::BootstrapFailed => Kind::BootstrapFailed,
        Event::BootstrapRefused(Refusal::Gone) => Kind::RefusedGone,
        Event::BootstrapRefused(Refusal::Unauthorized | Refusal::NotFound | Refusal::WrongMode) => {
            Kind::RefusedOther
        }
        Event::InputConnected { .. } => Kind::InputConnected,
        Event::InputFrame => Kind::InputFrame,
        Event::InputLost { .. } => Kind::InputLost,
        Event::OutputProgress { .. } => Kind::OutputProgress,
        Event::OutputError { .. } => Kind::OutputError,
        Event::OutputDrained => Kind::OutputDrained,
        Event::EncoderOverrun => Kind::EncoderOverrun,
        Event::CefReady { .. } => Kind::CefReady,
        Event::CefCrashed { .. } => Kind::CefCrashed,
        Event::CefAlive { .. } => Kind::CefAlive,
        Event::ScoreStale(_) => Kind::ScoreStale,
        Event::PhaseChannel(_) => Kind::PhaseChannel,
        Event::HeartbeatReply(Desired::Live) => Kind::Live,
        Event::HeartbeatReply(Desired::Ending) => Kind::Ending,
        Event::HeartbeatFailed => Kind::HeartbeatFailed,
        Event::HeartbeatRefused(HeartbeatRefusal::Gone) => Kind::HeartbeatGone,
        Event::Signal(SignalKind::Term) => Kind::Term,
        Event::Signal(SignalKind::Int) => Kind::Int,
        Event::Fatal { .. } => Kind::Fatal,
    }
}

/// One event of `kind`, from the generations `s` holds now.
fn sample(kind: Kind, s: &Supervisor) -> Event {
    let e = match kind {
        Kind::Tick => Event::Tick,
        Kind::BootstrapOk => Event::BootstrapOk {
            heartbeat_interval_ms: t().heartbeat_interval_ms,
            deadline_at: FAR,
        },
        Kind::BootstrapFailed => Event::BootstrapFailed,
        Kind::RefusedGone => Event::BootstrapRefused(Refusal::Gone),
        Kind::RefusedOther => Event::BootstrapRefused(Refusal::Unauthorized),
        Kind::InputConnected => Event::InputConnected {
            gen: s.input_generation(),
        },
        Kind::InputFrame => Event::InputFrame,
        Kind::InputLost => Event::InputLost {
            gen: s.input_generation(),
            reason: LossReason::Error,
        },
        Kind::OutputProgress => Event::OutputProgress {
            gen: s.output_generation(),
            total_bytes: 1_000,
        },
        Kind::OutputError => Event::OutputError {
            gen: s.output_generation(),
        },
        Kind::OutputDrained => Event::OutputDrained,
        Kind::EncoderOverrun => Event::EncoderOverrun,
        Kind::CefReady => Event::CefReady {
            gen: s.graphics_generation(),
        },
        Kind::CefCrashed => Event::CefCrashed {
            gen: s.graphics_generation(),
        },
        Kind::CefAlive => Event::CefAlive {
            gen: s.graphics_generation(),
        },
        Kind::ScoreStale => Event::ScoreStale(true),
        Kind::PhaseChannel => Event::PhaseChannel(PhaseChannel::Web),
        Kind::Live => Event::HeartbeatReply(Desired::Live),
        Kind::Ending => Event::HeartbeatReply(Desired::Ending),
        Kind::HeartbeatFailed => Event::HeartbeatFailed,
        Kind::HeartbeatGone => Event::HeartbeatRefused(HeartbeatRefusal::Gone),
        Kind::Term => Event::Signal(SignalKind::Term),
        Kind::Int => Event::Signal(SignalKind::Int),
        Kind::Fatal => Event::Fatal {
            detail: Some(ExitDetail::new(FATAL_DETAIL)),
        },
    };
    assert_eq!(kind_of(&e), kind, "sample");
    e
}

/// The supervisor in `setup`, and the instant the event is delivered. On air (Running, Holding),
/// every generation is 1 and one heartbeat failure is counted, so every routed event visibly changes
/// the state: the input and the output are connecting, and the page is loading, except for
/// `CefAlive`, whose page is made Ready first (a beat counts only while Ready). The hold and the drain
/// began 50 ms before the event, well inside `final_hold_ms` and `drain_timeout_ms`.
fn setup(st: Setup, kind: Kind) -> (Supervisor, Ms) {
    let on_air = || {
        let mut s = booted(0, FAR).decide(Event::Tick, 0).next;
        s = s.decide(Event::HeartbeatFailed, 0).next;
        if kind == Kind::CefAlive {
            s = s.decide(Event::CefReady { gen: 1 }, 0).next;
        }
        s
    };
    let (s, now) = match st {
        Setup::BootingDue => (Supervisor::new(t()), 0),
        Setup::BootingInFlight => (Supervisor::new(t()).decide(Event::Tick, 0).next, 100),
        Setup::BootingBackoff => {
            let s = Supervisor::new(t()).decide(Event::Tick, 0).next;
            (s.decide(Event::BootstrapFailed, 100).next, 200)
        }
        Setup::Running => (on_air(), 100),
        Setup::Holding => (
            on_air().decide(Event::HeartbeatReply(Desired::Ending), 50).next,
            100,
        ),
        Setup::Draining => (on_air().decide(Event::Signal(SignalKind::Term), 50).next, 100),
        Setup::Exited => (on_air().decide(Event::Fatal { detail: None }, 50).next, 100),
    };
    let ok = match (st, s.lifecycle()) {
        (Setup::BootingDue, Lifecycle::Booting { retry_at, .. }) => retry_at.is_some_and(|r| r <= now),
        (Setup::BootingInFlight, Lifecycle::Booting { retry_at, .. }) => retry_at.is_none(),
        (Setup::BootingBackoff, Lifecycle::Booting { retry_at, .. }) => retry_at.is_some_and(|r| r > now),
        (Setup::Running, Lifecycle::Running) => true,
        (Setup::Holding, Lifecycle::Holding { since, .. }) => now - since < t().final_hold_ms,
        (Setup::Draining, Lifecycle::Draining { since, .. }) => now - since < t().drain_timeout_ms,
        (Setup::Exited, Lifecycle::Exited(_)) => true,
        _ => false,
    };
    assert!(ok, "setup {st:?}: {:?}", s.lifecycle());
    (s, now)
}

/// What a pair must do, written from the brief's prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    /// Nothing changes: not the state, no effect, no frame.
    Ignored,
    /// Delivered to a sub-machine: the state changes, the lifecycle does not, no effect, no frame.
    Routed,
    /// An on-air tick: the lifecycle holds and a frame is yielded (the Final card in the hold).
    Frame,
    /// `FetchSession`, and the fetch is in flight.
    Fetch,
    /// The first failure: the retry waits `bootstrap_backoff_base_ms`.
    Retry,
    /// `StartProgramme`, and Running.
    Start,
    /// The final hold starts now with the reason: no effect, no frame.
    Hold(ExitReason),
    /// The drain starts now: `DrainOutput`, then the final beat with the reason.
    Drain(ExitReason),
    /// An immediate exit: the final beat with the reason, then `Exit`.
    Exit(ExitReason),
    /// `Exit` alone: the drain's end (its final beat went out when it started), or a heartbeat 410
    /// (no beat at all: web refuses every beat on a terminal session with that 410).
    ExitAlone(ExitReason),
}

fn brief(st: Setup, k: Kind) -> Want {
    use ExitReason as R;
    use Kind as K;
    use Setup as S;
    use Want as W;
    match (st, k) {
        (S::Exited, _) => W::Ignored,
        // Booting: one fetch in flight at a time; nothing on air, so every end exits at once.
        (S::BootingDue, K::Tick) => W::Fetch,
        (S::BootingInFlight, K::BootstrapFailed) => W::Retry,
        (S::BootingDue | S::BootingInFlight | S::BootingBackoff, k) => match k {
            K::BootstrapOk => W::Start,
            K::RefusedGone => W::Exit(R::SessionGone),
            K::RefusedOther => W::Exit(R::Refused),
            K::Term | K::Int => W::Exit(R::Signalled),
            K::Fatal => W::Exit(R::Fatal),
            K::HeartbeatGone => W::ExitAlone(R::SessionGone),
            _ => W::Ignored,
        },
        // Draining: only the drain's end, a fatal error or a heartbeat 410.
        (S::Draining, K::OutputDrained) => W::ExitAlone(R::Signalled),
        (S::Draining, K::Fatal) => W::Exit(R::Fatal),
        (S::Draining, K::HeartbeatGone) => W::ExitAlone(R::SessionGone),
        (S::Draining, _) => W::Ignored,
        // On air.
        (S::Running | S::Holding, K::Tick) => W::Frame,
        (S::Running | S::Holding, K::Term) => W::Drain(R::Signalled),
        (S::Running | S::Holding, K::Fatal) => W::Exit(R::Fatal),
        // Web has ended the session: no hold, no drain, no beat (coordinator decision, wave gate 1 I5).
        (S::Running | S::Holding, K::HeartbeatGone) => W::ExitAlone(R::SessionGone),
        (S::Running, K::Ending) => W::Hold(R::Ended),
        (S::Running, K::Int) => W::Hold(R::Signalled),
        // A second end request keeps the first hold's clock; an overrun changes nothing in the hold.
        (S::Holding, K::Ending | K::Int | K::EncoderOverrun) => W::Ignored,
        (
            S::Running | S::Holding,
            K::BootstrapOk | K::BootstrapFailed | K::RefusedGone | K::RefusedOther | K::OutputDrained,
        ) => W::Ignored,
        (S::Running | S::Holding, _) => W::Routed,
    }
}

fn check(st: Setup, k: Kind, s: &Supervisor, now: Ms, d: &Decision) {
    let at = format!("({st:?}, {k:?})");
    let th = t();
    // Owed 27 (a): a Stopped beat carries the fatal error's text iff the event was the fatal error.
    let detail = (k == Kind::Fatal).then(|| ExitDetail::new(FATAL_DETAIL));
    let beat_with = |fx: &Effect, r: ExitReason| {
        matches!(fx, Effect::SendHeartbeat(h) if stopped_reason(h) == Some(r)
            && h.exit == Some(ExitInfo { reason: r, detail: detail.clone() }))
    };
    match brief(st, k) {
        Want::Ignored => {
            assert!(d.effects.is_empty() && d.frame.is_none(), "{at}: {:?}", d.effects);
            assert_eq!(&d.next, s, "{at}: changed the supervisor");
        }
        Want::Routed => {
            assert!(d.effects.is_empty() && d.frame.is_none(), "{at}: {:?}", d.effects);
            assert_eq!(d.next.lifecycle(), s.lifecycle(), "{at}");
            assert_ne!(&d.next, s, "{at}: never reached its sub-machine");
        }
        Want::Frame => {
            assert_eq!(d.next.lifecycle(), s.lifecycle(), "{at}");
            let want = match st {
                Setup::Holding => FrameChoice::Slate(SlateKind::Final),
                _ => FrameChoice::Slate(SlateKind::Waiting), // no camera frame yet
            };
            assert_eq!(d.frame, Some(want), "{at}");
        }
        Want::Fetch => {
            assert_eq!(d.effects, vec![Effect::FetchSession], "{at}");
            assert!(
                matches!(d.next.lifecycle(), Lifecycle::Booting { retry_at: None, .. }),
                "{at}"
            );
        }
        Want::Retry => {
            assert!(d.effects.is_empty() && d.frame.is_none(), "{at}");
            assert_eq!(
                d.next.lifecycle(),
                Lifecycle::Booting {
                    attempt: 1,
                    retry_at: Some(now + th.bootstrap_backoff_base_ms)
                },
                "{at}"
            );
        }
        Want::Start => {
            assert_eq!(d.effects, vec![Effect::StartProgramme], "{at}");
            assert_eq!(d.next.lifecycle(), Lifecycle::Running, "{at}");
        }
        Want::Hold(r) => {
            assert!(d.effects.is_empty() && d.frame.is_none(), "{at}: {:?}", d.effects);
            assert_eq!(
                d.next.lifecycle(),
                Lifecycle::Holding {
                    since: now,
                    reason: r
                },
                "{at}"
            );
            assert_eq!(d.next.programme_phase(), ProgrammePhase::Final, "{at}");
        }
        Want::Drain(r) => {
            assert_eq!(d.effects.len(), 2, "{at}: {:?}", d.effects);
            assert_eq!(d.effects[0], Effect::DrainOutput, "{at}");
            assert!(beat_with(&d.effects[1], r), "{at}: {:?}", d.effects[1]);
            assert_eq!(d.frame, None, "{at}");
            assert_eq!(
                d.next.lifecycle(),
                Lifecycle::Draining {
                    since: now,
                    reason: r
                },
                "{at}"
            );
        }
        Want::Exit(r) => {
            assert_eq!(d.effects.len(), 2, "{at}: {:?}", d.effects);
            assert!(beat_with(&d.effects[0], r), "{at}: {:?}", d.effects[0]);
            assert_eq!(d.effects[1], Effect::Exit(contract_code(r)), "{at}");
            assert_eq!(d.frame, None, "{at}");
            assert_eq!(d.next.lifecycle(), Lifecycle::Exited(r), "{at}");
        }
        Want::ExitAlone(r) => {
            assert_eq!(d.effects, vec![Effect::Exit(contract_code(r))], "{at}");
            assert_eq!(d.next.lifecycle(), Lifecycle::Exited(r), "{at}");
        }
    }
}

#[test]
fn every_lifecycle_event_pair_follows_the_brief() {
    let (mut checked, mut moved) = (0usize, 0usize);
    for st in SETUPS {
        for k in KINDS {
            let (s, now) = setup(st, k);
            let d = s.decide(sample(k, &s), now);
            check(st, k, &s, now, &d);
            checked += 1;
            moved += usize::from(d.next.lifecycle() != s.lifecycle());
        }
    }
    eprintln!("supervisor pairs checked: {checked} ({moved} move the lifecycle)");
    assert_eq!(checked, SETUPS.len() * KINDS.len(), "every pair, once");
    assert!(moved > 0, "a matrix where nothing moves is vacuous");
}

// ---- every exit path's final beat (owner ruling 2026-10-15), but a heartbeat 410's ----

/// Applies `events` in order and returns the supervisor and every effect, in order.
fn run(mut s: Supervisor, events: &[(Ms, Event)]) -> (Supervisor, Vec<Effect>) {
    let mut fx = Vec::new();
    for (at, e) in events {
        let d = s.decide(e.clone(), *at);
        fx.extend(d.effects);
        s = d.next;
    }
    (s, fx)
}

/// Ticks on the frame period from `from` until a `DrainOutput` or an `Exit` comes out, at most
/// `budget` ms; then, unless the drain is to run out, `OutputDrained`. Returns the supervisor and
/// every effect, in order.
fn tick_to_the_end(mut s: Supervisor, from: Ms, budget: Ms, drained: bool) -> (Supervisor, Vec<Effect>) {
    let mut fx = Vec::new();
    let mut now = from;
    let mut draining = false;
    while now <= from + budget {
        let d = s.decide(Event::Tick, now);
        draining |= d.effects.contains(&Effect::DrainOutput);
        let exited = d.effects.iter().any(|e| matches!(e, Effect::Exit(_)));
        fx.extend(d.effects);
        s = d.next;
        if exited {
            return (s, fx);
        }
        if draining && drained {
            let d = s.decide(Event::OutputDrained, now + 1);
            fx.extend(d.effects);
            return (d.next, fx);
        }
        now += period();
    }
    panic!("no end within {budget} ms of {from}");
}

#[test]
fn every_exit_but_a_heartbeat_410_is_preceded_by_a_final_beat_with_its_reason() {
    let th = t();
    let hold_and_drain = th.final_hold_ms + th.drain_timeout_ms + 2 * period();
    let on_air = || booted(0, FAR).decide(Event::Tick, 0).next;
    let fresh = || Supervisor::new(t()).decide(Event::Tick, 0).next;
    let mut paths: Vec<(&str, ExitReason, Supervisor, Vec<Effect>)> = Vec::new();

    let (s, mut fx) = run(on_air(), &[(100, Event::HeartbeatReply(Desired::Ending))]);
    let (s, more) = tick_to_the_end(s, 100, hold_and_drain, true);
    fx.extend(more);
    paths.push(("an Ending reply, the hold, drained", ExitReason::Ended, s, fx));

    let deadline = 60_000;
    let s = booted(0, deadline).decide(Event::Tick, 0).next;
    let (s, fx) = tick_to_the_end(s, deadline - 10 * period(), hold_and_drain + 10 * period(), false);
    paths.push((
        "the deadline tick, the hold, the drain timeout",
        ExitReason::Deadline,
        s,
        fx,
    ));

    let (s, mut fx) = run(on_air(), &[(100, Event::Signal(SignalKind::Int))]);
    let (s, more) = tick_to_the_end(s, 100, hold_and_drain, true);
    fx.extend(more);
    paths.push(("SIGINT, the hold, drained", ExitReason::Signalled, s, fx));

    let (s, mut fx) = run(on_air(), &[(100, Event::Signal(SignalKind::Term))]);
    let (s, more) = tick_to_the_end(s, 100, hold_and_drain, false);
    fx.extend(more);
    paths.push(("SIGTERM, the drain timeout", ExitReason::Signalled, s, fx));

    let (s, fx) = run(fresh(), &[(5, Event::BootstrapRefused(Refusal::Gone))]);
    paths.push(("a 410 at bootstrap", ExitReason::SessionGone, s, fx));

    let (s, fx) = run(fresh(), &[(5, Event::BootstrapRefused(Refusal::Unauthorized))]);
    paths.push(("a 401 at bootstrap", ExitReason::Refused, s, fx));

    let fatal = Event::Fatal {
        detail: Some(ExitDetail::new(FATAL_DETAIL)),
    };
    let (s, fx) = run(on_air(), &[(100, fatal)]);
    paths.push(("a fatal error while Running", ExitReason::Fatal, s, fx));

    // The one exit with no final beat (coordinator decision, wave gate 1 review I5): web refuses every
    // beat on a terminal session with this same 410.
    let no_beat = "a heartbeat 410 while Running";
    let gone = Event::HeartbeatRefused(HeartbeatRefusal::Gone);
    let (s, fx) = run(on_air(), &[(100, gone)]);
    paths.push((no_beat, ExitReason::SessionGone, s, fx));

    let mut checked = 0;
    for (name, reason, s, fx) in &paths {
        let exits: Vec<usize> = (0..fx.len())
            .filter(|&i| matches!(fx[i], Effect::Exit(_)))
            .collect();
        assert_eq!(exits.len(), 1, "{name}: one Exit, in {fx:?}");
        let exit_at = exits[0];
        assert_eq!(fx[exit_at], Effect::Exit(contract_code(*reason)), "{name}");
        assert_eq!(exit_at, fx.len() - 1, "{name}: nothing after Exit");
        let finals: Vec<usize> = (0..fx.len())
            .filter(|&i| matches!(&fx[i], Effect::SendHeartbeat(h) if h.state == BeatState::Stopped))
            .collect();
        if *name == no_beat {
            assert!(finals.is_empty(), "{name}: no beat for web to refuse: {fx:?}");
        } else {
            assert_eq!(finals.len(), 1, "{name}: one final beat");
            assert!(finals[0] < exit_at, "{name}: the final beat comes before Exit");
            let Effect::SendHeartbeat(h) = &fx[finals[0]] else {
                unreachable!()
            };
            assert_eq!(stopped_reason(h), Some(*reason), "{name}");
            let detail = (*reason == ExitReason::Fatal).then(|| ExitDetail::new(FATAL_DETAIL));
            assert_eq!(h.exit.as_ref().and_then(|x| x.detail.clone()), detail, "{name}");
        }
        for e in fx {
            if let Effect::SendHeartbeat(h) = e {
                assert_eq!(
                    h.exit.is_some(),
                    h.state == BeatState::Stopped,
                    "{name}: exit on a {:?} beat",
                    h.state
                );
            }
        }
        assert_eq!(s.lifecycle(), Lifecycle::Exited(*reason), "{name}");
        checked += 1;
    }
    eprintln!("exit paths checked: {checked}");
    assert_eq!(checked, 8, "every exit path, each reason at least once");
    let reasons: Vec<ExitReason> = paths.iter().map(|p| p.1).collect();
    for r in ExitReason::ALL {
        assert!(reasons.contains(&r), "{r:?}: no path reaches it");
    }
}

// ---- the ladder's one step per session (owner ruling 2026-10-18: no lite mode) ----

/// A small deterministic generator (an LCG): no RNG crate, and the same cases every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

#[test]
fn bursts_of_overruns_step_down_once_per_session() {
    let th = t();
    let need = th.ladder_overruns_to_step as u64;
    let mut rng = Lcg(0x05ee_dc06);
    let (mut sessions, mut bursts, mut overruns_sent) = (0u64, 0u64, 0u64);
    for _ in 0..48 {
        let mut s = booted(0, FAR).decide(Event::Tick, 0).next;
        let (mut now, mut sent, mut steps) = (0, 0u64, Vec::new());
        for _ in 0..1 + rng.next(8) {
            now += 2 * th.ladder_window_ms + rng.next(20_000);
            // Enough overruns to step, all inside one window (spaced below window / count).
            let count = need + rng.next(2 * need);
            let gap = th.ladder_window_ms / (count + 1);
            for k in 0..count {
                let d = s.decide(Event::EncoderOverrun, now + k * gap);
                steps.extend(d.effects.into_iter().filter(|e| matches!(e, Effect::StepDown(_))));
                s = d.next;
                sent += 1;
            }
            now += count * gap;
            let d = s.decide(Event::Tick, now);
            s = d.next;
            assert_eq!(s.health(now).rung, Rung::OverlayHalfRate, "after burst {bursts}");
            bursts += 1;
        }
        assert_eq!(
            steps,
            vec![Effect::StepDown(Rung::OverlayHalfRate)],
            "session {sessions}"
        );
        assert_eq!(
            s.health(now).dropped_frames,
            sent,
            "session {sessions}: every overrun counted"
        );
        overruns_sent += sent;
        sessions += 1;
    }
    eprintln!("ladder sessions checked: {sessions}, bursts: {bursts}, overruns: {overruns_sent}");
    assert!(
        bursts > sessions,
        "anti-vacuity: some session must burst more than once"
    );
    assert!(sessions > 0 && bursts > 0);
}

// ---- Owed 31: a fatal error's text never reaches a `Debug` string ----

/// A planted key in a fatal error's text, the way an element's error names its location. The C08 and
/// C12 holders (`ControlRequest`, `Inbound`, `Driver`) derive `Debug`, so every domain value they hold
/// must print without it. The text still rides the final beat verbatim (its one reader is `as_str`).
#[test]
fn no_debug_string_shows_a_fatal_detail() {
    const KEY: &str = "PLANTED-OWED31-KEY";
    let text = format!("rtmp2sink0: could not connect to rtmps://live.cloudflare.com:443/live/{KEY}");
    let on_air = || booted(0, FAR).decide(Event::Tick, 0).next;
    let draining = || {
        let d = on_air().decide(Event::Signal(SignalKind::Term), 10);
        assert!(
            matches!(d.next.lifecycle(), Lifecycle::Draining { .. }),
            "setup: a drain"
        );
        d.next
    };
    let mut checked = 0;
    for (name, s) in [("live", on_air()), ("draining", draining())] {
        let ev = Event::Fatal {
            detail: Some(ExitDetail::new(text.clone())),
        };
        let d = s.decide(ev.clone(), 20);
        let report = d.next.health(30);
        let carried = report.exit.as_ref().and_then(|x| x.detail.as_ref());
        assert_eq!(
            carried.map(ExitDetail::as_str),
            Some(text.as_str()),
            "{name}: carried verbatim"
        );
        let mut shown: Vec<(&str, String)> = vec![
            ("Event", format!("{ev:?}")),
            ("Event alt", format!("{ev:#?}")),
            ("Decision", format!("{d:?}")),
            ("Decision alt", format!("{d:#?}")),
            ("Supervisor", format!("{:?}", d.next)),
            ("Supervisor alt", format!("{:#?}", d.next)),
            ("HealthReport", format!("{report:?}")),
            ("HealthReport alt", format!("{report:#?}")),
            ("ExitInfo", format!("{:?}", report.exit)),
        ];
        for e in &d.effects {
            shown.push(("Effect", format!("{e:?}")));
            shown.push(("Effect alt", format!("{e:#?}")));
        }
        assert!(
            d.effects
                .iter()
                .any(|e| matches!(e, Effect::SendHeartbeat(h) if h.exit.is_some())),
            "{name}: the final beat is among the effects"
        );
        for (what, s) in &shown {
            assert!(!s.contains(KEY), "{name}: {what} shows the fatal text: {s}");
            assert!(
                !s.contains("rtmp2sink0"),
                "{name}: {what} shows the fatal text: {s}"
            );
            checked += 1;
        }
    }
    eprintln!("Debug strings checked for a fatal detail: {checked}");
    assert!(checked >= 2 * 11, "anti-vacuity: {checked}");
}
