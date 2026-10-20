//! C11b: the in-place restart CEF's health asks for (owner ruling 2026-10-19 "B"; Owed 55, 56, 61).
//! The rule's table from its text, each lifecycle's answer, the second call, and a sweep over the
//! health values around each threshold. They live here, not inline, because a sweep prints the count
//! it checked and the domain's crate root forbids `clippy::print_stderr` (AGENTS.md §5).
//!
//! Expected values come from the rule's text and its sources, never from the code under test: the
//! exit code is sysexits' EX_TEMPFAIL (C11b's choice, written here independently of
//! `ExitReason::code`), the thresholds come from `Thresholds::default()` (pinned to owner ruling B and
//! F-013 in `config.rs`), and each final beat's text is written out.
use domain::cef_health::{CefHealth, RestartCause};
use domain::config::{OutputProfile, Thresholds, CHROMIUM_GPU_DEATHS_ABORT};
use domain::health::{BeatState, ExitDetail, ExitInfo, HealthReport};
use domain::lifecycle::{ExitReason, Lifecycle};
use domain::supervisor::{Decision, Desired, Effect, Event, SignalKind, Supervisor};
use domain::time::Ms;

const FAR: Ms = 10_000_000;
/// sysexits.h EX_TEMPFAIL ("temporary failure; the user is invited to retry"): not 0, so the host's
/// `on-failure` policy restarts the process (ADR 0004, ADR 0007 item 5); not 70 (fatal) and not 78
/// (refused: web's RELAY_EXIT_REFUSED), so the exit is its own.
const EX_TEMPFAIL: i32 = 75;

fn t() -> Thresholds {
    Thresholds::default()
}

/// C08 ticks on the frame period: 33 ms in both modes (ADR 0005, 30 fps).
fn period() -> Ms {
    OutputProfile::P720.frame_period_ms().round() as Ms
}

fn booted_with(th: Thresholds, now: Ms) -> Supervisor {
    let s = Supervisor::new(th).decide(Event::Tick, 0).next;
    let d = s.decide(
        Event::BootstrapOk {
            heartbeat_interval_ms: t().heartbeat_interval_ms,
            deadline_at: FAR,
        },
        now,
    );
    assert_eq!(d.next.lifecycle(), Lifecycle::Running, "setup: booted");
    d.next
}

/// Running, its first tick done: every pipeline built, the page loading as generation 1.
fn on_air_with(th: Thresholds) -> Supervisor {
    let s = booted_with(th, 0).decide(Event::Tick, 0).next;
    assert_eq!(s.graphics_generation(), 1, "setup: the first page is building");
    s
}

fn on_air() -> Supervisor {
    on_air_with(t())
}

fn health(unrecoverable: Option<u64>, leaked: usize, gpu: usize) -> CefHealth {
    CefHealth {
        unrecoverable,
        leaked_teardowns: leaked,
        gpu_process_deaths: gpu,
    }
}

fn stopped(h: &HealthReport) -> Option<&ExitInfo> {
    (h.state == BeatState::Stopped)
        .then_some(h.exit.as_ref())
        .flatten()
}

/// The decision is the in-place restart: the final beat (Stopped, `Restart`, `detail`) first, then
/// `Exit(EX_TEMPFAIL)`, nothing else; the supervisor has exited with that reason.
fn assert_restart(d: &Decision, detail: &str, at: &str) {
    assert_eq!(d.effects.len(), 2, "{at}: {:?}", d.effects);
    let Effect::SendHeartbeat(h) = &d.effects[0] else {
        panic!("{at}: the final beat comes first: {:?}", d.effects)
    };
    assert_eq!(
        stopped(h),
        Some(&ExitInfo {
            reason: ExitReason::Restart,
            detail: Some(ExitDetail::new(detail)),
        }),
        "{at}: {h:?}"
    );
    assert!(detail.chars().count() <= 100, "{at}: the wire's 100 characters");
    assert_eq!(d.effects[1], Effect::Exit(EX_TEMPFAIL), "{at}");
    assert_eq!(d.frame, None, "{at}");
    assert_eq!(d.next.lifecycle(), Lifecycle::Exited(ExitReason::Restart), "{at}");
}

fn assert_ignored(s: &Supervisor, d: &Decision, at: &str) {
    assert!(d.effects.is_empty() && d.frame.is_none(), "{at}: {:?}", d.effects);
    assert_eq!(&d.next, s, "{at}: changed the supervisor");
}

// ---- the rule ----

// The empty case: a healthy process (the adapter's `CefHealth::default()`) asks for nothing, and
// telling a Running supervisor so changes nothing at all.
#[test]
fn a_healthy_process_asks_for_nothing() {
    let th = t();
    assert_eq!(CefHealth::default(), health(None, 0, 0));
    assert_eq!(CefHealth::default().restart_cause(&th), None);
    let s = on_air();
    let d = s.decide(Event::CefHealth(CefHealth::default()), 100);
    assert_ignored(&s, &d, "healthy");
}

// Owner ruling B (Owed 56): any dead zygote ends CEF for the process, so the process restarts in
// place at once: its final beat names the restart, then it exits for the host to start it again.
#[test]
fn a_dead_zygote_restarts_in_place_its_final_beat_first() {
    let th = t();
    assert_eq!(
        health(Some(1), 0, 0).restart_cause(&th),
        Some(RestartCause::ZygoteGone { gen: 1 })
    );
    let s = on_air();
    let d = s.decide(Event::CefHealth(health(Some(1), 0, 0)), 100);
    assert_restart(&d, "restart in place: a CEF zygote is gone (gen 1)", "zygote");
}

// The verdict is the PROCESS's, not a generation's: CEF is dead for every later page, so a verdict
// that names an older page than the current one, no page (0), or one the domain never built, still
// restarts. Ignoring a "stale" one, as the stub's "rule out a stale gen" suggests, would keep a dead
// CEF on air until the GPU process's next death aborts it.
#[test]
fn a_zygote_verdict_restarts_whatever_generation_it_names() {
    // Generation 3: two crashes, each restarted after its back-off.
    let mut s = on_air();
    let mut now = 100;
    for _ in 0..2 {
        let gen = s.graphics_generation();
        s = s.decide(Event::CefCrashed { gen }, now).next;
        while s.graphics_generation() == gen {
            now += period();
            s = s.decide(Event::Tick, now).next;
        }
    }
    assert_eq!(s.graphics_generation(), 3, "setup");
    for gen in [0, 1, 2, 3, 99] {
        let d = s.decide(Event::CefHealth(health(Some(gen), 0, 0)), now);
        let text = format!("restart in place: a CEF zygote is gone (gen {gen})");
        assert_restart(&d, &text, &format!("verdict gen {gen}"));
    }
}

// Owner ruling B (Owed 55): leaked teardowns are a LIVE count. Below the count nothing happens; at it
// the process restarts in place. A count that falls back below (a teardown returned after all) is
// nothing again.
#[test]
fn leaked_teardowns_restart_at_the_live_count() {
    let th = t();
    let n = th.cef_leaks_to_restart;
    let s = on_air();
    for live in 0..n {
        assert_eq!(health(None, live, 0).restart_cause(&th), None, "{live}");
        let d = s.decide(Event::CefHealth(health(None, live, 0)), 100);
        assert_ignored(&s, &d, &format!("{live} leaked"));
    }
    // Up to the count and back down: each report alone decides, so 2 after 3-minus-one is nothing.
    let mut s2 = s.clone();
    for live in [1, 2, 1, 2] {
        let d = s2.decide(Event::CefHealth(health(None, live, 0)), 100);
        assert_ignored(&s2, &d, &format!("{live} leaked, counted back down"));
        s2 = d.next;
    }
    for live in [n, n + 1] {
        assert_eq!(
            health(None, live, 0).restart_cause(&th),
            Some(RestartCause::LeakedTeardowns { live })
        );
        let d = s.decide(Event::CefHealth(health(None, live, 0)), 100);
        let text = format!("restart in place: {live} CEF teardowns leaked");
        assert_restart(&d, &text, &format!("{live} leaked"));
    }
}

// F-013 / Owed 61: Chromium aborts at the 6th GPU-process death of the process's life, so the process
// restarts in place at `cef_gpu_deaths_to_restart`, short of it.
#[test]
fn gpu_process_deaths_restart_short_of_chromiums_abort() {
    let th = t();
    let n = th.cef_gpu_deaths_to_restart;
    assert!(n < CHROMIUM_GPU_DEATHS_ABORT, "setup");
    let s = on_air();
    for deaths in 0..n {
        assert_eq!(health(None, 0, deaths).restart_cause(&th), None, "{deaths}");
        let d = s.decide(Event::CefHealth(health(None, 0, deaths)), 100);
        assert_ignored(&s, &d, &format!("{deaths} GPU deaths"));
    }
    for deaths in [n, CHROMIUM_GPU_DEATHS_ABORT - 1] {
        assert_eq!(
            health(None, 0, deaths).restart_cause(&th),
            Some(RestartCause::GpuProcessDeaths { count: deaths })
        );
        let d = s.decide(Event::CefHealth(health(None, 0, deaths)), 100);
        let text = format!("restart in place: {deaths} CEF GPU-process deaths");
        assert_restart(&d, &text, &format!("{deaths} GPU deaths"));
    }
}

// Every trigger at once: the beat names the most urgent. A dead zygote first (the next GPU-process
// death may abort the process), then the GPU deaths (each brings the abort closer), then the leaks
// (memory only).
#[test]
fn the_final_beat_names_the_most_urgent_cause() {
    let th = t();
    let (l, g) = (th.cef_leaks_to_restart, th.cef_gpu_deaths_to_restart);
    let cases = [
        (health(Some(2), l, g), RestartCause::ZygoteGone { gen: 2 }),
        (health(Some(2), 0, g), RestartCause::ZygoteGone { gen: 2 }),
        (health(Some(2), l, 0), RestartCause::ZygoteGone { gen: 2 }),
        (health(None, l, g), RestartCause::GpuProcessDeaths { count: g }),
    ];
    for (h, want) in cases {
        assert_eq!(h.restart_cause(&th), Some(want), "{h:?}");
    }
}

// Each count reads its own threshold. The defaults share 3 between the leak count, `cef_fail_count`,
// `kick_alert_count` and `ladder_overruns_to_step`, so a rule that read another field would keep its
// answer under them; here every one differs.
#[test]
fn each_count_reads_its_own_threshold() {
    let th = Thresholds {
        cef_leaks_to_restart: 5,
        cef_gpu_deaths_to_restart: 2,
        cef_fail_count: 4,
        kick_alert_count: 6,
        ladder_overruns_to_step: 7,
        ..t()
    };
    let s = on_air_with(th.clone());
    for (h, restarts) in [
        (health(None, 4, 0), false),
        (health(None, 5, 0), true),
        (health(None, 0, 1), false),
        (health(None, 0, 2), true),
    ] {
        assert_eq!(h.restart_cause(&th).is_some(), restarts, "{h:?}");
        let d = s.decide(Event::CefHealth(h), 100);
        assert_eq!(
            d.next.lifecycle() == Lifecycle::Exited(ExitReason::Restart),
            restarts,
            "{h:?}: {:?}",
            d.effects
        );
    }
}

// ---- the lifecycle's answer ----

// The second call: once the restart is asked for, the process has exited. A later report (a 4th
// leak, the verdict again) and every later event change nothing, so the restart is asked for once.
#[test]
fn the_restart_is_asked_for_once() {
    let th = t();
    let d = on_air().decide(Event::CefHealth(health(None, th.cef_leaks_to_restart, 0)), 100);
    assert_eq!(
        d.next.lifecycle(),
        Lifecycle::Exited(ExitReason::Restart),
        "setup"
    );
    let s = d.next;
    let later = [
        Event::CefHealth(health(None, th.cef_leaks_to_restart + 1, 0)),
        Event::CefHealth(health(Some(1), 0, 0)),
        Event::CefHealth(CefHealth::default()),
        Event::Tick,
        Event::Signal(SignalKind::Term),
        Event::HeartbeatReply(Desired::Ending),
    ];
    let mut checked = 0;
    for (i, e) in later.into_iter().enumerate() {
        let d = s.decide(e, 200 + i as Ms * 100);
        assert_ignored(&s, &d, &format!("after the restart, event {i}"));
        checked += 1;
    }
    assert_eq!(checked, 6);
}

/// Every effect of `events` in order, and the supervisor after them.
fn run(mut s: Supervisor, events: Vec<(Ms, Event)>) -> (Supervisor, Vec<Effect>) {
    let mut fx = Vec::new();
    for (at, e) in events {
        let d = s.decide(e, at);
        fx.extend(d.effects);
        s = d.next;
    }
    (s, fx)
}

fn exits(fx: &[Effect]) -> Vec<i32> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Exit(c) => Some(*c),
            _ => None,
        })
        .collect()
}

fn final_reasons(fx: &[Effect]) -> Vec<ExitReason> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::SendHeartbeat(h) => stopped(h).map(|x| x.reason),
            _ => None,
        })
        .collect()
}

// The final hold and the drain are already ending the process (exit 0, which the host does not
// restart), so a restart there would cut the Final card short and boot a process for a session that
// is over: each report is ignored, and the hold ends as it began. Every trigger, in the hold and in
// the drain.
#[test]
fn the_final_hold_and_the_drain_end_the_process_their_own_way() {
    let th = t();
    let triggers = [
        health(Some(1), 0, 0),
        health(None, th.cef_leaks_to_restart, 0),
        health(None, 0, th.cef_gpu_deaths_to_restart),
    ];
    let mut checked = 0;
    for h in triggers {
        // The hold: an Ending reply at 100, the report 50 ms in, then ticks to the end.
        let (s, mut fx) = run(on_air(), vec![(100, Event::HeartbeatReply(Desired::Ending))]);
        assert!(matches!(s.lifecycle(), Lifecycle::Holding { .. }), "setup");
        let d = s.decide(Event::CefHealth(h), 150);
        assert_ignored(&s, &d, &format!("hold: {h:?}"));
        let mut s = d.next;
        let mut now = 150;
        while exits(&fx).is_empty() {
            now += period();
            assert!(
                now < 100 + th.final_hold_ms + th.drain_timeout_ms + 1_000,
                "no end"
            );
            let d = s.decide(Event::Tick, now);
            fx.extend(d.effects);
            s = d.next;
        }
        assert_eq!(exits(&fx), vec![0], "hold: {h:?}");
        assert_eq!(final_reasons(&fx), vec![ExitReason::Ended], "hold: {h:?}");
        checked += 1;

        // The drain: a SIGTERM at 100, the report 50 ms in, then the drain's end.
        let (s, _) = run(on_air(), vec![(100, Event::Signal(SignalKind::Term))]);
        assert!(matches!(s.lifecycle(), Lifecycle::Draining { .. }), "setup");
        let d = s.decide(Event::CefHealth(h), 150);
        assert_ignored(&s, &d, &format!("drain: {h:?}"));
        let d = d.next.decide(Event::OutputDrained, 200);
        assert_eq!(exits(&d.effects), vec![0], "drain: {h:?}");
        assert_eq!(d.next.lifecycle(), Lifecycle::Exited(ExitReason::Signalled));
        checked += 1;
    }
    eprintln!("hold and drain cases checked: {checked}");
    assert_eq!(checked, 6);
}

// Nothing of CEF runs before the session (graphics build on the first Running tick), so a report
// while booting cannot be about a page of this session: ignored, in each booting state.
#[test]
fn booting_ignores_every_report() {
    let fresh = Supervisor::new(t());
    let in_flight = fresh.decide(Event::Tick, 0).next;
    let backoff = in_flight.decide(Event::BootstrapFailed, 10).next;
    let mut checked = 0;
    for s in [fresh, in_flight, backoff] {
        assert!(matches!(s.lifecycle(), Lifecycle::Booting { .. }), "setup");
        for h in [health(Some(0), 0, 0), health(None, 9, 9)] {
            let d = s.decide(Event::CefHealth(h), 20);
            assert_ignored(&s, &d, &format!("{:?}: {h:?}", s.lifecycle()));
            checked += 1;
        }
    }
    assert_eq!(checked, 6);
}

// ---- the sweep ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setup {
    Booting,
    Running,
    Holding,
    Draining,
    Exited,
}

fn setup(st: Setup, th: &Thresholds) -> Supervisor {
    let s = match st {
        Setup::Booting => Supervisor::new(th.clone()).decide(Event::Tick, 0).next,
        Setup::Running => on_air_with(th.clone()),
        Setup::Holding => {
            on_air_with(th.clone())
                .decide(Event::HeartbeatReply(Desired::Ending), 50)
                .next
        }
        Setup::Draining => {
            on_air_with(th.clone())
                .decide(Event::Signal(SignalKind::Term), 50)
                .next
        }
        Setup::Exited => {
            on_air_with(th.clone())
                .decide(Event::Fatal { detail: None }, 50)
                .next
        }
    };
    let ok = matches!(
        (st, s.lifecycle()),
        (Setup::Booting, Lifecycle::Booting { .. })
            | (Setup::Running, Lifecycle::Running)
            | (Setup::Holding, Lifecycle::Holding { .. })
            | (Setup::Draining, Lifecycle::Draining { .. })
            | (Setup::Exited, Lifecycle::Exited(_))
    );
    assert!(ok, "setup {st:?}: {:?}", s.lifecycle());
    s
}

/// The rule, written from owner ruling B and F-013's text, not from `restart_cause`: a Running process
/// restarts in place when any zygote is gone, or the live leaks reach their count, or the GPU-process
/// deaths reach theirs; the beat names the zygote first, then the GPU deaths, then the leaks.
fn oracle(st: Setup, h: &CefHealth, th: &Thresholds) -> Option<String> {
    if st != Setup::Running {
        return None;
    }
    if let Some(gen) = h.unrecoverable {
        return Some(format!("restart in place: a CEF zygote is gone (gen {gen})"));
    }
    if h.gpu_process_deaths >= th.cef_gpu_deaths_to_restart {
        return Some(format!(
            "restart in place: {} CEF GPU-process deaths",
            h.gpu_process_deaths
        ));
    }
    if h.leaked_teardowns >= th.cef_leaks_to_restart {
        return Some(format!(
            "restart in place: {} CEF teardowns leaked",
            h.leaked_teardowns
        ));
    }
    None
}

// Every lifecycle against every report around each threshold (each count from 0 to two past its
// threshold, the verdict absent or naming several generations), under the defaults and under
// thresholds that differ from each other and from the defaults.
#[test]
fn every_report_in_every_lifecycle_follows_the_rule() {
    let distinct = Thresholds {
        cef_leaks_to_restart: 2,
        cef_gpu_deaths_to_restart: 5,
        ..t()
    };
    let (mut checked, mut restarts, mut ignored, mut under_distinct) = (0, 0, 0, 0);
    for th in [t(), distinct] {
        for st in [
            Setup::Booting,
            Setup::Running,
            Setup::Holding,
            Setup::Draining,
            Setup::Exited,
        ] {
            let s = setup(st, &th);
            for unrecoverable in [None, Some(0), Some(1), Some(7)] {
                for leaked in 0..=th.cef_leaks_to_restart + 2 {
                    for gpu in 0..=th.cef_gpu_deaths_to_restart + 2 {
                        let h = health(unrecoverable, leaked, gpu);
                        let d = s.decide(Event::CefHealth(h), 100);
                        let at = format!("{st:?} {h:?} under {th:?}");
                        match oracle(st, &h, &th) {
                            Some(text) => {
                                assert_restart(&d, &text, &at);
                                restarts += 1;
                            }
                            None => {
                                assert_ignored(&s, &d, &at);
                                ignored += 1;
                            }
                        }
                        checked += 1;
                        under_distinct += usize::from(th != t());
                    }
                }
            }
        }
    }
    eprintln!(
        "health reports checked: {checked} ({restarts} restart, {ignored} ignored; {under_distinct} under distinct thresholds)"
    );
    assert!(
        restarts > 0 && ignored > 0 && under_distinct > 0,
        "a vacuous sweep"
    );
    assert_eq!(checked, 5 * 4 * (6 * 7 + 5 * 8), "every report, once");
}
