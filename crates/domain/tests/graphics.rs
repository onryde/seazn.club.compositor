//! Sweeps over C05's graphics sub-machine (with C05b's hang detection) and degrade ladder: every
//! (state, event) pair against a matrix written from the briefs, both machines over generated
//! schedules against facts the harness tracks for itself, and, on the frame tick grid of both modes,
//! the plan's review focus 2 (a page that never signals ready), a page that stops beating, and an
//! encoder that drops every frame. They live here, not inline,
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

/// The instant a state's own timer is due: the load timeout, the recycle or the hang (C05b: whichever
/// comes first), the back-off's end, the cooldown's end.
fn due(s: GraphicsState, t: &Thresholds) -> Ms {
    match s {
        GraphicsState::Starting { since } => since + t.cef_load_timeout_ms,
        GraphicsState::Ready { since, last_alive } => {
            (since + t.cef_recycle_every_ms).min(last_alive + t.cef_hang_after_ms)
        }
        GraphicsState::Backoff { until, .. } => until,
        GraphicsState::Failed { retry_at } => retry_at,
    }
}

/// Whether a Ready page has been silent for `cef_hang_after_ms` at `at` (C05b: a hang).
fn hang_due(s: GraphicsState, at: Ms, t: &Thresholds) -> bool {
    matches!(s, GraphicsState::Ready { last_alive, .. } if at >= last_alive + t.cef_hang_after_ms)
}

/// How a Ready setup's page beat: which of its timers falls due first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Beat {
    /// No beat after the ready: the hang is due first (for a non-Ready setup, nothing beats).
    Silent,
    /// Beating up to 1 ms past `recycle − hang`, so the recycle is due 1 ms before the hang.
    Kept,
    /// The last beat exactly `cef_hang_after_ms` before the recycle: both are due on one tick.
    Both,
}

/// Every reachable (state, failures already in the window, beat) setup. Starting and Ready can hold
/// up to `cef_fail_count - 1` earlier failures; Backoff holds one fewer, since its own failure is in
/// the window too; Failed exists only at the count. A Ready setup comes in each `Beat`, so a hang lands
/// both below the count and at it, and both with and without the recycle due beside it.
fn setups(t: &Thresholds) -> Vec<(Kind, usize, Beat)> {
    let count = t.cef_fail_count;
    assert!(count >= 2, "the setups need a back-off below the count");
    let mut s = vec![
        (Kind::Starting, 0, Beat::Silent),
        (Kind::Starting, count - 1, Beat::Silent),
    ];
    for beat in [Beat::Kept, Beat::Silent, Beat::Both] {
        s.push((Kind::Ready, 0, beat));
        s.push((Kind::Ready, count - 1, beat));
    }
    s.extend([
        (Kind::Backoff, 0, Beat::Silent),
        (Kind::Backoff, count - 2, Beat::Silent),
        (Kind::Failed, count - 1, Beat::Silent),
    ]);
    s
}

/// A machine in `kind` with `prior` failures in its window, built by crashing each page 10 ms after
/// its restart, and beating as `beat` says if Ready. Returns it with the harness's own failure stamps
/// and the instant of its last setup event (the entry, or the Ready page's last beat).
fn graphics_in(kind_want: Kind, prior: usize, beat: Beat, t: &Thresholds) -> (Graphics, Vec<Ms>, Ms) {
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
    let mut last = entered;
    match kind_want {
        Kind::Starting => {}
        Kind::Ready => {
            g.on_ready(entered, g.generation());
            let (recycle, hang) = (t.cef_recycle_every_ms, t.cef_hang_after_ms);
            last = match beat {
                Beat::Silent => entered,
                Beat::Kept => entered + recycle - hang + 1,
                Beat::Both => entered + recycle - hang,
            };
            if last > entered {
                g.on_alive(last, g.generation());
            }
            let want = GraphicsState::Ready {
                since: entered,
                last_alive: last,
            };
            assert_eq!(
                g.state(),
                want,
                "setup Ready, {beat:?}: the harness's own instants"
            );
        }
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
    (g, stamps, last)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GEvent {
    Tick,
    Ready,
    Crashed,
    Alive,
}

const G_EVENTS: [GEvent; 4] = [GEvent::Tick, GEvent::Ready, GEvent::Crashed, GEvent::Alive];

fn apply(g: &mut Graphics, e: GEvent, now: Ms, gen: u64, t: &Thresholds) -> Option<GraphicsAction> {
    match e {
        GEvent::Tick => g.on_tick(now, t),
        GEvent::Ready => {
            g.on_ready(now, gen);
            None
        }
        GEvent::Crashed => g.on_crashed(now, gen, t),
        GEvent::Alive => {
            g.on_alive(now, gen);
            None
        }
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

/// The briefs' transitions (C05 Context and Step 3; C05b Rules), written from their prose: the state
/// and action after `e` at `at`, from `from`, where `n` is the failures in the window if `e` is a
/// failure. One row is NOT the C05 brief: `(Failed, Tick)` encodes C05's deliberate deviation
/// (docs/_INDEX.md, the C05 decision row), so this matrix is no independent witness of the brief there.
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
        // C05b: a hang is a counted failure and hides at once, below the count too; it wins over a
        // recycle that is due on the same tick.
        (K::Ready, E::Tick) if hang_due(from, at, t) => (after_failure(n, at, t).0, Some(Hide)),
        (K::Ready, E::Tick) => restart, // the planned recycle
        // C05 DEVIATION, not Step 3: the brief went Failed → `Backoff { 0, now }` with no action and
        // restarted on the next tick; C05 restarts on the tick the cooldown ends (docs/_INDEX.md).
        (K::Failed, E::Tick) => restart,
        (K::Starting, E::Tick) => after_failure(n, at, t), // the load timed out
        (K::Starting | K::Ready, E::Crashed) => after_failure(n, at, t),
        (K::Starting, E::Ready) => (
            GraphicsState::Ready {
                since: at,
                last_alive: at,
            },
            None,
        ),
        // C05b: a beat moves the hang deadline, never the recycle clock.
        (K::Ready, E::Alive) => match from {
            GraphicsState::Ready { since, .. } => (
                GraphicsState::Ready {
                    since,
                    last_alive: at,
                },
                None,
            ),
            _ => unreachable!("kind() said Ready"),
        },
        _ => (from, None), // everything else changes nothing
    }
}

/// Whether `e` at `at` from `from` is a failure the window must record (the tick only when its timer
/// is due: a load timeout, or a hang).
fn is_failure(from: GraphicsState, e: GEvent, at: Ms, t: &Thresholds) -> bool {
    match (kind(from), e) {
        (Kind::Starting, GEvent::Tick) => at >= due(from, t),
        (Kind::Ready, GEvent::Tick) => hang_due(from, at, t),
        (Kind::Starting | Kind::Ready, GEvent::Crashed) => true,
        _ => false,
    }
}

// Ticks land at the state's due instant; ready, crash and beat 1 ms after the setup's last event,
// inside every window. Each setup holds its own failure history, so a failure lands both below the
// count and at it; each Ready setup beats its own way, so the tick is a recycle, a hang, or both due.
#[test]
fn every_graphics_pair_follows_the_brief() {
    let t = Thresholds::default();
    let (mut checked, mut moved, mut hides, mut hang_hides) = (0usize, 0usize, 0usize, 0usize);
    for (k, prior, beat) in setups(&t) {
        for e in G_EVENTS {
            let (mut g, mut stamps, last) = graphics_in(k, prior, beat, &t);
            let from = g.state();
            let gen = g.generation();
            let at = if e == GEvent::Tick {
                due(from, &t)
            } else {
                last + 1
            };
            if is_failure(from, e, at, &t) {
                stamps.push(at);
            }
            let n = within(&stamps, at, t.cef_fail_window_ms);
            let a = apply(&mut g, e, at, gen, &t);
            let want = brief(from, e, at, n, &t);
            let ctx = format!("({k:?} prior {prior} {beat:?}, {e:?}) at {at}");
            assert_eq!((g.state(), a), want, "{ctx}");
            let restarted = want.1 == Some(Restart);
            assert_eq!(g.generation(), gen + u64::from(restarted), "{ctx}");
            checked += 1;
            moved += usize::from(kind(want.0) != k || want.0 != from);
            hides += usize::from(want.1 == Some(Hide));
            hang_hides += usize::from(e == GEvent::Tick && hang_due(from, at, &t) && a == Some(Hide));
        }
    }
    eprintln!(
        "graphics pairs checked: {checked} ({moved} move the state, {hides} hide, {hang_hides} of them a hang)"
    );
    assert_eq!(checked, setups(&t).len() * G_EVENTS.len(), "every pair, once");
    assert!(
        moved > 0 && hides > 0,
        "a matrix where nothing moves or hides is vacuous"
    );
    assert!(hang_hides > 0, "no hang hid: the C05b rows are vacuous");
}

// A tick 1 ms before each timer is due, and every ready, crash or beat from the previous generation
// and from one never built, change nothing in any state: not the state, the generation or the window.
#[test]
fn graphics_early_ticks_and_other_generations_change_nothing() {
    let t = Thresholds::default();
    let other = [GEvent::Ready, GEvent::Crashed, GEvent::Alive];
    let mut checked = 0usize;
    for (k, prior, beat) in setups(&t) {
        let (mut g, _, last) = graphics_in(k, prior, beat, &t);
        let before = g.clone();
        assert_eq!(
            g.on_tick(due(g.state(), &t) - 1, &t),
            None,
            "{k:?} {beat:?}: acted 1 ms early"
        );
        assert_eq!(g, before, "{k:?} {beat:?}: early tick");
        checked += 1;
        for e in other {
            for gen in [before.generation() - 1, before.generation() + 1] {
                let (mut g, _, _) = graphics_in(k, prior, beat, &t);
                assert_eq!(apply(&mut g, e, last + 1, gen, &t), None);
                assert_eq!(g, before, "({k:?} {beat:?}, {e:?}) from generation {gen}");
                checked += 1;
            }
        }
    }
    eprintln!("graphics no-ops checked: {checked}");
    assert_eq!(checked, setups(&t).len() * (1 + other.len() * 2));
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

/// The harness's model of a Ready page (C05b). It beats every `cef_alive_every_ms` plus a slip (CEF
/// children run at nice 10, so beats come late under load; the longest slip leaves a beat 1 ms
/// inside the threshold), and, unless `stop` is None, its last beat is exactly at `stop` and it is
/// silent after: a hang `cef_hang_after_ms` later. A stop at the ready itself never beats at all.
#[derive(Debug)]
struct Page {
    ready_at: Ms,
    last_beat: Ms,
    next_beat: Ms,
    stop: Option<Ms>,
}

impl Page {
    fn new(r: &mut Lcg, ready_at: Ms, t: &Thresholds) -> Self {
        let (recycle, hang) = (t.cef_recycle_every_ms, t.cef_hang_after_ms);
        let stop = match r.next(8) {
            0..=2 => None,                              // beats until it leaves Ready
            3 => Some(ready_at),                        // never beats: the ready is its last
            4 | 5 => Some(ready_at + r.next(4 * hang)), // stops soon after the ready
            _ => Some(ready_at + recycle - hang - r.pick(&[0, 0, 1, 999, 30_000])), // near the recycle
        };
        Self {
            ready_at,
            last_beat: ready_at,
            next_beat: ready_at + Self::spacing(r, t),
            stop,
        }
    }

    fn spacing(r: &mut Lcg, t: &Thresholds) -> Ms {
        let (every, hang) = (t.cef_alive_every_ms, t.cef_hang_after_ms);
        every + r.pick(&[0, 0, 0, 1, 33, every / 4, every / 2, hang - every - 1])
    }

    /// The next beat at or before `now`, if the page has one: its schedule, ending exactly at `stop`.
    fn beat_by(&mut self, r: &mut Lcg, now: Ms, t: &Thresholds) -> Option<Ms> {
        let b = match self.stop {
            Some(s) if self.last_beat >= s => return None, // silent since its last beat
            Some(s) => self.next_beat.min(s),
            None => self.next_beat,
        };
        if b > now {
            return None;
        }
        self.last_beat = b;
        self.next_beat = b + Self::spacing(r, t);
        Some(b)
    }

    /// The instant this page's Ready ends on its own if nothing else happens: the recycle, or the hang
    /// after its last beat. The generator aims at it, so recycles and hangs both come up.
    fn deadline(&self, t: &Thresholds) -> Ms {
        let recycle = self.ready_at + t.cef_recycle_every_ms;
        match self.stop {
            None => recycle,
            Some(s) => recycle.min(s.max(self.last_beat) + t.cef_hang_after_ms),
        }
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
    beats: usize,
    beats_ignored_not_ready: usize,
    beats_after_a_hang: usize,
    hangs: usize,
    hangs_to_failed: usize,
    hang_hides_lifted_by_ready: usize,
    beaten_ready_ticks: usize,
    hang_at_recycle: usize,
}

/// What the harness knows about a hide it has seen: the generation hidden, and whether a hang hid it.
#[derive(Debug, Clone, Copy)]
struct Hidden {
    gen: u64,
    by_hang: bool,
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
        // the Ready page it is driving, whether the overlay is hidden (C05b: a `Hide` sets it, the
        // Ready of a generation newer than the one hidden clears it, nothing else touches it), and
        // whether the page now backing off or Failed left Ready by a hang.
        let mut stamps: Vec<Ms> = Vec::new();
        let mut restarts: u64 = 0;
        let mut recycled_at: Option<Ms> = None;
        let mut page: Option<Page> = None;
        let mut hidden: Option<Hidden> = None;
        let mut hid_since_restart = false;
        let mut left_by_hang = false;
        for _ in 0..STEPS {
            let aim = match &page {
                Some(p) => p.deadline(&t),
                None => due(g.state(), &t),
            };
            now = advance(&mut r, now, aim);
            let gen = g.generation();
            assert_eq!(gen, restarts, "case {case}: one generation per restart");
            // The Ready page's beats up to `now`, in order, before this step's event.
            if let Some(p) = page.as_mut() {
                while let Some(b) = p.beat_by(&mut r, now, &t) {
                    g.on_alive(b, gen);
                    let want = GraphicsState::Ready {
                        since: p.ready_at,
                        last_alive: b,
                    };
                    assert_eq!(g.state(), want, "case {case}: the beat at {b}");
                    c.beats += 1;
                }
            }
            let before = g.state();
            let k = kind(before);
            match (&page, before) {
                (Some(p), GraphicsState::Ready { since, last_alive }) => assert_eq!(
                    (since, last_alive),
                    (p.ready_at, p.last_beat),
                    "case {case}: the machine's Ready is the harness's own"
                ),
                (None, GraphicsState::Ready { .. }) => panic!("case {case}: Ready with no page"),
                (Some(_), other) => panic!("case {case}: a page, but {other:?}"),
                (None, _) => {}
            }
            let (e, ev_gen) = match r.next(12) {
                0..=4 => (GEvent::Tick, gen),
                5..=6 => (GEvent::Ready, gen),
                7..=8 => (GEvent::Crashed, gen),
                9 => (GEvent::Alive, gen),
                _ => {
                    let other = if gen > 0 && r.next(2) == 0 {
                        gen - 1
                    } else {
                        gen + 1
                    };
                    (r.pick(&[GEvent::Ready, GEvent::Crashed, GEvent::Alive]), other)
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
            let hang = e == GEvent::Tick && hang_due(before, now, &t);
            let failure = is_failure(before, e, now, &t);
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
                (_, GEvent::Tick, None) if !due_now => {
                    c.quiet_ticks += 1;
                    // A quiet Ready tick the ready alone would have hung: a beat kept the page.
                    if let (Kind::Ready, Some(p)) = (k, &page) {
                        let kept = now >= p.ready_at + t.cef_hang_after_ms;
                        assert!(
                            !kept || p.last_beat > p.ready_at,
                            "case {case}: kept with no beat"
                        );
                        c.beaten_ready_ticks += usize::from(kept);
                    }
                }
                (Kind::Backoff, GEvent::Tick, _) => c.restarts_after_backoff += 1,
                (Kind::Ready, GEvent::Tick, _) if hang => {
                    c.hangs += 1;
                    c.hangs_to_failed += usize::from(kind(g.state()) == Kind::Failed);
                    let recycle_due = matches!(before, GraphicsState::Ready { since, .. }
                        if now >= since + t.cef_recycle_every_ms);
                    c.hang_at_recycle += usize::from(recycle_due);
                }
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
                (Kind::Ready, GEvent::Alive, _) => {
                    // A beat off the page's schedule (a late one): the page is silent again after it
                    // if it had stopped.
                    let p = page.as_mut().expect("Ready has a page");
                    p.last_beat = now;
                    c.beats += 1;
                }
                (_, GEvent::Alive, _) => {
                    c.beats_ignored_not_ready += 1;
                    c.beats_after_a_hang += usize::from(left_by_hang);
                }
            }
            if failure {
                c.crashes_to_failed += usize::from(e == GEvent::Crashed && a == Some(Hide));
                c.waits_doubled += usize::from(a.is_none() && n >= 2);
                c.failures_within_a_window_of_a_recycle +=
                    usize::from(recycled_at.is_some_and(|at| now - at <= span));
            }
            // The page the harness drives: made at the machine's Ready, gone when it leaves Ready.
            let after = kind(g.state());
            if k == Kind::Starting && after == Kind::Ready {
                page = Some(Page::new(&mut r, now, &t));
                if let Some(h) = hidden.take() {
                    assert!(
                        g.generation() > h.gen,
                        "case {case}: the hidden generation {} went Ready",
                        h.gen
                    );
                    c.hang_hides_lifted_by_ready += usize::from(h.by_hang);
                }
            } else if after != Kind::Ready {
                page = None;
            }
            match a {
                Some(Hide) => {
                    assert!(
                        !hid_since_restart,
                        "case {case}: a second hide with no restart between"
                    );
                    hid_since_restart = true;
                    assert!(
                        matches!(after, Kind::Backoff | Kind::Failed),
                        "case {case}: hidden, but {:?}",
                        g.state()
                    );
                    let by_hang = hidden.is_some_and(|h| h.by_hang) || hang;
                    hidden = Some(Hidden { gen, by_hang });
                }
                Some(Restart) => {
                    restarts += 1;
                    hid_since_restart = false;
                    left_by_hang = false;
                }
                None => {}
            }
            left_by_hang |= hang;
            // C05b's invariant, replacing "hidden iff Failed": Failed is always hidden, and a hidden
            // overlay is never on a Ready page (only a newer generation's Ready lifts it, above).
            assert!(
                after != Kind::Failed || hidden.is_some(),
                "case {case}: Failed but not hidden"
            );
            assert!(
                after != Kind::Ready || hidden.is_none(),
                "case {case}: hidden on a Ready page"
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
        ("beats", c.beats),
        ("beats ignored while not Ready", c.beats_ignored_not_ready),
        ("beats after a hang was declared", c.beats_after_a_hang),
        ("hangs", c.hangs),
        ("hangs that fail", c.hangs_to_failed),
        (
            "hang hides lifted by the next Ready",
            c.hang_hides_lifted_by_ready,
        ),
        ("Ready ticks a beat kept from hanging", c.beaten_ready_ticks),
        ("hangs with the recycle due beside them", c.hang_at_recycle),
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

// C05b on the frame tick grid of both modes, at every phase: a page that is ready off the grid, beats
// every `cef_alive_every_ms` for 30 s on its own clock (between ticks), then stops, is hidden on the
// first tick at or after `cef_hang_after_ms` past its last beat (late by under one tick spacing: C14
// case 11's "± one tick"), and restarted on the first tick at or after the hide plus
// `cef_backoff_base_ms` (C06 and C08's hang timing). Nothing acts between its first restart and the
// hide, and each run counts the beats it delivered, so "never hung while beating" can fail.
#[test]
fn a_page_that_stops_beating_hides_on_the_frame_tick_grid() {
    let t = Thresholds::default();
    let (every, hang, base) = (t.cef_alive_every_ms, t.cef_hang_after_ms, t.cef_backoff_base_ms);
    let (mut cases, mut beats_delivered) = (0usize, 0usize);
    for mode in BOTH_MODES {
        let spacing = widest(mode);
        for phase in 0..spacing {
            let mut g = Graphics::new();
            let ready = phase + t.cef_load_timeout_ms / 2 + 7; // off the grid
            let beats: Vec<Ms> = (1..=30_000 / every).map(|k| ready + k * every).collect();
            let last = *beats.last().unwrap();
            let mut next = 0usize;
            let mut is_ready = false;
            let mut acts = Vec::new();
            let horizon = last + hang + base + 2 * spacing;
            for now in ticks(mode, phase) {
                assert!(now <= horizon, "{mode:?} φ{phase}: no restart by {horizon} ms");
                if !is_ready && now >= ready {
                    g.on_ready(ready, 1);
                    is_ready = true;
                }
                while next < beats.len() && beats[next] <= now {
                    g.on_alive(beats[next], 1);
                    next += 1;
                    beats_delivered += 1;
                }
                if let Some(a) = g.on_tick(now, &t) {
                    acts.push((now, a));
                }
                if acts.len() == 3 {
                    break;
                }
            }
            let ctx = format!("{mode:?} φ{phase}: {acts:?}");
            assert_eq!(next, beats.len(), "{ctx}: every beat delivered");
            let kinds: Vec<GraphicsAction> = acts.iter().map(|&(_, a)| a).collect();
            assert_eq!(kinds, vec![Restart, Hide, Restart], "{ctx}");
            assert_eq!(acts[0].0, phase, "{ctx}: the first tick restarts");
            let hidden = acts[1].0;
            assert!(
                (last + hang..last + hang + spacing).contains(&hidden),
                "{ctx}: hidden {} ms after the last beat",
                hidden - last
            );
            let restarted = acts[2].0;
            assert!(
                (hidden + base..hidden + base + spacing).contains(&restarted),
                "{ctx}: restarted {} ms after the hide",
                restarted - hidden
            );
            assert_eq!(g.generation(), 2, "{ctx}");
            cases += 1;
        }
    }
    eprintln!(
        "stopped-beat runs checked: {cases} (mode × phase), {beats_delivered} beats delivered; hide within one \
         tick of the last beat + {hang} ms, restart within one tick of the hide + {base} ms"
    );
    assert!(cases > 0 && beats_delivered > 0, "checked 0 runs or 0 beats");
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
