//! Properties over random event streams. The ORACLE recomputes programme rules from raw frame times
//! the test itself recorded, and the hold and exit rules from `Thresholds` and the exit contract: it
//! never asks the code under test what the answer should be. Every property counts what it checked,
//! prints it, and fails on 0 (AGENTS.md §5).
use domain::config::Thresholds;
use domain::health::{BeatState, GraphicsStatus};
use domain::input_link::LossReason;
use domain::lifecycle::{ExitReason, Lifecycle};
use domain::programme::{FrameChoice, ProgrammePhase, SlateKind};
use domain::supervisor::{Desired, Effect, Event, SignalKind, Supervisor};
use domain::time::Ms;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use std::cell::Cell;

const CASES: u32 = 512;

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

fn end_stream() -> impl Strategy<Value = (Option<Ms>, Vec<(Ms, u8)>)> {
    // A deadline inside the stream, or none; dt 0..2 000 ms; ticks dominant, Fatal and SIGTERM rare,
    // so holds run to their end
    (
        prop::option::of(1u64..60_000),
        prop::collection::vec((0u64..2_000, 0u8..=19), 1..200),
    )
}

#[test]
fn exit_is_emitted_at_most_once_and_nothing_follows_it() {
    let t = Thresholds::default();
    let checked = Cell::new(0u32);
    let exits = Cell::new(0u32);
    let holds = Cell::new(0u64);
    let held_out = Cell::new(0u64);
    let final_ticks = Cell::new(0u64);
    let final_beats = Cell::new(0u64);
    let mut runner = TestRunner::new(Config {
        cases: CASES,
        ..Config::default()
    });
    runner
        .run(&end_stream(), |(deadline, steps)| {
            checked.set(checked.get() + 1);
            let mut s = booted(&t, deadline.unwrap_or(Ms::MAX / 2));
            let mut now = 0;
            let mut seen_exit = false;
            let mut seen_final = false;
            // The reason of the last Stopped beat sent, in effect order
            let mut last_final: Option<ExitReason> = None;
            for (dt, k) in steps {
                now += dt;
                let ev = match k {
                    0..=11 => Event::Tick,
                    12 | 13 => Event::HeartbeatReply(Desired::Ending),
                    14 => Event::Signal(SignalKind::Int),
                    15 | 16 => Event::OutputDrained,
                    17 => Event::Signal(SignalKind::Term),
                    18 => Event::HeartbeatReply(Desired::Live),
                    _ => Event::Fatal,
                };
                let d = s.decide(ev.clone(), now);
                let n = d.effects.iter().filter(|e| matches!(e, Effect::Exit(_))).count();
                if seen_exit {
                    prop_assert!(d.effects.is_empty() && d.frame.is_none());
                }
                prop_assert!(n <= 1);
                // P1′ across the end: a tick yields a frame iff on air before and after it
                let air = on_air(s.lifecycle()) && on_air(d.next.lifecycle());
                prop_assert_eq!(d.frame.is_some(), air && ev == Event::Tick, "frame on {:?}", ev);
                // P6: every beat carries an exit reason iff it is Stopped; each Exit follows a Stopped
                // beat whose reason is the exit's, with the contract's code
                for e in &d.effects {
                    match e {
                        Effect::SendHeartbeat(h) => {
                            prop_assert_eq!(h.exit.is_some(), h.state == BeatState::Stopped);
                            if let Some(x) = &h.exit {
                                last_final = Some(x.reason);
                                final_beats.set(final_beats.get() + 1);
                            }
                        }
                        Effect::Exit(code) => {
                            let Lifecycle::Exited(r) = d.next.lifecycle() else {
                                return Err(TestCaseError::fail("Exit without Exited"));
                            };
                            prop_assert_eq!(last_final, Some(r), "Exit({}) with no final beat for it", code);
                            prop_assert_eq!(*code, contract_code(r));
                        }
                        _ => {}
                    }
                }
                // P5: once Final, the programme stays Final, and nothing is rebuilt
                if seen_final {
                    prop_assert_eq!(d.next.programme_phase(), ProgrammePhase::Final);
                    for e in &d.effects {
                        prop_assert!(
                            !matches!(
                                e,
                                Effect::RebuildInput { .. }
                                    | Effect::SwapSink { .. }
                                    | Effect::RestartGraphics { .. }
                            ),
                            "a build after Final: {:?}",
                            e
                        );
                    }
                }
                if d.frame == Some(FrameChoice::Slate(SlateKind::Final)) {
                    final_ticks.set(final_ticks.get() + 1);
                }
                // P7: the hold lasts final_hold_ms. A hold starts at its request's instant; a tick
                // drains it iff final_hold_ms has passed, with the hold's reason
                match (s.lifecycle(), d.next.lifecycle()) {
                    (Lifecycle::Running, Lifecycle::Holding { since, .. }) => {
                        prop_assert_eq!(since, now);
                        holds.set(holds.get() + 1);
                    }
                    (Lifecycle::Holding { since, reason }, next) if ev == Event::Tick => {
                        if now - since >= t.final_hold_ms {
                            prop_assert_eq!(next, Lifecycle::Draining { since: now, reason });
                            held_out.set(held_out.get() + 1);
                        } else {
                            prop_assert_eq!(next, Lifecycle::Holding { since, reason });
                        }
                    }
                    _ => {}
                }
                seen_final |= d.next.programme_phase() == ProgrammePhase::Final;
                if n == 1 {
                    seen_exit = true;
                    exits.set(exits.get() + 1);
                }
                s = d.next;
            }
            Ok(())
        })
        .expect("property failed");
    eprintln!(
        "cases checked: {}, exits observed: {}, holds entered: {}, holds run out: {}, final ticks: {}, final beats: {}",
        checked.get(),
        exits.get(),
        holds.get(),
        held_out.get(),
        final_ticks.get(),
        final_beats.get()
    );
    assert_eq!(checked.get(), CASES);
    assert!(exits.get() > 0, "anti-vacuity: no case ever exited");
    assert!(holds.get() > 0, "anti-vacuity: no hold was entered");
    assert!(held_out.get() > 0, "anti-vacuity: no hold ran to its end");
    assert!(final_ticks.get() > 0, "anti-vacuity: no Final frame was checked");
    assert!(final_beats.get() > 0, "anti-vacuity: no final beat was checked");
}
