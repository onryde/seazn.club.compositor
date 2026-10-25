//! C11b: the in-place restart CEF's health asks for (owner ruling 2026-10-19 "B"; Owed 55, 56, 61),
//! and the reload a GPU-process death below it asks for (Owed 96; owner ruling 2026-10-25 "A").
//! The rule's table from its text, each lifecycle's answer, the second call, and a sweep over the
//! health values around each threshold. They live here, not inline, because a sweep prints the count
//! it checked and the domain's crate root forbids `clippy::print_stderr` (AGENTS.md §5).
//!
//! Expected values come from the rule's text and its sources, never from the code under test: the
//! exit code is sysexits' EX_TEMPFAIL (C11b's choice, written here independently of
//! `ExitReason::code`), the thresholds come from `Thresholds::default()` (pinned to owner ruling B and
//! F-013 in `config.rs`), and each final beat's text is written out.
use domain::cef_health::{page_faults_restart, CefHealth, RestartCause};
use domain::config::{OutputProfile, Thresholds, CHROMIUM_GPU_DEATHS_ABORT};
use domain::graphics::HideReason;
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

/// The decision is the reload (Owed 96): the one effect builds the NEXT graphics generation after
/// `s`'s, naming the process's GPU-process death count, and the supervisor is still Running.
fn assert_reload(s: &Supervisor, d: &Decision, deaths: usize, at: &str) {
    let gen = s.graphics_generation() + 1;
    assert_eq!(
        d.effects,
        vec![Effect::ReloadGraphics {
            gen,
            gpu_process_deaths: deaths
        }],
        "{at}"
    );
    assert_eq!(d.frame, None, "{at}");
    assert_eq!(d.next.graphics_generation(), gen, "{at}");
    assert_eq!(d.next.lifecycle(), Lifecycle::Running, "{at}");
}

fn reloads(fx: &[Effect]) -> Vec<(u64, usize)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::ReloadGraphics {
                gen,
                gpu_process_deaths,
            } => Some((*gen, *gpu_process_deaths)),
            _ => None,
        })
        .collect()
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
// restarts in place at `cef_gpu_deaths_to_restart`, short of it. Below it a death asks for no restart,
// and reloads the page (Owed 96): told so first, a Running supervisor reloads.
#[test]
fn gpu_process_deaths_restart_short_of_chromiums_abort() {
    let th = t();
    let n = th.cef_gpu_deaths_to_restart;
    assert!(n < CHROMIUM_GPU_DEATHS_ABORT, "setup");
    let s = on_air();
    for deaths in 0..n {
        assert_eq!(health(None, 0, deaths).restart_cause(&th), None, "{deaths}");
        let d = s.decide(Event::CefHealth(health(None, 0, deaths)), 100);
        let at = format!("{deaths} GPU deaths");
        if deaths == 0 {
            assert_ignored(&s, &d, &at);
        } else {
            assert_reload(&s, &d, deaths, &at);
        }
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
// (memory, and CPU while a held page keeps painting: Owed 93).
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
    // A death below the count too: its reload would rebuild in the hold, where nothing is rebuilt.
    let triggers = [
        health(Some(1), 0, 0),
        health(None, th.cef_leaks_to_restart, 0),
        health(None, 0, th.cef_gpu_deaths_to_restart),
        health(None, 0, 1),
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
    assert_eq!(checked, 8);
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

#[derive(Debug)]
enum Answer {
    Restart(String),
    Reload,
    Ignored,
}

/// The rule, written from owner rulings B and A and F-013's text, not from `restart_cause`: a Running
/// process restarts in place when any zygote is gone, or the live leaks reach their count, or the
/// GPU-process deaths reach theirs; the beat names the zygote first, then the GPU deaths, then the
/// leaks. Short of every restart, a GPU-process death the supervisor has not seen reloads the page:
/// every sweep report goes to a fresh setup that has seen none.
fn oracle(st: Setup, h: &CefHealth, th: &Thresholds) -> Answer {
    if st != Setup::Running {
        return Answer::Ignored;
    }
    if let Some(gen) = h.unrecoverable {
        return Answer::Restart(format!("restart in place: a CEF zygote is gone (gen {gen})"));
    }
    if h.gpu_process_deaths >= th.cef_gpu_deaths_to_restart {
        return Answer::Restart(format!(
            "restart in place: {} CEF GPU-process deaths",
            h.gpu_process_deaths
        ));
    }
    if h.leaked_teardowns >= th.cef_leaks_to_restart {
        return Answer::Restart(format!(
            "restart in place: {} CEF teardowns leaked",
            h.leaked_teardowns
        ));
    }
    if h.gpu_process_deaths > 0 {
        return Answer::Reload;
    }
    Answer::Ignored
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
    let (mut checked, mut restarts, mut reloaded, mut ignored, mut under_distinct) = (0, 0, 0, 0, 0);
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
                            Answer::Restart(text) => {
                                assert_restart(&d, &text, &at);
                                restarts += 1;
                            }
                            Answer::Reload => {
                                assert_reload(&s, &d, gpu, &at);
                                reloaded += 1;
                            }
                            Answer::Ignored => {
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
        "health reports checked: {checked} ({restarts} restart, {reloaded} reload, {ignored} ignored; {under_distinct} under distinct thresholds)"
    );
    assert!(
        restarts > 0 && reloaded > 0 && ignored > 0 && under_distinct > 0,
        "a vacuous sweep"
    );
    // The reloads, counted from the grid: Running, no verdict, leaks below their count, GPU deaths
    // 1 to one short of theirs. Defaults: 3 leak counts by 2 death counts; distinct: 2 by 4.
    assert_eq!(reloaded, 3 * 2 + 2 * 4, "every reload, once");
    // 5 lifecycles, 4 verdicts; the defaults' leaks 0..=5 by GPU deaths 0..=5, the distinct ones'
    // 0..=4 by 0..=7.
    assert_eq!(checked, 5 * 4 * (6 * 6 + 5 * 8), "every report, once");
}

// ---- Owed 96: the reload after a GPU-process death (owner ruling 2026-10-25 "A") ----

/// Running, generation 1 Ready at 100.
fn ready_with(th: Thresholds) -> Supervisor {
    let s = on_air_with(th).decide(Event::CefReady { gen: 1 }, 100).next;
    let s = s.decide(Event::CefPainted { gen: 1 }, 100).next;
    assert_eq!(
        s.health(100).graphics,
        domain::health::GraphicsStatus::Ready,
        "setup"
    );
    s
}

// The sequence, the brief's three rules: each death the process counts below the in-place restart's
// count reloads the page exactly once (the next generation, naming the count); the same count read
// again (the adapter's health on the next tick) reloads nothing; and the death AT the count restarts
// the process and does not also reload. Under the defaults and under a count that differs from them,
// so the number of reloads before the restart is the threshold's, read from the declaration.
#[test]
fn each_gpu_death_below_the_count_reloads_once_and_the_count_restarts_without_one() {
    let distinct = Thresholds {
        cef_gpu_deaths_to_restart: 5,
        ..t()
    };
    let mut checked = 0;
    for th in [t(), distinct] {
        let n = th.cef_gpu_deaths_to_restart;
        let mut s = ready_with(th.clone());
        let mut now = 200;
        let mut fx = Vec::new();
        for deaths in 1..n {
            for read in 0..3 {
                now += period();
                let d = s.decide(Event::CefHealth(health(None, 0, deaths)), now);
                let at = format!("{deaths} deaths, read {read}, under {n}");
                if read == 0 {
                    assert_reload(&s, &d, deaths, &at);
                } else {
                    assert_ignored(&s, &d, &at);
                }
                fx.extend(d.effects.clone());
                s = d.next;
                checked += 1;
            }
            // A ready of the reload between deaths: the next death reloads a Ready page again.
            let gen = s.graphics_generation();
            s = s.decide(Event::CefReady { gen }, now + 1).next;
            s = s.decide(Event::CefPainted { gen }, now + 1).next;
        }
        let want: Vec<(u64, usize)> = (1..n).map(|k| (1 + k as u64, k)).collect();
        assert_eq!(reloads(&fx), want, "one reload a death, under {n}");
        let d = s.decide(Event::CefHealth(health(None, 0, n)), now + period());
        let text = format!("restart in place: {n} CEF GPU-process deaths");
        assert_restart(&d, &text, &format!("the {n}th death"));
        assert_eq!(reloads(&d.effects), vec![], "the restart does not also reload");
        checked += 1;
    }
    eprintln!("GPU-death reports checked: {checked}");
    // Defaults: deaths 1..3, 3 reads each, then the restart; distinct: deaths 1..5.
    assert_eq!(checked, (2 * 3 + 1) + (4 * 3 + 1));
}

// Two deaths counted in one report (both inside one child poll): one reload, naming the count. A
// count read lower than one seen before (it never falls: the adapter counts over the process's life)
// reloads nothing.
#[test]
fn one_report_reloads_once_however_many_deaths_it_counts() {
    let th = Thresholds {
        cef_gpu_deaths_to_restart: 5,
        ..t()
    };
    let s = ready_with(th);
    let d = s.decide(Event::CefHealth(health(None, 0, 3)), 200);
    assert_reload(&s, &d, 3, "two deaths at once, after none");
    let s = d.next;
    for lower in [0, 1, 2, 3] {
        let d = s.decide(Event::CefHealth(health(None, 0, lower)), 300);
        assert_ignored(&s, &d, &format!("{lower} after 3"));
    }
    let d = s.decide(Event::CefHealth(health(None, 0, 4)), 400);
    assert_reload(&s, &d, 4, "the next death");
}

// A page backing off has nothing to reload: its back-off's restart builds after the death anyway, and
// a reload would cut the back-off short. The death is seen all the same, so once that restart is
// Ready the same count reloads nothing; the next death does.
#[test]
fn a_death_while_backing_off_reloads_nothing_then_or_later() {
    let s = ready_with(t());
    let d = s.decide(Event::CefCrashed { gen: 1 }, 200);
    let s = d.next;
    let d = s.decide(Event::CefHealth(health(None, 0, 1)), 210);
    assert_eq!(reloads(&d.effects), vec![], "backing off: {:?}", d.effects);
    assert_eq!(d.next.graphics_generation(), 1, "no build");
    let mut s = d.next;
    let mut now = 210;
    while s.graphics_generation() == 1 {
        now += period();
        assert!(
            now < 210 + t().cef_backoff_cap_ms + 1_000,
            "the back-off never ended"
        );
        s = s.decide(Event::Tick, now).next;
    }
    s = s.decide(Event::CefReady { gen: 2 }, now + 1).next;
    s = s.decide(Event::CefPainted { gen: 2 }, now + 1).next;
    let d = s.decide(Event::CefHealth(health(None, 0, 1)), now + 2);
    assert_ignored(&s, &d, "the death seen while backing off");
    let d = s.decide(Event::CefHealth(health(None, 0, 2)), now + 3);
    assert_reload(&s, &d, 2, "the next death");
}

// The reload is no failure and keeps the bound: a Ready page reloaded whose reload never gets ready
// comes off air at its own bound, its last proof plus `cef_hang_after_ms`, named `Reload` and its
// generation, once; and a reload that gets ready inside the bound never hides.
#[test]
fn a_reload_that_never_readies_hides_its_page_at_the_bound() {
    let th = t();
    let mut checked = 0;
    for readies in [false, true] {
        let s = ready_with(th.clone());
        let d = s.decide(Event::CefHealth(health(None, 0, 1)), 200);
        assert_reload(&s, &d, 1, "setup");
        let mut s = d.next;
        let mut now = 200;
        let mut hides = Vec::new();
        while now < 100 + th.cef_hang_after_ms + 2 * period() {
            now += period();
            if readies && s.health(now).graphics != domain::health::GraphicsStatus::Ready && now >= 3_000 {
                s = s.decide(Event::CefReady { gen: 2 }, now).next;
                s = s.decide(Event::CefPainted { gen: 2 }, now).next;
            }
            let d = s.decide(Event::Tick, now);
            for e in &d.effects {
                if let Effect::HideOverlay { reason, gen } = e {
                    hides.push((now, *reason, *gen));
                }
            }
            s = d.next;
        }
        if readies {
            assert_eq!(hides, vec![], "a reload ready inside the bound");
        } else {
            assert_eq!(hides.len(), 1, "{hides:?}");
            let (at, reason, gen) = hides[0];
            assert_eq!((reason, gen), (HideReason::Reload, 1));
            let bound = 100 + th.cef_hang_after_ms;
            assert!(at >= bound && at < bound + period(), "{at} vs {bound}");
        }
        checked += 1;
    }
    assert_eq!(checked, 2);
}

// ---- Owed 99: the blank page (option B; owner 2026-10-25 "Apply rec", relayed by the coordinator) ----
//
// A Ready page that has shown no frame of its own `cef_hang_after_ms` after its ready is a counted page
// fault: counted with the GPU-process deaths toward `cef_gpu_deaths_to_restart`, it reloads the page
// below that count and restarts the process in place at it, reloading nothing. Every instant below is
// the harness's own: its ready, plus the declared bound, on the frame period's tick grid.

/// How the harness's pages behave, by generation: whether a build signals ready (`READY_AFTER` after
/// the decision that built it), whether C11 reports its first frame at that ready, and whether it beats
/// every `cef_alive_every_ms` while Ready.
#[derive(Clone, Copy)]
struct Pages {
    ready: fn(u64) -> bool,
    paints: fn(u64) -> bool,
    beats: fn(u64) -> bool,
}

const READY_AFTER: Ms = 500;

fn every(_: u64) -> bool {
    true
}

fn never(_: u64) -> bool {
    false
}

/// What a run saw, each with its instant.
#[derive(Debug, Default)]
struct Seen {
    readies: Vec<(u64, Ms)>,
    /// `ReloadGraphicsNoFrames`: (instant, the generation built, the blank pages counted).
    blank_reloads: Vec<(Ms, u64, usize)>,
    /// `ReloadGraphics`: (instant, the generation built, the deaths counted).
    death_reloads: Vec<(Ms, u64, usize)>,
    hides: Vec<(Ms, HideReason, u64)>,
    /// Each final beat: (instant, its reason, its text).
    finals: Vec<(Ms, ExitReason, Option<String>)>,
    exits: Vec<(Ms, i32)>,
}

/// The first tick of the grid `from + k · period()` at or after `at`.
fn first_tick(from: Ms, at: Ms) -> Ms {
    from + at.saturating_sub(from).div_ceil(period()) * period()
}

/// Drives `s` from `from` to `until` on the frame period. On each tick: the current page's ready (and
/// its first frame, if it paints), then its beat, then `extra`'s events due by then, each at its own
/// instant, then the tick. Stops at the exit.
fn drive(
    mut s: Supervisor,
    from: Ms,
    until: Ms,
    p: Pages,
    extra: Vec<(Ms, Event)>,
    th: &Thresholds,
) -> (Supervisor, Seen) {
    let mut seen = Seen::default();
    let mut extra = extra.into_iter().peekable();
    let mut gen = s.graphics_generation();
    let mut built = from;
    let mut readied = s.health(from).graphics != domain::health::GraphicsStatus::Starting;
    let mut next_beat = from + th.cef_alive_every_ms;
    let mut now = from;
    while now + period() <= until {
        now += period();
        let mut events = Vec::new();
        let status = s.health(now).graphics;
        if !readied
            && now >= built + READY_AFTER
            && (p.ready)(gen)
            && status == domain::health::GraphicsStatus::Starting
        {
            events.push((now, Event::CefReady { gen }));
            seen.readies.push((gen, now));
            if (p.paints)(gen) {
                events.push((now, Event::CefPainted { gen }));
            }
            readied = true;
            next_beat = now + th.cef_alive_every_ms;
        } else if readied && (p.beats)(gen) && now >= next_beat {
            events.push((now, Event::CefAlive { gen }));
            next_beat += th.cef_alive_every_ms;
        }
        while let Some((at, _)) = extra.peek() {
            if *at > now {
                break;
            }
            events.push(extra.next().expect("peeked"));
        }
        events.push((now, Event::Tick));
        for (at, e) in events {
            let d = s.decide(e, at);
            for e in d.effects {
                match e {
                    Effect::ReloadGraphicsNoFrames { gen, blank_pages } => {
                        seen.blank_reloads.push((at, gen, blank_pages))
                    }
                    Effect::ReloadGraphics {
                        gen,
                        gpu_process_deaths,
                    } => seen.death_reloads.push((at, gen, gpu_process_deaths)),
                    Effect::HideOverlay { reason, gen } => seen.hides.push((at, reason, gen)),
                    Effect::SendHeartbeat(h) => {
                        if let Some(x) = stopped(&h) {
                            seen.finals.push((
                                at,
                                x.reason,
                                x.detail.as_ref().map(|d| d.as_str().to_owned()),
                            ));
                        }
                    }
                    Effect::Exit(c) => seen.exits.push((at, c)),
                    _ => {}
                }
            }
            s = d.next;
            if s.graphics_generation() != gen {
                gen = s.graphics_generation();
                built = at;
                readied = false;
            }
            if matches!(s.lifecycle(), Lifecycle::Exited(_)) {
                return (s, seen);
            }
        }
    }
    (s, seen)
}

/// The in-place restart's text for `blanks` blank pages and `deaths` GPU-process deaths, from the
/// brief (Owed 99) and C11b's wire limit.
fn blank_text(blanks: usize, deaths: usize) -> String {
    format!("restart in place: {blanks} blank CEF pages, {deaths} GPU-process deaths")
}

/// The thresholds each Owed 99 sequence runs under: the defaults, and one whose bound and count both
/// differ from them, so every instant and count below is the declaration's.
fn blank_thresholds() -> [Thresholds; 2] {
    [
        t(),
        Thresholds {
            cef_hang_after_ms: 6_000,
            cef_gpu_deaths_to_restart: 5,
            ..t()
        },
    ]
}

// The sequence's first step: a beating page that never paints is reloaded once, on the first tick at
// its ready + `cef_hang_after_ms` (the declaration; not a tick before), as the next generation, naming
// one blank page. Nothing was on air (it is the first page), so nothing hides; nothing exits. Its
// reload paints, and nothing more happens for three more bounds.
#[test]
fn a_page_that_never_paints_reloads_once_at_its_bound() {
    let mut checked = 0;
    for th in blank_thresholds() {
        let hang = th.cef_hang_after_ms;
        let p = Pages {
            ready: every,
            paints: |g| g != 1,
            beats: every,
        };
        let (s, seen) = drive(on_air_with(th.clone()), 0, 4 * hang, p, vec![], &th);
        let r1 = first_tick(0, READY_AFTER);
        let at = first_tick(0, r1 + hang);
        assert_eq!(seen.blank_reloads, vec![(at, 2, 1)], "{hang}: {seen:?}");
        assert_eq!(seen.readies, vec![(1, r1), (2, first_tick(0, at + READY_AFTER))]);
        assert_eq!((seen.death_reloads.len(), seen.hides.len()), (0, 0), "{seen:?}");
        assert_eq!((seen.finals.len(), seen.exits.len()), (0, 0), "{seen:?}");
        assert_eq!(s.graphics_generation(), 2);
        assert_eq!(s.health(4 * hang).graphics, domain::health::GraphicsStatus::Ready);
        checked += 1;
    }
    eprintln!("blank pages reloaded once: {checked}");
    assert_eq!(checked, 2);
}

// The watch ends at the Ready page's own first frame: a painted report of generation 1 delivered after
// its ready, at the ready, 1 ms before the bound's tick or at that tick (before it), means no reload;
// one from generation 0 or 2, or one delivered while it was still loading, does not, and it reloads at
// its bound as if none had come.
#[test]
fn only_the_ready_pages_own_painted_report_ends_the_watch() {
    let th = t();
    let p = Pages {
        ready: every,
        paints: |g| g != 1,
        beats: every,
    };
    let r1 = first_tick(0, READY_AFTER);
    let at = first_tick(0, r1 + th.cef_hang_after_ms);
    let painted = |gen: u64| Event::CefPainted { gen };
    // (what, the extra reports at their instants, whether the page is still called blank).
    type Case = (&'static str, Vec<(Ms, Event)>, bool);
    let cases: [Case; 6] = [
        ("at the ready", vec![(r1, painted(1))], false),
        ("1 ms before the bound's tick", vec![(at - 1, painted(1))], false),
        ("at the bound's tick", vec![(at, painted(1))], false),
        ("from generation 0", vec![(r1, painted(0))], true),
        ("from generation 2", vec![(r1, painted(2))], true),
        ("while loading", vec![(r1 - period(), painted(1))], true),
    ];
    let mut checked = 0;
    for (name, extra, reloads) in cases {
        let (_, seen) = drive(
            on_air_with(th.clone()),
            0,
            3 * th.cef_hang_after_ms,
            p,
            extra,
            &th,
        );
        let want = if reloads { vec![(at, 2, 1)] } else { vec![] };
        assert_eq!(seen.blank_reloads, want, "{name}");
        assert!(seen.exits.is_empty(), "{name}");
        checked += 1;
    }
    eprintln!("painted reports checked: {checked}");
    assert_eq!(checked, 6);
}

// Repeated blank pages reach the in-place restart: each page that never paints reloads once at its
// bound, the k-th naming k blank pages, up to one short of `cef_gpu_deaths_to_restart`; the next is
// the restart in place, its final beat naming the count, then `Exit(EX_TEMPFAIL)`, and it does NOT
// also reload. So a page that never paints cannot loop forever: it costs restarts, which ruling 2A caps.
#[test]
fn blank_pages_reach_the_in_place_restart_and_it_reloads_nothing() {
    let mut checked = 0;
    for th in blank_thresholds() {
        let n = th.cef_gpu_deaths_to_restart;
        let p = Pages {
            ready: every,
            paints: never,
            beats: every,
        };
        let until = (n as Ms + 1) * (th.cef_hang_after_ms + 2 * READY_AFTER);
        let (s, seen) = drive(on_air_with(th.clone()), 0, until, p, vec![], &th);
        assert_eq!(seen.readies.len(), n, "{n}: one page a count: {seen:?}");
        let bounds: Vec<Ms> = seen
            .readies
            .iter()
            .map(|&(_, r)| first_tick(0, r + th.cef_hang_after_ms))
            .collect();
        let want: Vec<(Ms, u64, usize)> = (1..n).map(|k| (bounds[k - 1], 1 + k as u64, k)).collect();
        assert_eq!(seen.blank_reloads, want, "{n}");
        let last = bounds[n - 1];
        assert_eq!(
            seen.finals,
            vec![(last, ExitReason::Restart, Some(blank_text(n, 0)))],
            "{n}"
        );
        assert!(
            blank_text(n, 0).chars().count() <= 100,
            "the wire's 100 characters"
        );
        assert_eq!(seen.exits, vec![(last, EX_TEMPFAIL)], "{n}");
        assert_eq!(s.lifecycle(), Lifecycle::Exited(ExitReason::Restart));
        assert!(seen.death_reloads.is_empty() && seen.hides.is_empty(), "{seen:?}");
        checked += seen.blank_reloads.len() + 1;
    }
    eprintln!("blank pages counted to the restart: {checked}");
    let [d, x] = blank_thresholds();
    assert_eq!(checked, d.cef_gpu_deaths_to_restart + x.cef_gpu_deaths_to_restart);
}

// Blank pages and GPU-process deaths count together, each once. A blank page first: each death report
// below the shared count reloads (the deaths' own reload), and the death that brings blank + deaths to
// the count restarts with both named. Deaths first: each blank page below it reloads, and the blank page
// that reaches it restarts with both named. Neither path reloads at the restart.
#[test]
fn blank_pages_and_gpu_deaths_count_together_once_each() {
    let mut checked = 0;
    for th in blank_thresholds() {
        let n = th.cef_gpu_deaths_to_restart;
        let hang = th.cef_hang_after_ms;
        // A blank page, then deaths 1..n-1 (each report 3 s after the last, its reload ready and painted).
        let p = Pages {
            ready: every,
            paints: |g| g != 1,
            beats: every,
        };
        let b1 = first_tick(0, first_tick(0, READY_AFTER) + hang);
        let reports: Vec<(Ms, Event)> = (1..n)
            .map(|k| (b1 + 3_000 * k as Ms, Event::CefHealth(health(None, 0, k))))
            .collect();
        let until = b1 + 3_000 * n as Ms;
        let (_, seen) = drive(on_air_with(th.clone()), 0, until, p, reports, &th);
        assert_eq!(seen.blank_reloads, vec![(b1, 2, 1)], "{n}");
        let want: Vec<(Ms, u64, usize)> = (1..n - 1)
            .map(|k| (b1 + 3_000 * k as Ms, 2 + k as u64, k))
            .collect();
        assert_eq!(seen.death_reloads, want, "{n}: deaths below the shared count");
        let at = b1 + 3_000 * (n - 1) as Ms; // the report's own instant
        assert_eq!(
            seen.finals,
            vec![(at, ExitReason::Restart, Some(blank_text(1, n - 1)))],
            "{n}"
        );
        assert_eq!(seen.exits, vec![(at, EX_TEMPFAIL)], "{n}");
        checked += 1;
        // A death, then blank pages 1..n-1.
        let p = Pages {
            ready: every,
            paints: |g| g == 1,
            beats: every,
        };
        let death = 2_000;
        let until = death + (n as Ms + 1) * (hang + 2 * READY_AFTER);
        let report = vec![(death, Event::CefHealth(health(None, 0, 1)))];
        let (_, seen) = drive(on_air_with(th.clone()), 0, until, p, report, &th);
        assert_eq!(seen.death_reloads, vec![(death, 2, 1)], "{n}");
        let bounds: Vec<Ms> = seen
            .readies
            .iter()
            .skip(1)
            .map(|&(_, r)| first_tick(0, r + hang))
            .collect();
        assert_eq!(bounds.len(), n - 1, "{n}: {seen:?}");
        let want: Vec<(Ms, u64, usize)> = (1..n - 1).map(|k| (bounds[k - 1], 2 + k as u64, k)).collect();
        assert_eq!(
            seen.blank_reloads, want,
            "{n}: blank pages below the shared count"
        );
        let at = bounds[n - 2];
        assert_eq!(
            seen.finals,
            vec![(at, ExitReason::Restart, Some(blank_text(n - 1, 1)))],
            "{n}"
        );
        assert_eq!(seen.exits, vec![(at, EX_TEMPFAIL)], "{n}");
        checked += 1;
    }
    eprintln!("shared counts checked: {checked}");
    assert_eq!(checked, 2 * blank_thresholds().len());
}

// One page, one count, whichever way it fails, and nothing counts outside a Running Ready page:
// - silent and unpainted: a hang at the same bound, hidden as a hang, never also blank;
// - reloaded by a GPU-process death before its bound: one death, no blank page;
// - the reload of a blank page that never gets ready: loading pages are never blank, however long;
// - the final hold: a page Ready and unpainted there is never reloaded or counted.
// The first two are proven by the counts after them: the deaths' own rule alone restarts, at the
// deaths' count, with the deaths' own text.
#[test]
fn a_page_is_counted_once_and_only_while_running_ready() {
    let th = t();
    let n = th.cef_gpu_deaths_to_restart;
    let hang = th.cef_hang_after_ms;
    let r1 = first_tick(0, READY_AFTER);
    let bound = first_tick(0, r1 + hang);
    let deaths_after = |from: Ms, count: usize| -> Vec<(Ms, Event)> {
        (1..=count)
            .map(|k| (from + 3_000 * k as Ms, Event::CefHealth(health(None, 0, k))))
            .collect()
    };
    let deaths_text = format!("restart in place: {n} CEF GPU-process deaths");
    // Silent and unpainted: the hang, then deaths 1..=n restart on the deaths' own count.
    let p = Pages {
        ready: every,
        paints: |g| g >= 2,
        beats: |g| g >= 2,
    };
    let from = bound + th.cef_backoff_base_ms + 2_000;
    let until = from + 3_000 * (n as Ms + 1);
    let (_, seen) = drive(on_air_with(th.clone()), 0, until, p, deaths_after(from, n), &th);
    assert_eq!(seen.hides, vec![(bound, HideReason::Hang, 1)], "the hang");
    assert_eq!(seen.blank_reloads, vec![], "never also blank");
    assert_eq!(seen.death_reloads.len(), n - 1, "{seen:?}");
    let at = from + 3_000 * n as Ms; // the n-th report's own instant
    assert_eq!(
        seen.finals,
        vec![(at, ExitReason::Restart, Some(deaths_text.clone()))]
    );
    // Reloaded by a death before its bound: never blank, and the deaths alone reach their count.
    let p = Pages {
        ready: every,
        paints: |g| g >= 2,
        beats: every,
    };
    let from = r1 + hang / 2 - 3_000;
    let until = from + 3_000 * (n as Ms + 1);
    let (_, seen) = drive(on_air_with(th.clone()), 0, until, p, deaths_after(from, n), &th);
    assert_eq!(seen.blank_reloads, vec![], "reloaded before its bound");
    assert_eq!(seen.death_reloads.first().map(|r| (r.1, r.2)), Some((2, 1)));
    assert_eq!(seen.death_reloads.len(), n - 1, "{seen:?}");
    assert_eq!(
        seen.finals.iter().map(|f| f.2.clone()).collect::<Vec<_>>(),
        vec![Some(deaths_text)]
    );
    // A blank page's reload that never gets ready: loading, backing off, Failed; never blank again.
    let p = Pages {
        ready: |g| g == 1,
        paints: never,
        beats: every,
    };
    let until = bound + 3 * (th.cef_load_timeout_ms + th.cef_backoff_cap_ms);
    let (s, seen) = drive(on_air_with(th.clone()), 0, until, p, vec![], &th);
    assert_eq!(seen.blank_reloads, vec![(bound, 2, 1)], "{seen:?}");
    assert!(
        s.graphics_generation() > 2,
        "setup: the reload's load timed out and rebuilt"
    );
    assert!(seen.finals.is_empty() && seen.exits.is_empty(), "{seen:?}");
    // The final hold: requested a second after the ready, its bound inside the hold.
    assert!(
        r1 + 1_000 + th.final_hold_ms > bound,
        "setup: the bound falls in the hold"
    );
    let p = Pages {
        ready: every,
        paints: never,
        beats: every,
    };
    let ending = vec![(r1 + 1_000, Event::HeartbeatReply(Desired::Ending))];
    let until = r1 + 1_000 + th.final_hold_ms + th.drain_timeout_ms + 1_000;
    let (_, seen) = drive(on_air_with(th.clone()), 0, until, p, ending, &th);
    assert_eq!(seen.blank_reloads, vec![], "the hold reloads nothing");
    assert_eq!(
        seen.finals.iter().map(|f| f.1).collect::<Vec<_>>(),
        vec![ExitReason::Ended]
    );
    assert_eq!(seen.exits.iter().map(|e| e.1).collect::<Vec<_>>(), vec![0]);
    eprintln!("one-count sequences checked: 4");
}

// The shared count's rule, its table from its text (Owed 99): blank pages and GPU-process deaths count
// together toward `cef_gpu_deaths_to_restart`, and only with a blank page among them (deaths alone are
// their own rule, with their own text). Around the count, under two counts; each text fits the wire.
#[test]
fn page_faults_restart_only_with_a_blank_page_at_the_shared_count() {
    let mut checked = 0;
    for th in blank_thresholds() {
        let n = th.cef_gpu_deaths_to_restart;
        for blanks in 0..=n + 1 {
            for deaths in 0..=n + 1 {
                let want = (blanks > 0 && blanks + deaths >= n).then_some(RestartCause::PageFaults {
                    blank_pages: blanks,
                    gpu_process_deaths: deaths,
                });
                let got = page_faults_restart(blanks, deaths, &th);
                assert_eq!(got, want, "{blanks} blank, {deaths} deaths, count {n}");
                if let Some(c) = got {
                    assert_eq!(c.detail(), blank_text(blanks, deaths));
                    assert!(c.detail().chars().count() <= 100);
                }
                checked += 1;
            }
        }
    }
    eprintln!("shared-count cells checked: {checked}");
    let [d, x] = blank_thresholds();
    let cells = |n: usize| (n + 2) * (n + 2);
    assert_eq!(
        checked,
        cells(d.cef_gpu_deaths_to_restart) + cells(x.cef_gpu_deaths_to_restart)
    );
}
