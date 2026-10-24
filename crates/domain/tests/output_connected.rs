//! Owed 38 (F-011): properties of `OutputLink`'s Connected rule over random destinations, under
//! random thresholds. The ORACLE is written from the rule's text, never from the code under test: a
//! sink is Connected at its first growth at least `output_connected_after_ms` after its first growth,
//! if no error came first. Since Owed 87 (the owner, 2026-10-24, "A") nothing else ends a sink whose
//! bytes flow: no stall rule, so a silent sink, proving or Connected, waits for its own error however
//! long it is silent. Every other end of a sink is a failure: failure number f waits base · 2^(f - 1),
//! capped, and only Connected resets the count. Every property counts what it checked, prints it, and
//! fails on 0 (AGENTS.md §5).
use domain::config::{Thresholds, OUTPUT_SINK_TIMEOUT_MS};
use domain::output_link::{OutputLink, OutputState};
use domain::time::Ms;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use std::cell::Cell;

const CASES: u32 = 512;

/// The back-off definition, written independently of `backoff::delay`: the base, doubled once per
/// failure before this one, never above the cap.
fn wait(t: &Thresholds, failed_before: u32) -> Ms {
    let (base, cap) = (t.output_backoff_base_ms, t.output_backoff_cap_ms);
    (0..failed_before).fold(base.min(cap), |w, _| w.saturating_mul(2).min(cap))
}

/// The longest silence drawn: past the sink's timeout (the retired stall rule's 5 s) several times.
const QUIET_MAX: Ms = 6 * OUTPUT_SINK_TIMEOUT_MS;

/// A destination that refuses: first bytes `first_after` ms after the build, growth every `poll` ms
/// for `grows_for` ms (always short of the bound), then silence, and its error `error_after` ms after
/// its last growth. `repeat` re-sends each total once.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    first_after: Ms,
    poll: Ms,
    grows_for: Ms,
    error_after: Ms,
    repeat: bool,
}

/// The publish accepted at last: first bytes `first_after` ms after the build, growth every `poll` ms,
/// plus a growth exactly at the bound and 1 ms short of it when asked; then silence, and an error
/// `error_after` ms after it connects.
#[derive(Debug, Clone, Copy)]
struct Accepted {
    first_after: Ms,
    poll: Ms,
    at_the_bound: bool,
    one_short: bool,
    error_after: Ms,
}

#[derive(Debug, Clone)]
struct Case {
    t: Thresholds,
    tick: Ms,
    refusals: Vec<Refusal>,
    accepted: Accepted,
}

fn case() -> impl Strategy<Value = Case> {
    (300u64..9_000, 300u64..9_000, 50u64..1_500, 0u64..4_000, 1u64..=50)
        .prop_flat_map(|(n, timeout, base, cap_extra, tick)| {
            let t = Thresholds {
                output_connected_after_ms: n,
                output_connect_timeout_ms: timeout,
                output_backoff_base_ms: base,
                output_backoff_cap_ms: base + cap_extra,
                ..Thresholds::default()
            };
            let poll = 20u64..1_000;
            let first = 0u64..timeout.min(200);
            let refusal = (
                first.clone(),
                poll.clone(),
                0u64..n,
                0u64..QUIET_MAX,
                any::<bool>(),
            )
                .prop_map(|(first_after, poll, grows_for, error_after, repeat)| Refusal {
                    first_after,
                    poll,
                    grows_for,
                    error_after,
                    repeat,
                });
            let accepted = (first, poll, any::<bool>(), any::<bool>(), 0u64..QUIET_MAX).prop_map(
                |(first_after, poll, at_the_bound, one_short, error_after)| Accepted {
                    first_after,
                    poll,
                    at_the_bound,
                    one_short,
                    error_after,
                },
            );
            (
                Just(t),
                Just(tick),
                prop::collection::vec(refusal, 1..7),
                accepted,
            )
        })
        .prop_map(|(t, tick, refusals, accepted)| Case {
            t,
            tick,
            refusals,
            accepted,
        })
}

/// What the run saw, for the anti-vacuity lines.
#[derive(Default)]
struct Seen {
    cases: Cell<u32>,
    refused_by_error: Cell<u32>,
    /// Silences at least `OUTPUT_SINK_TIMEOUT_MS` long, where the retired stall rule ended the sink.
    silent_proving: Cell<u32>,
    silent_connected: Cell<u32>,
    silent_ticks: Cell<u64>,
    growth_checked: Cell<u64>,
    repeats: Cell<u64>,
    stale: Cell<u64>,
    connects: Cell<u32>,
    connects_at_the_bound: Cell<u32>,
    growth_1_ms_short: Cell<u32>,
    waits_at_the_cap: Cell<u32>,
    resets: Cell<u32>,
}

fn bump(c: &Cell<u32>) {
    c.set(c.get() + 1);
}

fn bump64(c: &Cell<u64>) {
    c.set(c.get() + 1);
}

/// The link's state must not move under the old sink's bytes or the bytes of a sink never built.
fn offer_stale(o: &mut OutputLink, now: Ms, t: &Thresholds, seen: &Seen) -> Result<(), TestCaseError> {
    let gen = o.generation();
    let before = o.clone();
    o.on_progress(now, gen - 1, u64::MAX / 2, t);
    o.on_progress(now, gen + 1, u64::MAX / 2, t);
    prop_assert_eq!(&*o, &before, "another generation's bytes moved the link");
    bump64(&seen.stale);
    Ok(())
}

/// Ticks on the grid from `now` until the next build, which the oracle expects at the first tick at
/// or after `until`. Returns the build's instant.
fn build_at(o: &mut OutputLink, mut now: Ms, until: Ms, c: &Case) -> Result<Ms, TestCaseError> {
    let due = until.div_ceil(c.tick) * c.tick;
    while now < due {
        now += c.tick;
        prop_assert_eq!(o.on_tick(now, &c.t), now == due, "built at {} (due {})", now, due);
    }
    Ok(due)
}

#[test]
fn refusals_never_connect_and_only_sustained_growth_resets_the_back_off() {
    let seen = Seen::default();
    let mut runner = TestRunner::new(Config {
        cases: CASES,
        ..Config::default()
    });
    runner
        .run(&case(), |c| {
            bump(&seen.cases);
            let t = &c.t;
            let n = t.output_connected_after_ms;
            let mut o = OutputLink::new();
            prop_assert!(o.on_tick(0, t), "the first tick builds");
            let mut built = 0;
            let mut failures: u32 = 0;
            for r in &c.refusals {
                prop_assert_eq!(
                    o.state(),
                    OutputState::Connecting {
                        attempt: failures,
                        since: built
                    },
                    "the build carries every failure before it"
                );
                // The sink's growth, in time order; the ticks on the grid between them.
                let first = built + r.first_after;
                let growth: Vec<Ms> = (0..)
                    .map(|k| first + k * r.poll)
                    .take_while(|&g| g <= first + r.grows_for)
                    .collect();
                let last = *growth.last().expect("a refusal grows at least once");
                prop_assert!(last - first < n, "setup: a refusal stays short of the bound");
                let mut now = built;
                for (k, &g) in growth.iter().enumerate() {
                    while now + c.tick < g {
                        now += c.tick;
                        prop_assert!(!o.on_tick(now, t));
                    }
                    let total = 476 + 1_000 * k as u64;
                    o.on_progress(g, o.generation(), total, t);
                    if r.repeat {
                        o.on_progress(g, o.generation(), total, t);
                        bump64(&seen.repeats);
                    }
                    offer_stale(&mut o, g, t, &seen)?;
                    prop_assert!(!o.is_connected(), "Connected {} ms into a refusal", g - first);
                    let proving = matches!(o.state(), OutputState::Proving { .. });
                    prop_assert!(proving, "a refusal's growth proves: {:?}", o.state());
                    bump64(&seen.growth_checked);
                }
                // Silence: no tick ends the proving sink, however long; only its error does (Owed 87).
                let ended_at = last + r.error_after;
                while now + c.tick < ended_at {
                    now += c.tick;
                    prop_assert!(!o.on_tick(now, t), "a silent proving sink ended at {}", now);
                    let proving = matches!(o.state(), OutputState::Proving { .. });
                    prop_assert!(proving, "{} ms into the silence: {:?}", now - last, o.state());
                    if now - last >= OUTPUT_SINK_TIMEOUT_MS {
                        bump64(&seen.silent_ticks);
                    }
                }
                if r.error_after >= OUTPUT_SINK_TIMEOUT_MS {
                    bump(&seen.silent_proving);
                }
                o.on_error(ended_at, o.generation(), t);
                bump(&seen.refused_by_error);
                prop_assert!(!o.is_connected());
                let w = wait(t, failures);
                failures += 1;
                prop_assert_eq!(
                    o.state(),
                    OutputState::Backoff {
                        attempt: failures,
                        until: ended_at + w
                    },
                    "refusal {} ended",
                    failures
                );
                if w == t.output_backoff_cap_ms {
                    bump(&seen.waits_at_the_cap);
                }
                offer_stale(&mut o, ended_at, t, &seen)?;
                built = build_at(&mut o, now, ended_at + w, &c)?;
            }
            // The accepted publish: growth until the oracle's bound, then silence and an error.
            let a = c.accepted;
            prop_assert_eq!(
                o.state(),
                OutputState::Connecting {
                    attempt: failures,
                    since: built
                }
            );
            let first = built + a.first_after;
            let mut growth: Vec<Ms> = (0..)
                .map(|k| first + k * a.poll)
                .take_while(|&g| g <= first + n + a.poll)
                .collect();
            growth.extend(a.at_the_bound.then_some(first + n));
            growth.extend(a.one_short.then_some(first + n - 1));
            growth.sort_unstable();
            growth.dedup();
            let mut now = built;
            let mut connected_at = None;
            for (k, &g) in growth.iter().enumerate() {
                while now + c.tick < g {
                    now += c.tick;
                    prop_assert!(!o.on_tick(now, t));
                }
                let total = 476 + 1_000 * k as u64;
                o.on_progress(g, o.generation(), total, t);
                offer_stale(&mut o, g, t, &seen)?;
                bump64(&seen.growth_checked);
                prop_assert_eq!(
                    o.is_connected(),
                    g - first >= n,
                    "{} ms after the first growth",
                    g - first
                );
                if g - first >= n {
                    connected_at = Some(g);
                    break;
                }
                if g - first == n - 1 {
                    bump(&seen.growth_1_ms_short);
                }
                prop_assert_eq!(
                    o.state(),
                    OutputState::Proving {
                        attempt: failures,
                        first_progress: first,
                        sent: total
                    },
                    "the failures still count while proving"
                );
            }
            let connected_at = connected_at.expect("setup: the accepted publish grows past the bound");
            bump(&seen.connects);
            if connected_at - first == n {
                bump(&seen.connects_at_the_bound);
            }
            // A later error from the Connected sink is failure 1 again: Connected reset the count. No
            // tick before it ends the silent sink (Owed 87).
            let e = connected_at + a.error_after;
            while now + c.tick < e {
                now += c.tick;
                prop_assert!(!o.on_tick(now, t), "a silent Connected sink swapped at {}", now);
                prop_assert!(o.is_connected(), "{} ms into the silence", now - connected_at);
                if now - connected_at >= OUTPUT_SINK_TIMEOUT_MS {
                    bump64(&seen.silent_ticks);
                }
            }
            if a.error_after >= OUTPUT_SINK_TIMEOUT_MS {
                bump(&seen.silent_connected);
            }
            o.on_error(e, o.generation(), t);
            prop_assert_eq!(
                o.state(),
                OutputState::Backoff {
                    attempt: 1,
                    until: e + wait(t, 0)
                },
                "after {} refusals and a connect",
                failures
            );
            bump(&seen.resets);
            Ok(())
        })
        .expect("property failed");
    let lines = [
        ("cases", u64::from(seen.cases.get())),
        (
            "refusals ended by an error",
            u64::from(seen.refused_by_error.get()),
        ),
        (
            "proving silences past the sink timeout",
            u64::from(seen.silent_proving.get()),
        ),
        (
            "connected silences past the sink timeout",
            u64::from(seen.silent_connected.get()),
        ),
        ("ticks past the sink timeout", seen.silent_ticks.get()),
        ("growth events checked", seen.growth_checked.get()),
        ("repeated totals", seen.repeats.get()),
        ("other-generation bytes offered", seen.stale.get()),
        ("connects", u64::from(seen.connects.get())),
        (
            "connects exactly at the bound",
            u64::from(seen.connects_at_the_bound.get()),
        ),
        (
            "growth 1 ms short of the bound",
            u64::from(seen.growth_1_ms_short.get()),
        ),
        ("waits at the cap", u64::from(seen.waits_at_the_cap.get())),
        ("resets by Connected", u64::from(seen.resets.get())),
    ];
    eprintln!("Owed 38 property: {lines:?}");
    assert_eq!(seen.cases.get(), CASES, "anti-vacuity: every configured case ran");
    for (name, k) in lines {
        assert!(k > 0, "anti-vacuity: never exercised: {name}");
    }
}
