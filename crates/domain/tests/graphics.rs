//! Sweeps over C05's graphics sub-machine and degrade ladder: every (state, event) pair against a
//! matrix written from the brief, both machines over generated schedules against facts the harness
//! tracks for itself, and, on the frame tick grid of both modes, the plan's review focus 2 (a page
//! that never signals ready) and an encoder that drops every frame. They live here, not inline,
//! because the domain's crate root forbids `clippy::print_stderr`, and a sweep must print the count
//! it checked (AGENTS.md §5).
use domain::config::{OutputProfile, Thresholds};
use domain::graphics::{Graphics, GraphicsAction, GraphicsState};
use domain::ladder::{Ladder, Rung};
use domain::time::Ms;

use GraphicsAction::{Hide, Restart};

const BOTH_MODES: [OutputProfile; 2] = [OutputProfile::P720, OutputProfile::P1080];

/// The CEF back-off, written independently of `backoff::delay`: the base, doubled once per failure
/// before this one in the window, never above the cap.
fn wait(t: &Thresholds, failed_before: usize) -> Ms {
    (0..failed_before).fold(t.cef_backoff_base_ms.min(t.cef_backoff_cap_ms), |w, _| {
        w.saturating_mul(2).min(t.cef_backoff_cap_ms)
    })
}

/// How many of the harness's own stamps lie within `span` of `now`. A stamp exactly `span` old
/// counts (spec §4.2 "3 in 5 min", the C02 `Window` definition).
fn within(stamps: &[Ms], now: Ms, span: Ms) -> usize {
    stamps.iter().filter(|&&s| now.saturating_sub(s) <= span).count()
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

/// Tick instants for an output mode, shifted by `phase` ms: tick i at `phase` + ⌊i · frame period⌋
/// (C08 ticks on the frame period).
fn ticks(p: OutputProfile, phase: Ms) -> impl Iterator<Item = Ms> {
    let period = p.frame_period_ms();
    (0u32..).map(move |i| phase + (f64::from(i) * period).floor() as Ms)
}

/// The widest spacing of a mode's ticks.
fn widest(p: OutputProfile) -> Ms {
    let first: Vec<Ms> = ticks(p, 0).take(301).collect();
    first.windows(2).map(|w| w[1] - w[0]).max().unwrap()
}

// ---- graphics: the brief's transitions, every (state, event) pair ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Starting,
    Ready,
    Backoff,
    Failed,
}

/// Exhaustive, so a new state variant is a compile error here and the setups cannot go stale behind
/// a sweep that still reports its old count.
fn kind(s: GraphicsState) -> Kind {
    match s {
        GraphicsState::Starting { .. } => Kind::Starting,
        GraphicsState::Ready { .. } => Kind::Ready,
        GraphicsState::Backoff { .. } => Kind::Backoff,
        GraphicsState::Failed { .. } => Kind::Failed,
    }
}

/// The instant a state's own timer is due: the load timeout, the recycle, the back-off's end, the
/// cooldown's end.
fn due(s: GraphicsState, t: &Thresholds) -> Ms {
    match s {
        GraphicsState::Starting { since } => since + t.cef_load_timeout_ms,
        GraphicsState::Ready { since } => since + t.cef_recycle_every_ms,
        GraphicsState::Backoff { until, .. } => until,
        GraphicsState::Failed { retry_at } => retry_at,
    }
}

/// Every reachable (state, failures already in the window) setup. Starting and Ready can hold up to
/// `cef_fail_count - 1` earlier failures; Backoff holds one fewer, since its own failure is in the
/// window too; Failed exists only at the count.
fn setups(t: &Thresholds) -> Vec<(Kind, usize)> {
    let count = t.cef_fail_count;
    assert!(count >= 2, "the setups need a back-off below the count");
    vec![
        (Kind::Starting, 0),
        (Kind::Starting, count - 1),
        (Kind::Ready, 0),
        (Kind::Ready, count - 1),
        (Kind::Backoff, 0),
        (Kind::Backoff, count - 2),
        (Kind::Failed, count - 1),
    ]
}

/// A machine in `kind` with `prior` failures in its window, built by crashing each page 10 ms after
/// its restart. Returns it with the harness's own failure stamps and the instant it entered `kind`.
fn graphics_in(kind_want: Kind, prior: usize, t: &Thresholds) -> (Graphics, Vec<Ms>, Ms) {
    let mut g = Graphics::new();
    let mut stamps = Vec::new();
    let mut now = 0;
    assert_eq!(g.on_tick(now, t), Some(Restart));
    for _ in 0..prior {
        now += 10;
        assert_eq!(
            g.on_crashed(now, g.generation(), t),
            None,
            "prior failures stay below the count"
        );
        stamps.push(now);
        now = due(g.state(), t);
        assert_eq!(g.on_tick(now, t), Some(Restart));
    }
    let entered = now + 50;
    match kind_want {
        Kind::Starting => {}
        Kind::Ready => g.on_ready(entered, g.generation()),
        Kind::Backoff | Kind::Failed => {
            let a = g.on_crashed(entered, g.generation(), t);
            assert_eq!(a == Some(Hide), kind_want == Kind::Failed, "setup {kind_want:?}");
            stamps.push(entered);
        }
    }
    assert_eq!(
        (kind(g.state()), g.generation()),
        (kind_want, prior as u64 + 1),
        "setup"
    );
    (g, stamps, entered)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GEvent {
    Tick,
    Ready,
    Crashed,
}

const G_EVENTS: [GEvent; 3] = [GEvent::Tick, GEvent::Ready, GEvent::Crashed];

fn apply(g: &mut Graphics, e: GEvent, now: Ms, gen: u64, t: &Thresholds) -> Option<GraphicsAction> {
    match e {
        GEvent::Tick => g.on_tick(now, t),
        GEvent::Ready => {
            g.on_ready(now, gen);
            None
        }
        GEvent::Crashed => g.on_crashed(now, gen, t),
    }
}

/// The state a failure at `at` must leave, given `n` failures in the window, this one included: the
/// count-th or later enters Failed for one window; any other waits the back-off.
fn after_failure(n: usize, at: Ms, t: &Thresholds) -> (GraphicsState, Option<GraphicsAction>) {
    if n >= t.cef_fail_count {
        let retry_at = at + t.cef_fail_window_ms;
        (GraphicsState::Failed { retry_at }, Some(Hide))
    } else {
        let until = at + wait(t, n - 1);
        let attempt = (n - 1) as u32;
        (GraphicsState::Backoff { attempt, until }, None)
    }
}

/// The brief's transitions (C05 Context and Step 3), written from its prose: the state and action
/// after `e` at `at`, from `from`, where `n` is the failures in the window if `e` is a failure.
/// One row is NOT the brief: `(Failed, Tick)` encodes C05's deliberate deviation (docs/_INDEX.md, the
/// C05 decision row), so this matrix is no independent witness of the brief on that row.
fn brief(
    from: GraphicsState,
    e: GEvent,
    at: Ms,
    n: usize,
    t: &Thresholds,
) -> (GraphicsState, Option<GraphicsAction>) {
    use GEvent as E;
    use Kind as K;
    let restart = (GraphicsState::Starting { since: at }, Some(Restart));
    match (kind(from), e) {
        (K::Backoff, E::Tick) => restart, // the back-off is over
        (K::Ready, E::Tick) => restart,   // the planned recycle
        // C05 DEVIATION, not Step 3: the brief went Failed → `Backoff { 0, now }` with no action and
        // restarted on the next tick; C05 restarts on the tick the cooldown ends (docs/_INDEX.md).
        (K::Failed, E::Tick) => restart,
        (K::Starting, E::Tick) => after_failure(n, at, t), // the load timed out
        (K::Starting | K::Ready, E::Crashed) => after_failure(n, at, t),
        (K::Starting, E::Ready) => (GraphicsState::Ready { since: at }, None),
        _ => (from, None), // everything else changes nothing
    }
}

/// Whether `e` from `from` is a failure the window must record.
fn is_failure(from: Kind, e: GEvent) -> bool {
    matches!(
        (from, e),
        (Kind::Starting, GEvent::Tick) | (Kind::Starting | Kind::Ready, GEvent::Crashed)
    )
}

// Ticks land at the state's due instant; ready and crash 1 ms after the state was entered, inside
// every window. Each setup holds its own failure history, so a failure lands both below the count
// and at it.
#[test]
fn every_graphics_pair_follows_the_brief() {
    let t = Thresholds::default();
    let (mut checked, mut moved, mut hides) = (0usize, 0usize, 0usize);
    for (k, prior) in setups(&t) {
        for e in G_EVENTS {
            let (mut g, mut stamps, entered) = graphics_in(k, prior, &t);
            let from = g.state();
            let gen = g.generation();
            let at = if e == GEvent::Tick {
                due(from, &t)
            } else {
                entered + 1
            };
            if is_failure(k, e) {
                stamps.push(at);
            }
            let n = within(&stamps, at, t.cef_fail_window_ms);
            let a = apply(&mut g, e, at, gen, &t);
            let want = brief(from, e, at, n, &t);
            assert_eq!((g.state(), a), want, "({k:?} prior {prior}, {e:?}) at {at}");
            let restarted = want.1 == Some(Restart);
            assert_eq!(g.generation(), gen + u64::from(restarted), "({k:?}, {e:?})");
            checked += 1;
            moved += usize::from(kind(want.0) != k);
            hides += usize::from(want.1 == Some(Hide));
        }
    }
    eprintln!("graphics pairs checked: {checked} ({moved} move the state, {hides} hide)");
    assert_eq!(checked, setups(&t).len() * G_EVENTS.len(), "every pair, once");
    assert!(
        moved > 0 && hides > 0,
        "a matrix where nothing moves or hides is vacuous"
    );
}

// A tick 1 ms before each timer is due, and every ready or crash from the previous generation and
// from one never built, change nothing in any state: not the state, the generation or the window.
#[test]
fn graphics_early_ticks_and_other_generations_change_nothing() {
    let t = Thresholds::default();
    let mut checked = 0usize;
    for (k, prior) in setups(&t) {
        let (mut g, _, entered) = graphics_in(k, prior, &t);
        let before = g.clone();
        assert_eq!(
            g.on_tick(due(g.state(), &t) - 1, &t),
            None,
            "{k:?}: acted 1 ms early"
        );
        assert_eq!(g, before, "{k:?}: early tick");
        checked += 1;
        for e in [GEvent::Ready, GEvent::Crashed] {
            for gen in [before.generation() - 1, before.generation() + 1] {
                let (mut g, _, _) = graphics_in(k, prior, &t);
                assert_eq!(apply(&mut g, e, entered + 1, gen, &t), None);
                assert_eq!(g, before, "({k:?}, {e:?}) from generation {gen}");
                checked += 1;
            }
        }
    }
    eprintln!("graphics no-ops checked: {checked}");
    assert_eq!(checked, setups(&t).len() * (1 + 2 * 2));
}

// ---- graphics: generated schedules, against facts the harness tracks itself ----

const CASES: u64 = 120;
const STEPS: usize = 1_500;

/// The next instant: 1 ms either side of the due timer, or a plain step forward (short ones for
/// failure bursts, long ones to cross the 5-min window).
fn advance(r: &mut Lcg, now: Ms, due: Ms) -> Ms {
    match r.next(4) {
        0 if due > now => due - 1 + r.next(3), // due - 1, due or due + 1
        _ => now + r.pick(&[0, 1, 33, 34, 120, 999, 2_000, 10_000, 60_000, 150_000]),
    }
}

#[derive(Debug, Default)]
struct GraphicsCounts {
    restarts_after_backoff: usize,
    recycles: usize,
    retries_after_failed: usize,
    quiet_ticks: usize,
    timeouts_to_backoff: usize,
    timeouts_to_failed: usize,
    crashes_loading: usize,
    crashes_ready: usize,
    crashes_to_failed: usize,
    readies: usize,
    readies_ignored: usize,
    crashes_ignored_backing_off: usize,
    crashes_ignored_failed: usize,
    stale: usize,
    waits_doubled: usize,
    failures_within_a_window_of_a_recycle: usize,
}

#[test]
fn graphics_keeps_its_rules_over_generated_schedules() {
    let t = Thresholds::default();
    let span = t.cef_fail_window_ms;
    let mut c = GraphicsCounts::default();
    for case in 0..CASES {
        let mut r = Lcg(case.wrapping_mul(7_919).wrapping_add(29));
        let mut g = Graphics::new();
        let mut now: Ms = 0;
        // Facts kept by the harness: its own failure stamps, the restarts it saw, the last recycle,
        // and whether the overlay is hidden.
        let mut stamps: Vec<Ms> = Vec::new();
        let mut restarts: u64 = 0;
        let mut recycled_at: Option<Ms> = None;
        let mut hidden = false;
        for _ in 0..STEPS {
            let before = g.state();
            now = advance(&mut r, now, due(before, &t));
            let gen = g.generation();
            assert_eq!(gen, restarts, "case {case}: one generation per restart");
            let k = kind(before);
            let (e, ev_gen) = match r.next(10) {
                0..=4 => (GEvent::Tick, gen),
                5..=6 => (GEvent::Ready, gen),
                7..=8 => (GEvent::Crashed, gen),
                _ => {
                    let other = if gen > 0 && r.next(2) == 0 {
                        gen - 1
                    } else {
                        gen + 1
                    };
                    (
                        if r.next(2) == 0 {
                            GEvent::Ready
                        } else {
                            GEvent::Crashed
                        },
                        other,
                    )
                }
            };
            let a = apply(&mut g, e, now, ev_gen, &t);
            if ev_gen != gen {
                assert_eq!(
                    (g.state(), a),
                    (before, None),
                    "case {case}: generation {ev_gen} while at {gen}"
                );
                c.stale += 1;
                continue;
            }
            let due_now = now >= due(before, &t);
            let failure = match (k, e) {
                (Kind::Starting, GEvent::Tick) => due_now,
                (Kind::Starting | Kind::Ready, GEvent::Crashed) => true,
                _ => false,
            };
            if failure {
                stamps.push(now);
            }
            let n = within(&stamps, now, span);
            let want = match (k, e) {
                (_, GEvent::Tick) if !due_now => (before, None),
                _ => brief(before, e, now, n, &t),
            };
            assert_eq!(
                (g.state(), a),
                want,
                "case {case}: ({k:?}, {e:?}) at {now}, {n} in the window"
            );
            match (k, e, a) {
                (_, GEvent::Tick, None) if !due_now => c.quiet_ticks += 1,
                (Kind::Backoff, GEvent::Tick, _) => c.restarts_after_backoff += 1,
                (Kind::Ready, GEvent::Tick, _) => {
                    c.recycles += 1;
                    recycled_at = Some(now);
                }
                (Kind::Failed, GEvent::Tick, _) => c.retries_after_failed += 1,
                (Kind::Starting, GEvent::Tick, None) => c.timeouts_to_backoff += 1,
                (Kind::Starting, GEvent::Tick, Some(_)) => c.timeouts_to_failed += 1,
                (Kind::Starting, GEvent::Crashed, _) => c.crashes_loading += 1,
                (Kind::Ready, GEvent::Crashed, _) => c.crashes_ready += 1,
                (Kind::Starting, GEvent::Ready, _) => c.readies += 1,
                (_, GEvent::Ready, _) => c.readies_ignored += 1,
                (Kind::Backoff, GEvent::Crashed, _) => c.crashes_ignored_backing_off += 1,
                (Kind::Failed, GEvent::Crashed, _) => c.crashes_ignored_failed += 1,
            }
            if failure {
                c.crashes_to_failed += usize::from(e == GEvent::Crashed && a == Some(Hide));
                c.waits_doubled += usize::from(a.is_none() && n >= 2);
                c.failures_within_a_window_of_a_recycle +=
                    usize::from(recycled_at.is_some_and(|at| now - at <= span));
            }
            match a {
                Some(Hide) => {
                    assert!(!hidden, "case {case}: a second hide with no restart between");
                    hidden = true;
                }
                Some(Restart) => {
                    restarts += 1;
                    hidden = false;
                }
                None => {}
            }
            assert_eq!(
                hidden,
                kind(g.state()) == Kind::Failed,
                "case {case}: hidden iff Failed"
            );
        }
    }
    eprintln!("graphics schedules: {CASES} cases × {STEPS} steps: {c:?}");
    let all = [
        ("restarts after a back-off", c.restarts_after_backoff),
        ("recycles", c.recycles),
        ("retries after Failed", c.retries_after_failed),
        ("quiet ticks", c.quiet_ticks),
        ("load timeouts that back off", c.timeouts_to_backoff),
        ("load timeouts that fail", c.timeouts_to_failed),
        ("crashes while loading", c.crashes_loading),
        ("crashes while ready", c.crashes_ready),
        ("crashes that fail", c.crashes_to_failed),
        ("readies", c.readies),
        ("readies ignored", c.readies_ignored),
        ("crashes ignored while backing off", c.crashes_ignored_backing_off),
        ("crashes ignored while Failed", c.crashes_ignored_failed),
        ("stale events", c.stale),
        ("doubled waits", c.waits_doubled),
        (
            "failures within a window of a recycle",
            c.failures_within_a_window_of_a_recycle,
        ),
    ];
    for (name, n) in all {
        assert!(n > 0, "never exercised: {name}");
    }
}

// ---- graphics: review focus 2 on the frame tick grid ----

// Plan, review focus 2: a score page that never signals ready (a deploy mid-match, a JS error, a
// 500). Under F-001's console readiness its frames still flow, so only the domain's load timeout
// catches it. On the real tick grid of both modes, at every phase: the page is restarted
// `cef_fail_count` times and hidden once, `cef_fail_count` load timeouts plus the back-offs between
// them after the first restart (each timed step waits for the next tick, so the hide is late by under
// one tick spacing per step), retried a cooldown after the hide, and hidden again after a fresh full
// set of attempts. Ready signals the machine must ignore arrive on every tick: one from the previous
// generation (an earlier attempt that loaded late), and, while backing off or Failed, one from the
// current generation (the timed-out page that loaded after its timeout). None may make it Ready, and
// the run counts what it offered, so "never Ready" is a claim that can fail.
#[test]
fn a_page_that_never_signals_ready_hides_on_the_frame_tick_grid() {
    let t = Thresholds::default();
    let count = t.cef_fail_count;
    let to_hide = count as Ms * t.cef_load_timeout_ms + (0..count - 1).map(|k| wait(&t, k)).sum::<Ms>();
    let (mut cases, mut ticked) = (0usize, 0usize);
    let (mut stale_offered, mut late_offered, mut ready_transitions) = (0usize, 0usize, 0usize);
    for mode in BOTH_MODES {
        let spacing = widest(mode);
        assert!(
            spacing == 33 || spacing == 34,
            "{mode:?}: 30 fps ticks are 33–34 ms apart, got {spacing}"
        );
        let timed_steps = (2 * count - 1) as Ms;
        let late = timed_steps * spacing;
        for phase in 0..spacing {
            let mut g = Graphics::new();
            let (mut restarts, mut hides) = (Vec::new(), Vec::new());
            let horizon = phase + 2 * (to_hide + late) + t.cef_fail_window_ms + spacing;
            for now in ticks(mode, phase) {
                assert!(
                    now <= horizon,
                    "{mode:?} φ{phase}: no second hide by {horizon} ms"
                );
                match g.on_tick(now, &t) {
                    Some(Restart) => restarts.push(now),
                    Some(Hide) => hides.push(now),
                    None => {}
                }
                let gen = g.generation();
                if gen >= 2 {
                    g.on_ready(now, gen - 1);
                    stale_offered += 1;
                }
                if matches!(kind(g.state()), Kind::Backoff | Kind::Failed) && gen >= 1 {
                    g.on_ready(now, gen);
                    late_offered += 1;
                }
                ticked += 1;
                if kind(g.state()) == Kind::Ready {
                    // Stop here, so this run's own check below reports it before any timing check.
                    ready_transitions += 1;
                    break;
                }
                if hides.len() == 2 {
                    break;
                }
            }
            let ctx = format!("{mode:?} φ{phase}: restarts {restarts:?}, hides {hides:?}");
            assert_eq!(
                ready_transitions, 0,
                "{ctx}: a never-ready page went Ready on an ignorable ready signal"
            );
            assert_eq!(restarts.len(), 2 * count, "{ctx}");
            assert_eq!(restarts[0], phase, "{ctx}: the first tick restarts");
            assert!(restarts[..count].iter().all(|&r| r < hides[0]), "{ctx}");
            let first = hides[0] - restarts[0];
            assert!(
                (to_hide..to_hide + late).contains(&first),
                "{ctx}: first hide after {first} ms"
            );
            let retry = restarts[count] - hides[0];
            let cooldown = t.cef_fail_window_ms;
            assert!(
                (cooldown..cooldown + spacing).contains(&retry),
                "{ctx}: retried after {retry} ms"
            );
            let second = hides[1] - restarts[count];
            assert!(
                (to_hide..to_hide + late).contains(&second),
                "{ctx}: second hide after {second} ms"
            );
            cases += 1;
        }
    }
    eprintln!(
        "never-ready runs checked: {cases} (mode × phase), {ticked} ticks driven; ready signals offered and \
         ignored: {stale_offered} from the previous generation, {late_offered} late from a timed-out page; \
         Ready transitions: {ready_transitions}; hide after {to_hide} ms + < one tick per timed step"
    );
    assert!(cases > 0 && ticked > 0, "checked 0 runs");
    assert!(
        stale_offered > 0 && late_offered > 0,
        "offered no ready to ignore: the Ready check is vacuous"
    );
    assert_eq!(
        ready_transitions, 0,
        "a never-ready page went Ready on an ignorable signal"
    );
}

// ---- the ladder ----

/// The rungs, top first: the owner ruling of 2026-10-18 (`docs/_INDEX.md` Owed 20) removed the third,
/// so the ladder is `Full → OverlayHalfRate` and the bottom holds.
const RUNGS: [Rung; 2] = [Rung::Full, Rung::OverlayHalfRate];

/// Exhaustive, so a new rung is a compile error here and `RUNGS` cannot go stale.
fn rung_index(r: Rung) -> usize {
    match r {
        Rung::Full => 0,
        Rung::OverlayHalfRate => 1,
    }
}

#[derive(Debug, Default)]
struct LadderCounts {
    below_count: usize,
    held_at_bottom: usize,
    held_at_bottom_a_minute_after_the_step: usize,
    steps_to_half_rate: usize,
}

// Each case is a run of overruns at generated spacings (bursts inside the 1 s window, gaps around
// it, long silences). The harness keeps its own overrun stamps and rung, and the ladder must step
// exactly when the brief says: enough overruns in the window, and a rung left below. Once on the
// bottom it never steps again, however long after the step the next full window comes.
#[test]
fn ladder_keeps_its_rules_over_generated_schedules() {
    let t = Thresholds::default();
    let (window, need) = (t.ladder_window_ms, t.ladder_overruns_to_step);
    let mut c = LadderCounts::default();
    for case in 0..CASES {
        let mut r = Lcg(case.wrapping_mul(104_729).wrapping_add(7));
        let mut l = Ladder::new();
        let mut now: Ms = 0;
        let mut stamps: Vec<Ms> = Vec::new();
        let mut stepped_at: Option<Ms> = None;
        let mut at = 0usize;
        for calls in 1..=STEPS as u64 {
            now += r.pick(&[
                0, 1, 10, 33, 333, 499, 500, 501, 999, 1_000, 1_001, 5_000, 9_999, 10_000, 10_001, 60_000,
            ]);
            let got = l.on_overrun(now, &t);
            stamps.push(now);
            let n = within(&stamps, now, window);
            let bottom = at + 1 == RUNGS.len();
            let want = (n >= need && !bottom).then(|| RUNGS[at + 1]);
            assert_eq!(
                got, want,
                "case {case}: overrun {calls} at {now}, {n} in the window"
            );
            match (n >= need, bottom) {
                (false, _) => c.below_count += 1,
                (true, true) => {
                    c.held_at_bottom += 1;
                    let since = now - stepped_at.expect("on the bottom only after the step");
                    c.held_at_bottom_a_minute_after_the_step += usize::from(since >= 60 * window);
                }
                (true, false) => {
                    c.steps_to_half_rate += 1;
                    at += 1;
                    stepped_at = Some(now);
                }
            }
            assert_eq!(
                rung_index(l.rung()),
                at,
                "case {case}: the rung only ever steps down, once"
            );
            assert_eq!(
                l.overruns(),
                calls,
                "case {case}: every overrun is a dropped frame"
            );
        }
    }
    eprintln!("ladder schedules: {CASES} cases × {STEPS} overruns: {c:?}");
    let all = [
        ("below the count", c.below_count),
        ("held at the bottom", c.held_at_bottom),
        (
            "held at the bottom a minute after the step",
            c.held_at_bottom_a_minute_after_the_step,
        ),
        ("steps to OverlayHalfRate", c.steps_to_half_rate),
    ];
    for (name, n) in all {
        assert!(n > 0, "never exercised: {name}");
    }
}

// Owner ruling 2026-10-18: repeated overrun bursts never step below `OverlayHalfRate`, and the bottom
// holds. Each case is a run of bursts, each with at least `ladder_overruns_to_step` overruns inside
// one `ladder_window_ms` (so each alone would step from `Full`), separated by generated silences
// from none to a day. The first burst steps at its `ladder_overruns_to_step`-th overrun; no later
// overrun steps, the rung stays the bottom, and every overrun is counted.
#[test]
fn repeated_overrun_bursts_never_step_below_overlay_half_rate() {
    let t = Thresholds::default();
    let (window, need) = (t.ladder_window_ms, t.ladder_overruns_to_step as Ms);
    let (mut cases, mut bursts_on_bottom, mut overruns_on_bottom) = (0usize, 0usize, 0u64);
    for case in 0..CASES {
        let mut r = Lcg(case.wrapping_mul(7_919).wrapping_add(11));
        let mut l = Ladder::new();
        let mut now: Ms = r.next(window);
        let mut calls = 0u64;
        let mut steps: Vec<(u64, Rung)> = Vec::new();
        for burst in 0..40 {
            if burst > 0 {
                now += r.pick(&[0, 1, window, window + 1, 10 * window, 60 * window, 86_400_000]);
            }
            let len = need + r.next(2 * need);
            // `len` overruns no more than `window` apart end to end, so the window holds them all.
            let spacing = window / len;
            for k in 0..len {
                calls += 1;
                if let Some(rung) = l.on_overrun(now + k * spacing, &t) {
                    steps.push((calls, rung));
                }
            }
            now += len * spacing;
            if burst > 0 {
                bursts_on_bottom += 1;
                overruns_on_bottom += len;
            }
            assert_eq!(l.rung(), Rung::OverlayHalfRate, "case {case}: burst {burst}");
        }
        assert_eq!(
            steps,
            vec![(need, Rung::OverlayHalfRate)],
            "case {case}: one step, at the first burst's step count"
        );
        assert_eq!(l.overruns(), calls, "case {case}: every overrun counted");
        cases += 1;
    }
    eprintln!(
        "repeated-burst runs checked: {cases}, {bursts_on_bottom} bursts and {overruns_on_bottom} \
         overruns after the step, 0 steps below OverlayHalfRate"
    );
    assert!(
        cases > 0 && bursts_on_bottom > 0 && overruns_on_bottom > 0,
        "checked 0 bursts on the bottom rung"
    );
}

// An encoder that drops every frame for a minute, on the tick grid of both modes at every phase:
// the one step at the `ladder_overruns_to_step`-th dropped frame, then nothing, the bottom held;
// every frame counted.
#[test]
fn an_encoder_dropping_every_frame_steps_once_on_the_frame_tick_grid() {
    let t = Thresholds::default();
    let need = t.ladder_overruns_to_step;
    let (mut cases, mut frames) = (0usize, 0u64);
    for mode in BOTH_MODES {
        let spacing = widest(mode);
        for phase in 0..spacing {
            let mut l = Ladder::new();
            let grid: Vec<Ms> = ticks(mode, phase).take_while(|&at| at < phase + 60_000).collect();
            let steps: Vec<(Ms, Rung)> = grid
                .iter()
                .filter_map(|&now| l.on_overrun(now, &t).map(|r| (now, r)))
                .collect();
            let ctx = format!("{mode:?} φ{phase}: {steps:?}");
            assert_eq!(steps, vec![(grid[need - 1], Rung::OverlayHalfRate)], "{ctx}");
            assert_eq!(l.rung(), Rung::OverlayHalfRate, "{ctx}");
            assert_eq!(l.overruns(), grid.len() as u64, "{ctx}");
            frames += grid.len() as u64;
            cases += 1;
        }
    }
    eprintln!("saturated-encoder runs checked: {cases} (mode × phase), {frames} dropped frames");
    assert!(cases > 0 && frames > 0, "checked 0 runs");
}
