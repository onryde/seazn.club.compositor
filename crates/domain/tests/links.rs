//! Sweeps over C04's links: every (state, event) pair against a matrix written from the brief, both
//! links over generated schedules against facts the harness tracks for itself, and the output's
//! recovery paths against F-004's 15 s broadcast budget. They live here, not inline, because the
//! domain's crate root forbids `clippy::print_stderr`, and a sweep must print the count it checked
//! (AGENTS.md §5).
use domain::config::{OutputProfile, Thresholds, OUTPUT_GAP_BUDGET_MS};
use domain::input_link::{InputLink, InputState, LossReason, KICK_HISTORY_CAP};
use domain::output_link::{OutputLink, OutputState};
use domain::time::Ms;

const BOTH_MODES: [OutputProfile; 2] = [OutputProfile::P720, OutputProfile::P1080];

/// The back-off definition, written independently of `backoff::delay`: the base, doubled once per
/// failure before this one, never above the cap.
fn wait(base: Ms, cap: Ms, failed_before: u32) -> Ms {
    (0..failed_before).fold(base.min(cap), |w, _| w.saturating_mul(2).min(cap))
}

fn in_wait(t: &Thresholds, failed_before: u32) -> Ms {
    wait(t.input_backoff_base_ms, t.input_backoff_cap_ms, failed_before)
}

fn out_wait(t: &Thresholds, failed_before: u32) -> Ms {
    wait(t.output_backoff_base_ms, t.output_backoff_cap_ms, failed_before)
}

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

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.next(xs.len() as u64) as usize]
    }
}

// ---- the brief's transitions, every (state, event) pair ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connecting,
    Up,
    Backoff,
}

const KINDS: [Kind; 3] = [Kind::Connecting, Kind::Up, Kind::Backoff];

/// Exhaustive `match`es, so a new state variant is a compile error here and `KINDS` cannot go stale
/// behind a sweep that still reports its old count.
fn input_kind(s: InputState) -> Kind {
    match s {
        InputState::Connecting { .. } => Kind::Connecting,
        InputState::Running { .. } => Kind::Up,
        InputState::Backoff { .. } => Kind::Backoff,
    }
}

fn output_kind(s: OutputState) -> Kind {
    match s {
        OutputState::Connecting { .. } => Kind::Connecting,
        OutputState::Connected { .. } => Kind::Up,
        OutputState::Backoff { .. } => Kind::Backoff,
    }
}

/// An input link in `kind` at generation 2, so generation 1 is a real torn-down pipeline and 3 is
/// one never built. Returns the link and the instant its timer is due (connect timeout, the stall
/// rule, the end of the back-off).
fn input_in(kind: Kind, t: &Thresholds) -> (InputLink, Ms) {
    let mut l = InputLink::new();
    assert!(l.on_tick(0, t));
    l.on_connected(0, 1);
    l.on_lost(100, 1, LossReason::Error, t);
    let built = 100 + t.input_backoff_base_ms;
    assert!(l.on_tick(built, t));
    let up = built + 50;
    match kind {
        Kind::Connecting => {}
        Kind::Up => l.on_connected(up, 2),
        Kind::Backoff => {
            l.on_connected(up, 2);
            l.on_lost(up + 50, 2, LossReason::Error, t);
        }
    }
    assert_eq!((input_kind(l.state()), l.generation()), (kind, 2), "setup");
    let due = match l.state() {
        InputState::Connecting { since, .. } => since + t.input_connect_timeout_ms,
        InputState::Running { since } => since + t.input_stall_after_ms,
        InputState::Backoff { until, .. } => until,
    };
    (l, due)
}

#[derive(Debug, Clone, Copy)]
enum InputEvent {
    Tick,
    Connected,
    Lost(LossReason),
    Stall,
}

const INPUT_EVENTS: [InputEvent; 6] = [
    InputEvent::Tick,
    InputEvent::Connected,
    InputEvent::Lost(LossReason::Eos),
    InputEvent::Lost(LossReason::Error),
    InputEvent::Lost(LossReason::Stalled),
    InputEvent::Stall,
];

/// Applies `e` at `now`, from generation `gen`. Returns whether a tick built.
fn apply_input(l: &mut InputLink, e: InputEvent, now: Ms, gen: u64, t: &Thresholds) -> bool {
    match e {
        InputEvent::Tick => l.on_tick(now, t),
        InputEvent::Connected => {
            l.on_connected(now, gen);
            false
        }
        InputEvent::Lost(r) => {
            l.on_lost(now, gen, r, t);
            false
        }
        InputEvent::Stall => {
            l.check_stall(now, None, t);
            false
        }
    }
}

/// The brief's transitions (Step 3), written from its prose: (kind after, built, kicks added).
fn brief_input(from: Kind, e: InputEvent) -> (Kind, bool, usize) {
    use InputEvent as E;
    use Kind as K;
    match (from, e) {
        (K::Backoff, E::Tick) => (K::Connecting, true, 0),
        (K::Connecting, E::Tick) => (K::Backoff, false, 0), // the connect timed out
        (K::Connecting, E::Connected) => (K::Up, false, 0),
        (K::Connecting, E::Lost(_)) => (K::Backoff, false, 0), // a failure, never a kick
        (K::Up, E::Lost(r)) => (K::Backoff, false, usize::from(r == LossReason::Eos)),
        (K::Up, E::Stall) => (K::Backoff, false, 0), // no frame for 5 s
        (k, _) => (k, false, 0),                     // everything else changes nothing
    }
}

#[test]
fn every_input_pair_follows_the_brief() {
    let t = Thresholds::default();
    let (mut checked, mut moved) = (0usize, 0usize);
    for kind in KINDS {
        for e in INPUT_EVENTS {
            let (mut l, due) = input_in(kind, &t);
            let built = apply_input(&mut l, e, due, 2, &t);
            let (want, want_built, want_kicks) = brief_input(kind, e);
            let got = (input_kind(l.state()), built, l.kicks(due, &t));
            assert_eq!(got, (want, want_built, want_kicks), "({kind:?}, {e:?})");
            assert_eq!(l.generation(), 2 + u64::from(want_built), "({kind:?}, {e:?})");
            checked += 1;
            moved += usize::from(want != kind);
        }
    }
    eprintln!("input pairs checked: {checked} ({moved} move the state)");
    assert_eq!(checked, KINDS.len() * INPUT_EVENTS.len(), "every pair, once");
    assert!(moved > 0, "a matrix where nothing moves is vacuous");
}

// Ticks 1 ms before each timer is due, and every event from generation 1 (torn down) and 3 (never
// built), change nothing in any state: not the state, not the generation, not the kicks.
#[test]
fn input_early_ticks_and_other_generations_change_nothing() {
    let t = Thresholds::default();
    let mut checked = 0usize;
    for kind in KINDS {
        let (mut l, due) = input_in(kind, &t);
        let before = l.clone();
        assert!(!l.on_tick(due - 1, &t), "{kind:?}: built 1 ms early");
        assert_eq!(l, before, "{kind:?}: early tick");
        checked += 1;
        for gen in [1, 3] {
            for e in INPUT_EVENTS
                .into_iter()
                .filter(|e| !matches!(e, InputEvent::Tick | InputEvent::Stall))
            {
                let (mut l, due) = input_in(kind, &t);
                let before = l.clone();
                apply_input(&mut l, e, due, gen, &t);
                assert_eq!(l, before, "({kind:?}, {e:?}) from generation {gen}");
                checked += 1;
            }
        }
    }
    eprintln!("input no-ops checked: {checked}");
    assert_eq!(checked, KINDS.len() * (1 + 2 * 4));
}

/// An output link in `kind` at sink generation 2 (1 torn down, 3 never built), with 1 000 bytes sent
/// when Connected. Returns the link and the instant its timer is due.
fn output_in(kind: Kind, t: &Thresholds) -> (OutputLink, Ms) {
    let mut o = OutputLink::new();
    assert!(o.on_tick(0, t));
    o.on_progress(0, 1, 1_000);
    o.on_error(100, 1, t);
    let built = 100 + t.output_backoff_base_ms;
    assert!(o.on_tick(built, t));
    let up = built + 50;
    match kind {
        Kind::Connecting => {}
        Kind::Up => o.on_progress(up, 2, 1_000),
        Kind::Backoff => {
            o.on_progress(up, 2, 1_000);
            o.on_error(up + 50, 2, t);
        }
    }
    assert_eq!((output_kind(o.state()), o.generation()), (kind, 2), "setup");
    let due = match o.state() {
        OutputState::Connecting { since, .. } => since + t.output_connect_timeout_ms,
        OutputState::Connected { last_progress, .. } => last_progress + t.output_stall_after_ms,
        OutputState::Backoff { until, .. } => until,
    };
    (o, due)
}

#[derive(Debug, Clone, Copy)]
enum OutputEvent {
    Tick,
    /// A total above every one this sink reported (1 001: Connected holds 1 000).
    Progress,
    /// A total that is not above the highest: 0 for a new sink, a repeat for a connected one.
    NoProgress,
    Error,
}

const OUTPUT_EVENTS: [OutputEvent; 4] = [
    OutputEvent::Tick,
    OutputEvent::Progress,
    OutputEvent::NoProgress,
    OutputEvent::Error,
];

fn apply_output(o: &mut OutputLink, e: OutputEvent, now: Ms, gen: u64, t: &Thresholds) -> bool {
    let high = match o.state() {
        OutputState::Connected { sent, .. } => sent,
        _ => 0,
    };
    match e {
        OutputEvent::Tick => return o.on_tick(now, t),
        OutputEvent::Progress => o.on_progress(now, gen, 1_001),
        OutputEvent::NoProgress => o.on_progress(now, gen, high),
        OutputEvent::Error => o.on_error(now, gen, t),
    }
    false
}

/// The brief's transitions (Step 6) with the C04 corrections (bytes sent, F-005): (kind after, built).
fn brief_output(from: Kind, e: OutputEvent) -> (Kind, bool) {
    use Kind as K;
    use OutputEvent as E;
    match (from, e) {
        (K::Backoff, E::Tick) => (K::Connecting, true),
        (K::Connecting, E::Tick) => (K::Backoff, false), // the connect timed out
        (K::Up, E::Tick) => (K::Connecting, true),       // stalled for 5 s: swap at once
        (K::Connecting, E::Progress) => (K::Up, false),  // the first bytes connect it
        (K::Connecting | K::Up, E::Error) => (K::Backoff, false),
        (k, _) => (k, false),
    }
}

#[test]
fn every_output_pair_follows_the_brief() {
    let t = Thresholds::default();
    let (mut checked, mut moved) = (0usize, 0usize);
    for kind in KINDS {
        for e in OUTPUT_EVENTS {
            let (mut o, due) = output_in(kind, &t);
            let built = apply_output(&mut o, e, due, 2, &t);
            let want = brief_output(kind, e);
            assert_eq!((output_kind(o.state()), built), want, "({kind:?}, {e:?})");
            assert_eq!(o.generation(), 2 + u64::from(want.1), "({kind:?}, {e:?})");
            checked += 1;
            moved += usize::from(want.0 != kind);
        }
    }
    eprintln!("output pairs checked: {checked} ({moved} move the state)");
    assert_eq!(checked, KINDS.len() * OUTPUT_EVENTS.len(), "every pair, once");
    assert!(moved > 0, "a matrix where nothing moves is vacuous");
}

#[test]
fn output_early_ticks_and_other_generations_change_nothing() {
    let t = Thresholds::default();
    let mut checked = 0usize;
    for kind in KINDS {
        let (mut o, due) = output_in(kind, &t);
        let before = o.clone();
        assert!(!o.on_tick(due - 1, &t), "{kind:?}: built 1 ms early");
        assert_eq!(o, before, "{kind:?}: early tick");
        checked += 1;
        for gen in [1, 3] {
            for e in [OutputEvent::Progress, OutputEvent::Error] {
                let (mut o, due) = output_in(kind, &t);
                let before = o.clone();
                apply_output(&mut o, e, due, gen, &t);
                assert_eq!(o, before, "({kind:?}, {e:?}) from generation {gen}");
                checked += 1;
            }
        }
    }
    eprintln!("output no-ops checked: {checked}");
    assert_eq!(checked, KINDS.len() * (1 + 2 * 2));
}

// ---- generated schedules, against facts the harness tracks itself ----

const CASES: u64 = 120;
const STEPS: usize = 1_500;

/// The next instant at which a timer of `due` could fire, ±1 ms, or a plain step forward.
fn advance(g: &mut Lcg, now: Ms, due: Option<Ms>) -> Ms {
    match (g.next(4), due) {
        (0, Some(d)) if d > now => d - 1 + g.next(3), // d - 1, d or d + 1
        _ => now + g.pick(&[0, 1, 33, 34, 120, 300, 999, 2_000]),
    }
}

#[derive(Debug, Default)]
struct OutputCounts {
    builds_from_backoff: usize,
    stall_swaps: usize,
    connect_timeouts: usize,
    early_or_quiet_ticks: usize,
    connects: usize,
    refreshes: usize,
    repeats_connecting: usize,
    repeats_connected: usize,
    backwards: usize,
    errors_connecting: usize,
    errors_connected: usize,
    ignored_while_backing_off: usize,
    stale_connecting: usize,
    stale_connected: usize,
    stale_backoff: usize,
    waits_at_cap: usize,
}

#[test]
fn output_link_keeps_its_rules_over_generated_schedules() {
    let t = Thresholds::default();
    let (stall, timeout) = (t.output_stall_after_ms, t.output_connect_timeout_ms);
    let mut c = OutputCounts::default();
    for case in 0..CASES {
        let mut g = Lcg(case.wrapping_mul(7_919).wrapping_add(17));
        let mut o = OutputLink::new();
        let mut now: Ms = 0;
        // Facts kept by the harness: the highest total delivered to the current sink, and the
        // consecutive failures since the last first progress.
        let mut high: u64 = 0;
        let mut failures: u32 = 0;
        for _ in 0..STEPS {
            let due = match o.state() {
                OutputState::Connecting { since, .. } => Some(since + timeout),
                OutputState::Connected { last_progress, .. } => Some(last_progress + stall),
                OutputState::Backoff { until, .. } => Some(until),
            };
            now = advance(&mut g, now, due);
            let (before, gen) = (o.state(), o.generation());
            let kind = output_kind(before);
            match g.next(12) {
                0..=4 => {
                    let built = o.on_tick(now, &t);
                    match before {
                        OutputState::Backoff { attempt, until } if now >= until => {
                            assert!(built, "case {case}: a due back-off must build");
                            assert_eq!(attempt, failures, "case {case}: the attempt is the failure count");
                            assert_eq!(o.state(), OutputState::Connecting { attempt, since: now });
                            c.builds_from_backoff += 1;
                        }
                        OutputState::Connected { last_progress, .. } if now - last_progress >= stall => {
                            assert!(built, "case {case}: a stalled sink must be swapped");
                            assert_eq!(failures, 0, "case {case}");
                            assert_eq!(
                                o.state(),
                                OutputState::Connecting {
                                    attempt: 0,
                                    since: now
                                }
                            );
                            c.stall_swaps += 1;
                        }
                        OutputState::Connecting { attempt, since } if now - since >= timeout => {
                            assert!(!built, "case {case}: a timeout is not a build");
                            assert_eq!(attempt, failures, "case {case}");
                            let w = out_wait(&t, failures);
                            failures += 1;
                            let want = OutputState::Backoff {
                                attempt: failures,
                                until: now + w,
                            };
                            assert_eq!(o.state(), want, "case {case}");
                            c.connect_timeouts += 1;
                            c.waits_at_cap += usize::from(w == t.output_backoff_cap_ms);
                        }
                        _ => {
                            assert!(!built, "case {case}: built from {before:?} at {now}");
                            assert_eq!(o.state(), before, "case {case}: a quiet tick changed {before:?}");
                            c.early_or_quiet_ticks += 1;
                        }
                    }
                    if built {
                        assert_eq!(o.generation(), gen + 1, "case {case}: one build, one generation");
                        high = 0; // a new sink counts from 0
                    }
                }
                5..=7 => {
                    let total = g.pick(&[high, high.saturating_sub(1), high + 1, high + 64_000, 0]);
                    o.on_progress(now, gen, total);
                    if kind == Kind::Backoff {
                        assert_eq!(o.state(), before, "case {case}: bytes from a failed sink");
                        c.ignored_while_backing_off += 1;
                    } else if total > high {
                        let want = OutputState::Connected {
                            last_progress: now,
                            sent: total,
                        };
                        assert_eq!(o.state(), want, "case {case}: progress {total} over {high}");
                        if kind == Kind::Connecting {
                            failures = 0;
                            c.connects += 1;
                        } else {
                            c.refreshes += 1;
                        }
                        high = total;
                    } else {
                        assert_eq!(o.state(), before, "case {case}: {total} ≤ {high} is no progress");
                        c.backwards += usize::from(total < high);
                        if kind == Kind::Connecting {
                            c.repeats_connecting += 1;
                        } else {
                            c.repeats_connected += 1;
                        }
                    }
                }
                8 => {
                    o.on_error(now, gen, &t);
                    if kind == Kind::Backoff {
                        assert_eq!(o.state(), before, "case {case}: a second error");
                        c.ignored_while_backing_off += 1;
                    } else {
                        let w = out_wait(&t, failures);
                        failures += 1;
                        let want = OutputState::Backoff {
                            attempt: failures,
                            until: now + w,
                        };
                        assert_eq!(o.state(), want, "case {case}: error from {before:?}");
                        if kind == Kind::Connecting {
                            c.errors_connecting += 1;
                        } else {
                            c.errors_connected += 1;
                        }
                        c.waits_at_cap += usize::from(w == t.output_backoff_cap_ms);
                    }
                }
                _ => {
                    let other = if gen > 0 && g.next(2) == 0 {
                        gen - 1
                    } else {
                        gen + 1
                    };
                    if g.next(2) == 0 {
                        o.on_progress(now, other, high + 64_000);
                    } else {
                        o.on_error(now, other, &t);
                    }
                    assert_eq!(
                        o.state(),
                        before,
                        "case {case}: generation {other} while at {gen}"
                    );
                    match kind {
                        Kind::Connecting => c.stale_connecting += 1,
                        Kind::Up => c.stale_connected += 1,
                        Kind::Backoff => c.stale_backoff += 1,
                    }
                }
            }
            assert!(
                o.generation() <= gen + 1,
                "case {case}: at most one build per event"
            );
        }
    }
    eprintln!("output schedules: {CASES} cases × {STEPS} steps: {c:?}");
    let all = [
        ("builds from back-off", c.builds_from_backoff),
        ("stall swaps", c.stall_swaps),
        ("connect timeouts", c.connect_timeouts),
        ("quiet ticks", c.early_or_quiet_ticks),
        ("connects", c.connects),
        ("refreshes", c.refreshes),
        ("repeats while connecting", c.repeats_connecting),
        ("repeats while connected", c.repeats_connected),
        ("backwards totals", c.backwards),
        ("errors while connecting", c.errors_connecting),
        ("errors while connected", c.errors_connected),
        ("events ignored while backing off", c.ignored_while_backing_off),
        ("stale while connecting", c.stale_connecting),
        ("stale while connected", c.stale_connected),
        ("stale while backing off", c.stale_backoff),
        ("waits at the cap", c.waits_at_cap),
    ];
    for (name, n) in all {
        assert!(n > 0, "never exercised: {name}");
    }
}

#[derive(Debug, Default)]
struct InputCounts {
    builds: usize,
    connect_timeouts: usize,
    quiet_ticks: usize,
    connects: usize,
    duplicate_connects: usize,
    kicks: usize,
    losses_connecting: usize,
    losses_running: usize,
    ignored_while_backing_off: usize,
    stalls: usize,
    stall_checks_held: usize,
    stale: usize,
    alert_on: usize,
    alert_cleared: usize,
    kick_cap_reached: usize,
    waits_at_cap: usize,
}

#[test]
fn input_link_keeps_its_rules_over_generated_schedules() {
    let t = Thresholds::default();
    let (stall, timeout, span) = (
        t.input_stall_after_ms,
        t.input_connect_timeout_ms,
        t.kick_window_ms,
    );
    let mut c = InputCounts::default();
    for case in 0..CASES {
        let mut g = Lcg(case.wrapping_mul(104_729).wrapping_add(3));
        // Kick storms: only ticks, first frames and EOS, so more kicks land inside one window than
        // the history holds. The mixed cases alone never get there.
        let storm = case % 4 == 0;
        let mut l = InputLink::new();
        let mut now: Ms = 0;
        // Facts kept by the harness: consecutive failures, and every kick still inside the window.
        let mut failures: u32 = 0;
        let mut kicks: Vec<Ms> = Vec::new();
        let mut alerted = false;
        for _ in 0..STEPS {
            let due = match l.state() {
                InputState::Connecting { since, .. } => Some(since + timeout),
                InputState::Running { since } => Some(since + stall),
                InputState::Backoff { until, .. } => Some(until),
            };
            // Storms take short steps only, so a kick cycle (EOS, the 1 s back-off, the rebuild, a
            // first frame) takes about 2 s. The mixed cases run long enough for kicks to age out.
            now = if storm {
                now + g.pick(&[0, 33, 120, 300])
            } else {
                advance(&mut g, now, due)
            };
            let (before, gen) = (l.state(), l.generation());
            let roll = if storm { g.pick(&[0, 0, 5, 7]) } else { g.next(12) };
            match roll {
                0..=4 => {
                    let built = l.on_tick(now, &t);
                    match before {
                        InputState::Backoff { attempt, until } if now >= until => {
                            assert!(built, "case {case}: a due back-off must build");
                            assert_eq!(attempt, failures, "case {case}");
                            assert_eq!(l.state(), InputState::Connecting { attempt, since: now });
                            assert_eq!(l.generation(), gen + 1);
                            c.builds += 1;
                        }
                        InputState::Connecting { attempt, since } if now - since >= timeout => {
                            assert!(!built);
                            assert_eq!(attempt, failures, "case {case}");
                            let w = in_wait(&t, failures);
                            failures += 1;
                            let want = InputState::Backoff {
                                attempt: failures,
                                until: now + w,
                            };
                            assert_eq!(l.state(), want, "case {case}");
                            c.connect_timeouts += 1;
                            c.waits_at_cap += usize::from(w == t.input_backoff_cap_ms);
                        }
                        _ => {
                            assert!(!built, "case {case}: built from {before:?}");
                            assert_eq!(l.state(), before, "case {case}: a quiet tick");
                            c.quiet_ticks += 1;
                        }
                    }
                }
                5 | 6 => {
                    l.on_connected(now, gen);
                    match before {
                        InputState::Connecting { .. } => {
                            assert_eq!(l.state(), InputState::Running { since: now }, "case {case}");
                            failures = 0;
                            c.connects += 1;
                        }
                        _ => {
                            assert_eq!(l.state(), before, "case {case}: connected while {before:?}");
                            c.duplicate_connects += 1;
                        }
                    }
                }
                7 => {
                    let reason = if storm {
                        LossReason::Eos
                    } else {
                        g.pick(&[LossReason::Eos, LossReason::Error, LossReason::Stalled])
                    };
                    l.on_lost(now, gen, reason, &t);
                    lost(&t, &mut c, before, l.state(), now, &mut failures, case);
                    if matches!(before, InputState::Running { .. }) && reason == LossReason::Eos {
                        kicks.push(now);
                        c.kicks += 1;
                    }
                }
                8 => {
                    let last_frame = match g.next(5) {
                        0 => None,
                        1 => Some(now),
                        2 => Some(now.saturating_sub(stall)),
                        3 => Some(now.saturating_sub(stall - 1)),
                        _ => Some(now.saturating_sub(g.next(3 * stall))),
                    };
                    l.check_stall(now, last_frame, &t);
                    match before {
                        InputState::Running { since } => {
                            let fresh = last_frame.map_or(since, |f| f.max(since));
                            if now.saturating_sub(fresh) >= stall {
                                lost(&t, &mut c, before, l.state(), now, &mut failures, case);
                                c.stalls += 1;
                            } else {
                                assert_eq!(l.state(), before, "case {case}: a fresh frame holds");
                                c.stall_checks_held += 1;
                            }
                        }
                        _ => assert_eq!(l.state(), before, "case {case}: the stall rule is Running only"),
                    }
                }
                _ => {
                    let other = if gen > 0 && g.next(2) == 0 {
                        gen - 1
                    } else {
                        gen + 1
                    };
                    if g.next(2) == 0 {
                        l.on_connected(now, other);
                    } else {
                        l.on_lost(now, other, LossReason::Eos, &t);
                    }
                    assert_eq!(
                        l.state(),
                        before,
                        "case {case}: generation {other} while at {gen}"
                    );
                    c.stale += 1;
                }
            }
            kicks.retain(|&k| now - k <= span);
            let want = kicks.len().min(KICK_HISTORY_CAP);
            assert_eq!(l.kicks(now, &t), want, "case {case}: kicks at {now}");
            c.kick_cap_reached += usize::from(kicks.len() > KICK_HISTORY_CAP);
            let alert = want > t.kick_alert_count;
            assert_eq!(
                l.kick_alert(now, &t),
                alert,
                "case {case}: alert with {want} kicks"
            );
            c.alert_on += usize::from(alert);
            c.alert_cleared += usize::from(alerted && !alert);
            alerted = alert;
        }
    }
    eprintln!("input schedules: {CASES} cases × {STEPS} steps: {c:?}");
    let all = [
        ("builds", c.builds),
        ("connect timeouts", c.connect_timeouts),
        ("quiet ticks", c.quiet_ticks),
        ("connects", c.connects),
        ("duplicate connects", c.duplicate_connects),
        ("kicks", c.kicks),
        ("losses while connecting", c.losses_connecting),
        ("losses while running", c.losses_running),
        ("losses ignored while backing off", c.ignored_while_backing_off),
        ("stalls", c.stalls),
        ("stall checks that held", c.stall_checks_held),
        ("stale events", c.stale),
        ("alert on", c.alert_on),
        ("alert cleared", c.alert_cleared),
        ("kick history at its cap", c.kick_cap_reached),
        ("waits at the cap", c.waits_at_cap),
    ];
    for (name, n) in all {
        assert!(n > 0, "never exercised: {name}");
    }
}

/// A current-generation loss (EOS, error or the stall rule) from `before`.
fn lost(
    t: &Thresholds,
    c: &mut InputCounts,
    before: InputState,
    after: InputState,
    now: Ms,
    failures: &mut u32,
    case: u64,
) {
    match before {
        InputState::Backoff { .. } => {
            assert_eq!(after, before, "case {case}: already lost");
            c.ignored_while_backing_off += 1;
        }
        _ => {
            if matches!(before, InputState::Running { .. }) {
                assert_eq!(*failures, 0, "case {case}: Running means no failures pending");
                c.losses_running += 1;
            } else {
                c.losses_connecting += 1;
            }
            let w = in_wait(t, *failures);
            *failures += 1;
            assert_eq!(
                after,
                InputState::Backoff {
                    attempt: *failures,
                    until: now + w
                },
                "case {case}"
            );
            c.waits_at_cap += usize::from(w == t.input_backoff_cap_ms);
        }
    }
}

// ---- F-004's 15 s budget, through the real machine ----

/// F-004 rerun 1 saw one single-shot RTMPS restart fail ("Socket I/O timed out"), and docs/_INDEX.md
/// (the row "One output recovery fits the 15 s broadcast split") asks C04 to budget the attempt after
/// it explicitly. So a recovery from an error must fit the budget through this many failed connects.
const FAILED_CONNECTS_BUDGETED: u32 = 1;

/// The C09 brief: a 1 s timer reads the sink's byte count.
const POLL_MS: Ms = 1_000;

/// How far past `last_progress` a simulation may run.
const RUN_MS: Ms = 60_000;

#[derive(Debug, Clone, Copy)]
enum Fault {
    /// The sink errors `after` ms after its last progress (0: a reset that takes the last bytes with it).
    Error { after: Ms },
    /// The bytes stop and nothing errors: the stall rule ends the push.
    Stall,
}

#[derive(Debug)]
struct Recovery {
    last_progress: Ms,
    /// The error, or for a stall the swap.
    fault_at: Ms,
    recovered_at: Ms,
}

/// One output recovery through the real `OutputLink`, millisecond by millisecond. `ticks[ms]` says
/// whether a tick lands on that millisecond. The sink pushes, its total growing on every poll, the
/// last one at `last_progress`; then the fault. The first `failed` sinks built after the fault hang
/// (no bytes and no error, so the domain's connect timeout ends each one). The next one sends its
/// first bytes `connect_ms` after it was built, and the recovery ends there.
fn recover(
    t: &Thresholds,
    ticks: &[bool],
    last_progress: Ms,
    fault: Fault,
    failed: u32,
    connect_ms: Ms,
) -> Recovery {
    let mut o = OutputLink::new();
    let mut total = 0u64;
    let mut fault_at = None;
    let mut builds_after_fault = 0u32;
    let mut first_bytes: Option<(u64, Ms)> = None;
    for now in 0..ticks.len() as Ms {
        if now >= 1 && now <= last_progress && (last_progress - now).is_multiple_of(POLL_MS) {
            total += 64_000;
            o.on_progress(now, 1, total);
            assert!(
                o.is_connected() && o.generation() == 1,
                "the healthy push at {now}"
            );
        }
        if let Fault::Error { after } = fault {
            if now == last_progress + after {
                o.on_error(now, 1, t);
                fault_at = Some(now);
            }
        }
        if let Some((gen, at)) = first_bytes {
            if now == at {
                o.on_progress(now, gen, 4_096);
                assert!(o.is_connected(), "the recovering sink's first bytes connect it");
                return Recovery {
                    last_progress,
                    fault_at: fault_at.expect("a fault happened"),
                    recovered_at: now,
                };
            }
        }
        if ticks[now as usize] && o.on_tick(now, t) && o.generation() > 1 {
            builds_after_fault += 1;
            if matches!(fault, Fault::Stall) && builds_after_fault == 1 {
                fault_at = Some(now);
            }
            if builds_after_fault > failed {
                first_bytes = Some((o.generation(), now + connect_ms));
            }
        }
    }
    panic!("{fault:?} with {failed} failed connects never recovered");
}

/// The worst case, written from the thresholds alone. From the error: the waits before each of the
/// k + 1 builds, k connects that use their whole timeout, then a connect whose first bytes come 1 ms
/// before its own timeout would end it.
fn worst_from_error(t: &Thresholds, k: u32) -> Ms {
    let timeout = t.output_connect_timeout_ms;
    (0..=k).map(|j| out_wait(t, j)).sum::<Ms>() + Ms::from(k) * timeout + (timeout - 1)
}

/// From the last bytes, for a stall: the stall time, then the swap at once (no wait before it), and k
/// failed connects each followed by its wait.
fn worst_from_stall(t: &Thresholds, k: u32) -> Ms {
    let timeout = t.output_connect_timeout_ms;
    t.output_stall_after_ms
        + (0..k).map(|j| out_wait(t, j)).sum::<Ms>()
        + Ms::from(k) * timeout
        + (timeout - 1)
}

const SHOWN: u32 = 3;

#[test]
fn output_recovery_paths_against_the_f004_budget_on_a_1_ms_grid() {
    let t = Thresholds::default();
    let worst_connect = t.output_connect_timeout_ms - 1;
    let late = t.output_stall_after_ms - 1; // an error 1 ms before the stall rule would have fired
    let last = 20_000;
    let ticks = vec![true; (last + RUN_MS) as usize];
    let mut rows = Vec::new();
    for k in 0..=SHOWN {
        let e = recover(&t, &ticks, last, Fault::Error { after: 0 }, k, worst_connect);
        let s = recover(&t, &ticks, last, Fault::Stall, k, worst_connect);
        let l = recover(&t, &ticks, last, Fault::Error { after: late }, k, worst_connect);
        let row = (
            k,
            e.recovered_at - e.fault_at,
            s.recovered_at - s.last_progress,
            l.recovered_at - l.last_progress,
        );
        assert_eq!(row.1, worst_from_error(&t, k), "from the error, k = {k}");
        assert_eq!(
            e.fault_at, e.last_progress,
            "a reset takes the last bytes with it"
        );
        assert_eq!(row.2, worst_from_stall(&t, k), "a stall, k = {k}");
        assert_eq!(
            s.fault_at - s.last_progress,
            t.output_stall_after_ms,
            "the swap comes at the stall time"
        );
        assert_eq!(row.3, late + worst_from_error(&t, k), "a late error, k = {k}");
        eprintln!(
            "k = {k} failed connect(s): from the error {} ms; from the last bytes: after a stall {} ms, \
             after an error 1 ms before the stall rule {} ms (budget {OUTPUT_GAP_BUDGET_MS} ms)",
            row.1, row.2, row.3
        );
        rows.push(row);
    }
    let fits =
        |gap: fn(&(u32, Ms, Ms, Ms)) -> Ms| rows.iter().take_while(|r| gap(r) < OUTPUT_GAP_BUDGET_MS).count();
    let (from_error, stall, late_error) = (fits(|r| r.1), fits(|r| r.2), fits(|r| r.3));
    eprintln!(
        "failed connects that still fit {OUTPUT_GAP_BUDGET_MS} ms: from the error {}, after a stall {}, \
         after a late error {} (of {} rows)",
        from_error as i64 - 1,
        stall as i64 - 1,
        late_error as i64 - 1,
        rows.len()
    );
    assert!(!rows.is_empty(), "measured 0 recoveries");
    // The claim: from an output error, the budget holds through the failed connect F-004 saw.
    for r in &rows[..=FAILED_CONNECTS_BUDGETED as usize] {
        assert!(
            r.1 < OUTPUT_GAP_BUDGET_MS,
            "from the error, k = {}: {} ms",
            r.0,
            r.1
        );
    }
    // A single attempt fits on every path.
    assert!(
        rows[0].2 < OUTPUT_GAP_BUDGET_MS && rows[0].3 < OUTPUT_GAP_BUDGET_MS,
        "{:?}",
        rows[0]
    );
}

/// Tick instants for an output mode: tick i at ⌊i · frame period⌋ ms (C08 ticks on the frame period).
fn tick_grid(p: OutputProfile, len: Ms) -> (Vec<bool>, Ms) {
    let period = p.frame_period_ms();
    let mut ticks = vec![false; len as usize];
    let (mut i, mut prev, mut widest) = (0u32, 0, 0);
    loop {
        let at = (f64::from(i) * period).floor() as Ms;
        if at >= len {
            break;
        }
        ticks[at as usize] = true;
        widest = widest.max(at - prev);
        prev = at;
        i += 1;
    }
    (ticks, widest)
}

// On the real tick grid of both modes, at every phase of the fault against it: each timed step (a
// back-off ending, a connect timing out, the stall rule) waits for the next tick, so the gap exceeds
// the 1 ms worst case by at most one tick spacing per timed step, and the budget still holds.
#[test]
fn the_budget_holds_on_the_frame_tick_grid_at_every_phase() {
    let t = Thresholds::default();
    let worst_connect = t.output_connect_timeout_ms - 1;
    let late = t.output_stall_after_ms - 1;
    let (mut cases, mut worst_seen) = (0usize, 0);
    for mode in BOTH_MODES {
        let (ticks, widest) = tick_grid(mode, 21_000 + RUN_MS);
        assert!(
            widest == 33 || widest == 34,
            "{mode:?}: 30 fps ticks are 33–34 ms apart, got {widest}"
        );
        for phase in 0..widest {
            let last = 20_000 + phase;
            for k in 0..=FAILED_CONNECTS_BUDGETED {
                let slack = Ms::from(2 * k + 1) * widest;
                let e = recover(&t, &ticks, last, Fault::Error { after: 0 }, k, worst_connect);
                let from_error = e.recovered_at - e.fault_at;
                let floor = worst_from_error(&t, k);
                assert!(
                    (floor..=floor + slack).contains(&from_error),
                    "{mode:?} φ{phase} k{k}: {from_error}"
                );
                assert!(
                    from_error < OUTPUT_GAP_BUDGET_MS,
                    "{mode:?} φ{phase} k{k}: {from_error} ms"
                );
                worst_seen = worst_seen.max(from_error);
                if k == 0 {
                    let s = recover(&t, &ticks, last, Fault::Stall, k, worst_connect);
                    let gap = s.recovered_at - s.last_progress;
                    let floor = worst_from_stall(&t, k);
                    assert!(
                        (floor..=floor + slack).contains(&gap),
                        "{mode:?} φ{phase} stall: {gap}"
                    );
                    assert!(gap < OUTPUT_GAP_BUDGET_MS, "{mode:?} φ{phase} stall: {gap} ms");
                    let l = recover(&t, &ticks, last, Fault::Error { after: late }, k, worst_connect);
                    let gap = l.recovered_at - l.last_progress;
                    let floor = late + worst_from_error(&t, k);
                    assert!(
                        (floor..=floor + slack).contains(&gap),
                        "{mode:?} φ{phase} late error: {gap}"
                    );
                    assert!(
                        gap < OUTPUT_GAP_BUDGET_MS,
                        "{mode:?} φ{phase} late error: {gap} ms"
                    );
                    cases += 2;
                }
                cases += 1;
            }
        }
    }
    eprintln!(
        "tick-grid recoveries checked: {cases}; worst from an error through {FAILED_CONNECTS_BUDGETED} \
         failed connect(s): {worst_seen} ms (budget {OUTPUT_GAP_BUDGET_MS} ms)"
    );
    assert!(cases > 0, "checked 0 recoveries");
}
