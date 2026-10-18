//! Properties over random event streams. The ORACLE recomputes programme rules from raw frame times
//! the test itself recorded, and the hold and exit rules from `Thresholds` and the exit contract: it
//! never asks the code under test what the answer should be. Every property counts what it checked,
//! prints it, and fails on 0 (AGENTS.md §5).
use domain::config::Thresholds;
use domain::graphics::HideReason;
use domain::health::{BeatState, GraphicsStatus};
use domain::input_link::LossReason;
use domain::ladder::Rung;
use domain::lifecycle::{ExitReason, Lifecycle};
use domain::programme::{FrameChoice, ProgrammePhase, SlateKind};
use domain::supervisor::{Desired, Effect, Event, HeartbeatRefusal, SignalKind, Supervisor};
use domain::time::Ms;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

const CASES: u32 = 512;
/// The exit property's cases: the combinations it must reach in the hold (a hang, a crash at the
/// count) are each a few in a hundred, and a stream is cheap.
const END_CASES: u32 = 2_048;

fn steps() -> impl Strategy<Value = Vec<(Ms, u8)>> {
    // dt 0..300 ms; kind 0..=17, ticks (0..=4) and camera frames (5..=9) dominant
    prop::collection::vec((0u64..300, 0u8..=17), 1..600)
}

/// The harness's own facts, carried across a stream: the output's growing bytes-sent total.
struct Harness {
    sent: u64,
}

fn event_for(kind: u8, s: &Supervisor, h: &mut Harness) -> Event {
    match kind {
        0..=4 => Event::Tick,
        5..=9 => Event::InputFrame,
        10 => Event::InputConnected {
            gen: s.input_generation(),
        },
        11 => Event::InputLost {
            gen: s.input_generation(),
            reason: LossReason::Eos,
        },
        12 => {
            h.sent += 1_000;
            Event::OutputProgress {
                gen: s.output_generation(),
                total_bytes: h.sent,
            }
        }
        13 => Event::CefReady {
            gen: s.graphics_generation(),
        },
        14 => Event::CefAlive {
            gen: s.graphics_generation(),
        },
        // A beat from a page that is not the current one: the one before it, or one never built.
        15 => Event::CefAlive {
            gen: match s.graphics_generation() {
                0 => 1,
                g => g - 1,
            },
        },
        16 => Event::HeartbeatReply(Desired::Live),
        _ => Event::OutputError {
            gen: s.output_generation(),
        },
    }
}

fn booted(t: &Thresholds, deadline_at: Ms) -> Supervisor {
    let s = Supervisor::new(t.clone()).decide(Event::Tick, 0).next;
    s.decide(
        Event::BootstrapOk {
            heartbeat_interval_ms: 5_000,
            deadline_at,
        },
        0,
    )
    .next
}

fn on_air(l: Lifecycle) -> bool {
    matches!(l, Lifecycle::Running | Lifecycle::Holding { .. })
}

#[test]
fn programme_and_generation_properties_hold_on_random_streams() {
    let t = Thresholds::default();
    let checked = Cell::new(0u32);
    let ticks = Cell::new(0u64);
    let into_live = Cell::new(0u64);
    let alive_while_ready = Cell::new(0u64);
    let stale_alive = Cell::new(0u64);
    let mut runner = TestRunner::new(Config {
        cases: CASES,
        ..Config::default()
    });
    runner
        .run(&steps(), |steps| {
            checked.set(checked.get() + 1);
            let mut s = booted(&t, Ms::MAX / 2);
            let mut h = Harness { sent: 0 };
            let mut now: Ms = 0;
            let mut frames: Vec<Ms> = Vec::new();
            let mut last_rebuild = 0u64;
            for (dt, kind) in steps {
                now += dt;
                let ev = event_for(kind, &s, &mut h);
                if ev == Event::InputFrame {
                    frames.push(now);
                }
                let before = s.programme_phase();
                let d = s.decide(ev.clone(), now);
                // P1′: a tick yields exactly one frame iff the supervisor is on air before and after
                // it; nothing else ever does
                let air = on_air(s.lifecycle()) && on_air(d.next.lifecycle());
                prop_assert_eq!(d.frame.is_some(), air && ev == Event::Tick, "frame on {:?}", ev);
                if ev == Event::Tick && air {
                    ticks.set(ticks.get() + 1);
                }
                // P2: entering Live from Waiting/Reconnecting needs ≥ resume_after of unbroken frames
                let after = d.next.programme_phase();
                if after == ProgrammePhase::Live
                    && matches!(before, ProgrammePhase::Waiting | ProgrammePhase::Reconnecting)
                {
                    into_live.set(into_live.get() + 1);
                    let mut run_start = *frames.last().expect("live with no frames");
                    for w in frames.windows(2).rev() {
                        if w[1] - w[0] > t.frozen_after_ms {
                            break;
                        }
                        run_start = w[0];
                    }
                    prop_assert!(
                        now - run_start >= t.resume_after_ms,
                        "live after only {} ms of run",
                        now - run_start
                    );
                }
                // P3: leaving Live needs the newest frame to be older than frozen_after
                if before == ProgrammePhase::Live && after != ProgrammePhase::Live {
                    let age = frames.last().map_or(Ms::MAX, |&f| now - f);
                    prop_assert!(age > t.frozen_after_ms, "left Live with a {} ms old frame", age);
                }
                // P4: input generations rise by exactly one per rebuild
                for e in &d.effects {
                    if let Effect::RebuildInput { gen } = e {
                        prop_assert_eq!(*gen, last_rebuild + 1);
                        last_rebuild = *gen;
                    }
                }
                // P4, the page's beat (Owed 18): a beat from any page but the current one changes
                // nothing; one from the current page while Ready is delivered (counted below)
                if let Event::CefAlive { gen } = ev {
                    if gen != s.graphics_generation() {
                        stale_alive.set(stale_alive.get() + 1);
                        prop_assert_eq!(&d.next, &s, "a stale beat changed the supervisor");
                        prop_assert!(d.effects.is_empty());
                    } else if s.health(now).graphics == GraphicsStatus::Ready && on_air(s.lifecycle()) {
                        alive_while_ready.set(alive_while_ready.get() + 1);
                    }
                }
                s = d.next;
            }
            Ok(())
        })
        .expect("property failed");
    eprintln!(
        "cases checked: {}, on-air ticks: {}, transitions into Live: {}, beats while Ready: {}, stale beats: {}",
        checked.get(),
        ticks.get(),
        into_live.get(),
        alive_while_ready.get(),
        stale_alive.get()
    );
    assert_eq!(checked.get(), CASES, "anti-vacuity: every configured case ran");
    assert!(ticks.get() > 0, "anti-vacuity: no on-air tick was ever checked");
    assert!(
        into_live.get() > 0,
        "anti-vacuity: P2 never fired — the generator never reached Live"
    );
    assert!(
        alive_while_ready.get() > 0,
        "anti-vacuity: no beat reached a Ready page"
    );
    assert!(stale_alive.get() > 0, "anti-vacuity: no stale beat was checked");
}

/// The exit contract, written independently of `ExitReason::code`.
fn contract_code(r: ExitReason) -> i32 {
    match r {
        ExitReason::Ended | ExitReason::Deadline | ExitReason::Signalled | ExitReason::SessionGone => 0,
        ExitReason::Refused => 78,
        ExitReason::Fatal => 70,
    }
}

fn end_stream() -> impl Strategy<Value = (Option<Ms>, bool, Vec<(Ms, u8)>)> {
    // A deadline inside the stream, or none; whether the page beats at the start; dt 0..2 000 ms.
    // An end event comes about every 8 steps (Ending ×2, SIGINT and SIGTERM of the 40 kinds, a fatal
    // error at half and a 410 at a quarter of that), so a stream runs several seconds before its
    // first end request, long enough for an overrun burst, a crash storm, a Ready page or a silence
    // to come first, and the deadline is drawn early enough to come first in a fair share of cases.
    (
        prop::option::of(1u64..20_000),
        any::<bool>(),
        prop::collection::vec((0u64..2_000, 0u8..=39), 1..200),
    )
}

/// One event of the stream. The gen-carrying ones are resolved against the supervisor at delivery,
/// so they name the page or the sink current then (a page built mid-stream is the one that answers).
#[derive(Debug, Clone)]
enum Ev {
    Plain(Event),
    /// `CefReady` of the current page.
    Ready,
    /// `CefAlive` of the current page.
    Alive,
    /// `CefCrashed` of the current page.
    Crash,
    /// `OutputProgress` of the current sink, its total grown.
    Progress,
}

/// The stream's events at their instants, in order. Between the steps run the background: a tick
/// and a growing output total every second (the sink stays connected, and the hold is ticked on a
/// grid fine enough for its bound), and, while the page beats, `CefAlive` every
/// `cef_alive_every_ms`. A step is one event, or one of four bursts:
/// - camera frames and ticks every 100 ms for `resume_after_ms` + 200, unbroken and long enough to
///   read as continuous, so P5 sees frames try to take the programme out of Final;
/// - `ladder_overruns_to_step` overruns 50 ms apart, inside one ladder window, so the ladder steps;
/// - a crash storm: two crashes of the current page, each followed by ticks past its back-off, so
///   the page restarts with two failures counted, and one more crash reaches the count;
/// - a toggle between a beating page and a silent one (a silent Ready page hangs at its bound).
struct Stream {
    out: Vec<(Ms, Ev)>,
    next_second: Ms,
    next_alive: Ms,
    beating: bool,
}

impl Stream {
    /// Delivers the background due at or before `at`, then `ev` at `at`.
    fn push(&mut self, at: Ms, ev: Ev, t: &Thresholds) {
        loop {
            let alive = if self.beating { self.next_alive } else { Ms::MAX };
            let due = self.next_second.min(alive);
            if due > at {
                break;
            }
            if due == self.next_second {
                self.out.push((due, Ev::Progress));
                self.out.push((due, Ev::Plain(Event::Tick)));
                self.next_second += 1_000;
            } else {
                self.out.push((due, Ev::Alive));
                self.next_alive += t.cef_alive_every_ms;
            }
        }
        self.out.push((at, ev));
    }

    fn ticks(&mut self, from: Ms, span: Ms, t: &Thresholds) {
        for at in (from..=from + span).step_by(100) {
            self.push(at, Ev::Plain(Event::Tick), t);
        }
    }
}

fn end_events(beating: bool, steps: Vec<(Ms, u8)>, t: &Thresholds) -> Vec<(Ms, Ev)> {
    let mut st = Stream {
        out: Vec::new(),
        next_second: 0,
        next_alive: 0,
        beating,
    };
    let mut now = 0;
    for (dt, k) in steps {
        now += dt;
        let ev = match k {
            0..=13 | 37..=39 => Event::Tick,
            14 | 15 => Event::HeartbeatReply(Desired::Ending),
            16 => Event::Signal(SignalKind::Int),
            17 | 18 => Event::OutputDrained,
            19 => Event::Signal(SignalKind::Term),
            20 => Event::HeartbeatReply(Desired::Live),
            // Half as often as the other end events, so holds run out. The text, opaque: none, empty,
            // or some (Owed 27, option a).
            21 if dt < 1_000 => Event::Fatal {
                detail: match dt % 3 {
                    0 => None,
                    1 => Some(String::new()),
                    _ => Some(format!("fatal at {now}")),
                },
            },
            21 => Event::Tick,
            22 | 23 => {
                let span = t.resume_after_ms + 200;
                for at in (now..=now + span).step_by(100) {
                    st.push(at, Ev::Plain(Event::InputFrame), t);
                    st.push(at, Ev::Plain(Event::Tick), t);
                }
                now += span;
                continue;
            }
            24..=26 => {
                st.push(now, Ev::Ready, t);
                continue;
            }
            27 | 28 => {
                st.push(now, Ev::Crash, t);
                continue;
            }
            29 | 30 => {
                for i in 0..t.ladder_overruns_to_step as Ms {
                    st.push(now + 50 * i, Ev::Plain(Event::EncoderOverrun), t);
                }
                now += 50 * t.ladder_overruns_to_step as Ms;
                continue;
            }
            31 | 32 => {
                st.beating = !st.beating;
                st.next_alive = now;
                continue;
            }
            33 | 34 => {
                for wait in [t.cef_backoff_base_ms, 2 * t.cef_backoff_base_ms] {
                    st.push(now, Ev::Crash, t);
                    st.ticks(now + 100, wait + 200, t);
                    now += wait + 400;
                }
                continue;
            }
            // A quarter as often as the other end events: it ends the stream at once, hold or not.
            35 if dt < 500 => Event::HeartbeatRefused(HeartbeatRefusal::Gone),
            _ => Event::HeartbeatFailed,
        };
        st.push(now, Ev::Plain(ev), t);
    }
    st.out
}

/// The event `ev` names, against the supervisor at delivery.
fn resolve(ev: Ev, s: &Supervisor, sent: &mut u64) -> Event {
    match ev {
        Ev::Plain(e) => e,
        Ev::Ready => Event::CefReady {
            gen: s.graphics_generation(),
        },
        Ev::Alive => Event::CefAlive {
            gen: s.graphics_generation(),
        },
        Ev::Crash => Event::CefCrashed {
            gen: s.graphics_generation(),
        },
        Ev::Progress => {
            *sent += 1_000;
            Event::OutputProgress {
                gen: s.output_generation(),
                total_bytes: *sent,
            }
        }
    }
}

/// What the hold froze at its start: the three generations and the rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frozen {
    gens: (u64, u64, u64),
    rung: Rung,
}

fn frozen(s: &Supervisor, at: Ms) -> Frozen {
    Frozen {
        gens: (
            s.input_generation(),
            s.output_generation(),
            s.graphics_generation(),
        ),
        rung: s.health(at).rung,
    }
}

/// Named counts, each of which must be above 0 (AGENTS.md §5).
#[derive(Default)]
struct Counts(BTreeMap<&'static str, u64>);

impl Counts {
    fn bump(&mut self, k: &'static str) {
        *self.0.entry(k).or_insert(0) += 1;
    }
}

#[test]
fn exit_is_emitted_at_most_once_and_nothing_follows_it() {
    let t = Thresholds::default();
    let checked = Cell::new(0u32);
    let counts = RefCell::new(Counts::default());
    // Holds entered by reason: Ended, Signalled, Deadline
    let holds = Cell::new([0u64; 3]);
    let mut runner = TestRunner::new(Config {
        cases: END_CASES,
        ..Config::default()
    });
    runner
        .run(&end_stream(), |(deadline, beating, steps)| {
            checked.set(checked.get() + 1);
            let mut c = counts.borrow_mut();
            let mut s = booted(&t, deadline.unwrap_or(Ms::MAX / 2));
            let mut sent = 0u64;
            let mut seen_exit = false;
            let mut seen_final = false;
            let mut stepdowns = 0;
            let mut hold: Option<Frozen> = None;
            // The last proof of life each page delivered while Ready: its ready, then each beat.
            let mut proof: BTreeMap<u64, Ms> = BTreeMap::new();
            // The reason of the last Stopped beat sent, in effect order
            let mut last_final: Option<ExitReason> = None;
            for (now, ev) in end_events(beating, steps, &t) {
                let ev = resolve(ev, &s, &mut sent);
                if ev == Event::InputFrame && s.programme_phase() == ProgrammePhase::Final {
                    c.bump("frames in Final");
                }
                let (life, g) = (s.lifecycle(), s.health(now).graphics);
                let gen = s.graphics_generation();
                let d = s.decide(ev.clone(), now);
                let next = d.next.lifecycle();
                let n = d.effects.iter().filter(|e| matches!(e, Effect::Exit(_))).count();
                if seen_exit {
                    prop_assert!(d.effects.is_empty() && d.frame.is_none());
                }
                prop_assert!(n <= 1);
                // P1′ across the end: a tick yields a frame iff on air before and after it
                let air = on_air(life) && on_air(next);
                prop_assert_eq!(d.frame.is_some(), air && ev == Event::Tick, "frame on {:?}", ev);
                // The page's proofs, from the rules: a ready counts while its page loads, a beat while
                // it is Ready, each only from the current page and only on air
                match &ev {
                    Event::CefReady { gen: e }
                        if *e == gen && g == GraphicsStatus::Starting && on_air(life) =>
                    {
                        prop_assert_eq!(d.next.health(now).graphics, GraphicsStatus::Ready);
                        proof.insert(gen, now);
                    }
                    Event::CefAlive { gen: e } if *e == gen && g == GraphicsStatus::Ready && on_air(life) => {
                        let p = proof.get_mut(&gen).expect("a Ready page with no delivered ready");
                        *p = (*p).max(now);
                    }
                    _ => {}
                }
                // The bound: an on-air tick hides a Ready page as hung iff its last delivered proof
                // is at least cef_hang_after_ms old, in Running and in the hold alike
                if ev == Event::Tick && air && g == GraphicsStatus::Ready {
                    let p = *proof.get(&gen).expect("a Ready page with no delivered proof");
                    let due = now - p >= t.cef_hang_after_ms;
                    let hid = d.effects.contains(&Effect::HideOverlay {
                        reason: HideReason::Hang,
                        gen,
                    });
                    prop_assert_eq!(due, hid, "Ready page {} at {}, last proof {}", gen, now, p);
                    c.bump("Ready bounds checked");
                    if due && matches!(life, Lifecycle::Holding { .. }) {
                        c.bump("hang hides in the hold");
                    }
                }
                // P6: every beat carries an exit reason iff it is Stopped, and a fatal error's text
                // iff the beat is that fatal error's; each Exit follows a Stopped beat whose reason
                // is the exit's, with the contract's code, but a 410's, which is Exit(0) alone
                let fatal_detail = match &ev {
                    Event::Fatal { detail } => Some(detail.clone()),
                    _ => None,
                };
                for e in &d.effects {
                    match e {
                        Effect::SendHeartbeat(h) => {
                            prop_assert_eq!(h.exit.is_some(), h.state == BeatState::Stopped);
                            if let Some(x) = &h.exit {
                                last_final = Some(x.reason);
                                c.bump("final beats");
                                let want = if x.reason == ExitReason::Fatal {
                                    c.bump("fatal details checked");
                                    fatal_detail.clone().expect("a fatal beat with no fatal error")
                                } else {
                                    None
                                };
                                prop_assert_eq!(&x.detail, &want);
                            }
                        }
                        Effect::Exit(code) => {
                            let Lifecycle::Exited(r) = next else {
                                return Err(TestCaseError::fail("Exit without Exited"));
                            };
                            prop_assert_eq!(*code, contract_code(r));
                            if ev == Event::HeartbeatRefused(HeartbeatRefusal::Gone) {
                                prop_assert_eq!(r, ExitReason::SessionGone);
                                prop_assert_eq!(&d.effects, &vec![Effect::Exit(0)], "a 410 is Exit alone");
                                c.bump("exits on a 410");
                            } else {
                                prop_assert_eq!(
                                    last_final,
                                    Some(r),
                                    "Exit({}) with no final beat for it",
                                    code
                                );
                            }
                        }
                        Effect::StepDown(_) => stepdowns += 1,
                        _ => {}
                    }
                }
                // The ladder steps once per session at most (owner ruling 2026-10-18)
                prop_assert!(stepdowns <= 1, "a second StepDown");
                // P5: once Final, the programme stays Final, and nothing is rebuilt or stepped down
                if seen_final {
                    prop_assert_eq!(d.next.programme_phase(), ProgrammePhase::Final);
                    for e in &d.effects {
                        prop_assert!(
                            !matches!(
                                e,
                                Effect::RebuildInput { .. }
                                    | Effect::SwapSink { .. }
                                    | Effect::RestartGraphics { .. }
                                    | Effect::StepDown(_)
                            ),
                            "a build after Final: {:?}",
                            e
                        );
                    }
                }
                if d.frame == Some(FrameChoice::Slate(SlateKind::Final)) {
                    c.bump("final ticks");
                }
                // The hold freezes the generations and the rung it started with
                if let (Lifecycle::Running, Lifecycle::Holding { .. }) = (life, next) {
                    hold = Some(frozen(&s, now));
                    if stepdowns == 1 {
                        c.bump("holds while stepped");
                    }
                }
                if let Lifecycle::Holding { .. } = next {
                    let at_start = hold.expect("a hold not entered from Running");
                    prop_assert_eq!(
                        frozen(&d.next, now),
                        at_start,
                        "the hold moved a generation or the rung"
                    );
                    if let Event::CefCrashed { gen: e } = ev {
                        if e == gen && matches!(g, GraphicsStatus::Starting | GraphicsStatus::Ready) {
                            c.bump("crashes in the hold");
                            let failed = Effect::HideOverlay {
                                reason: HideReason::Failed,
                                gen,
                            };
                            prop_assert!(
                                d.effects.is_empty() || d.effects == vec![failed.clone()],
                                "{:?}",
                                d.effects
                            );
                            if d.effects == vec![failed] {
                                c.bump("crashes at the count in the hold");
                            }
                        }
                    }
                }
                // P7: the hold lasts final_hold_ms. A hold starts at its request's instant; a tick
                // drains it iff final_hold_ms has passed, with the hold's reason
                match (life, next) {
                    (Lifecycle::Running, Lifecycle::Holding { since, reason }) => {
                        prop_assert_eq!(since, now);
                        let i = match reason {
                            ExitReason::Ended => 0,
                            ExitReason::Signalled => 1,
                            ExitReason::Deadline => 2,
                            r => return Err(TestCaseError::fail(format!("a hold with reason {r:?}"))),
                        };
                        let mut h = holds.get();
                        h[i] += 1;
                        holds.set(h);
                    }
                    (Lifecycle::Holding { since, reason }, next) if ev == Event::Tick => {
                        if now - since >= t.final_hold_ms {
                            prop_assert_eq!(next, Lifecycle::Draining { since: now, reason });
                            c.bump("holds run out");
                        } else {
                            prop_assert_eq!(next, Lifecycle::Holding { since, reason });
                        }
                    }
                    _ => {}
                }
                seen_final |= d.next.programme_phase() == ProgrammePhase::Final;
                if n == 1 {
                    seen_exit = true;
                    c.bump("exits");
                }
                s = d.next;
            }
            Ok(())
        })
        .expect("property failed");
    let c = counts.into_inner();
    eprintln!(
        "cases checked: {}, holds entered (Ended, Signalled, Deadline): {:?}, counts: {:?}",
        checked.get(),
        holds.get(),
        c.0
    );
    assert_eq!(checked.get(), END_CASES);
    assert!(
        holds.get().iter().all(|&n| n > 0),
        "anti-vacuity: an end request never held: {:?}",
        holds.get()
    );
    for k in [
        "exits",
        "exits on a 410",
        "holds run out",
        "final ticks",
        "final beats",
        "fatal details checked",
        "frames in Final",
        "Ready bounds checked",
        "holds while stepped",
        "crashes in the hold",
        "crashes at the count in the hold",
        "hang hides in the hold",
    ] {
        assert!(
            c.0.get(k).copied().unwrap_or(0) > 0,
            "anti-vacuity: no {k}: {:?}",
            c.0
        );
    }
}
