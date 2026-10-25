//! Sweeps over C05's graphics sub-machine (with C05b's hang detection and the bound on a frame's
//! age) and degrade ladder: every (state, event) pair against a matrix written from the briefs, both
//! machines over generated schedules against facts the harness tracks for itself, and, on the frame
//! tick grid of both modes, the plan's review focus 2 (a page that never signals ready), a page that
//! stops beating, a crashed or recycled page whose reloads never signal ready, and an encoder that
//! drops every frame. They live here, not inline, because the domain's crate root forbids
//! `clippy::print_stderr`, and a sweep must print the count it checked (AGENTS.md §5).
use domain::config::{OutputProfile, Thresholds};
use domain::graphics::{Graphics, GraphicsAction, GraphicsState, HideReason};
use domain::ladder::{Ladder, Rung};
use domain::time::Ms;

use GraphicsAction::{Hide, Restart};

/// The hide C06 maps to `HideOverlay { reason, gen }` (C12: `overlay hidden reason=<r> gen=<g>`).
fn hide(reason: HideReason, gen: u64) -> GraphicsAction {
    Hide { reason, gen }
}

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

/// Exhaustive, so a new reason is a compile error here and the per-reason counts cannot go stale.
fn reason_index(r: HideReason) -> usize {
    match r {
        HideReason::Hang => 0,
        HideReason::Failed => 1,
        HideReason::Crash => 2,
        HideReason::Recycle => 3,
        HideReason::Reload => 4,
    }
}

const REASONS: [HideReason; 5] = [
    HideReason::Hang,
    HideReason::Failed,
    HideReason::Crash,
    HideReason::Recycle,
    HideReason::Reload,
];

/// The instant a state's own timer is due: the load timeout, the recycle, the back-off's end, the
/// cooldown's end. A Ready page's hang is the bound of its frame (`bound`), not a timer of its own.
fn due(s: GraphicsState, t: &Thresholds) -> Ms {
    match s {
        GraphicsState::Starting { since } => since + t.cef_load_timeout_ms,
        GraphicsState::Ready { since, .. } => since + t.cef_recycle_every_ms,
        GraphicsState::Backoff { until, .. } => until,
        GraphicsState::Failed { retry_at } => retry_at,
    }
}

/// The harness's own model of what of the machine's pages is on air, written from the rules (C05b's
/// Hide bullet; the bound, coordinator decision 2026-10-18), never read from the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Air {
    /// Nothing: before the first Ready, and from every hide until the next Ready.
    Nothing,
    /// The Ready page's own frames: their bound runs from its last beat (its `last_alive`).
    Live,
    /// Generation `gen`'s last frame, on air through a reload after `ended` (`Crash`, `Recycle`, or
    /// `Reload` after a GPU-process death) ended that page's life while it was Ready; `proof` was its
    /// last proof of life.
    Held { gen: u64, proof: Ms, ended: HideReason },
}

/// The frame on air with `air` while the machine is in `s` at generation `gen`: (its page, that
/// page's last proof of life, the reason a hide at its bound names).
fn frame(s: GraphicsState, air: Air, gen: u64) -> Option<(u64, Ms, HideReason)> {
    match (air, s) {
        (Air::Live, GraphicsState::Ready { last_alive, .. }) => Some((gen, last_alive, HideReason::Hang)),
        (Air::Live, other) => panic!("the harness's Live air beside {other:?}"),
        (Air::Held { gen, proof, ended }, _) => Some((gen, proof, ended)),
        (Air::Nothing, _) => None,
    }
}

/// The instant the frame on air reaches its bound: `cef_hang_after_ms` after its page's last proof
/// of life.
fn bound(s: GraphicsState, air: Air, t: &Thresholds) -> Option<Ms> {
    frame(s, air, 0).map(|(_, proof, _)| proof + t.cef_hang_after_ms)
}

/// Whether a tick at `at` finds the frame on air at or past its bound.
fn bound_due(s: GraphicsState, air: Air, at: Ms, t: &Thresholds) -> bool {
    bound(s, air, t).is_some_and(|end| at >= end)
}

/// The first instant anything is due: the state's own timer, or the bound.
fn due_with(s: GraphicsState, air: Air, t: &Thresholds) -> Ms {
    bound(s, air, t).map_or(due(s, t), |end| end.min(due(s, t)))
}

/// What is on air once `e`, from `from` at generation `gen` with `air`, moved the machine to
/// `to`: a hide takes everything off; the current generation's Ready shows its own frames; a crash
/// below the count, a recycle or a GPU-process death's reload ends a Ready page's life with its last
/// frame on air, held from its last beat; anything else (a reload's crash or load timeout, or a
/// loading page's reload, among it) leaves the frame on air as it was. Only the current generation's
/// events reach here.
fn air_after(
    air: Air,
    from: GraphicsState,
    e: GEvent,
    gen: u64,
    to: &(GraphicsState, Option<GraphicsAction>),
) -> Air {
    let ended = |how| match from {
        GraphicsState::Ready { last_alive, .. } => Air::Held {
            gen,
            proof: last_alive,
            ended: how,
        },
        other => panic!("only a Ready page's life ends: {other:?}"),
    };
    match (kind(from), e, to.1) {
        (_, _, Some(Hide { .. })) => Air::Nothing,
        (Kind::Starting, GEvent::Ready, _) => Air::Live,
        (Kind::Ready, GEvent::Crashed, _) => ended(HideReason::Crash),
        (Kind::Ready, GEvent::Tick, Some(Restart)) => ended(HideReason::Recycle),
        (Kind::Ready, GEvent::GpuDeath, Some(Restart)) => ended(HideReason::Reload),
        _ => air,
    }
}

/// How many of each `probe_air` check ran.
#[derive(Debug, Default)]
struct Probes {
    live: usize,
    held_crash: usize,
    held_recycle: usize,
    held_reload: usize,
    early: usize,
    off: usize,
}

/// Checks the machine's own record of what is on air against the harness's `air` at `at`, on clones,
/// so the run goes on unchanged. The machine exposes none of it, so each probe drives what the record
/// decides (PR #22 review M1: the harness's own hidden flag could never fail):
/// - A frame on air: a tick at its bound hides it, naming its path and its page (the bound is
///   checked first), and a tick 1 ms before hides nothing of it (a load timeout's `Failed` may come).
/// - Nothing on air: a tick `cef_hang_after_ms` later hides nothing at a bound, so the machine holds
///   no frame the harness does not.
fn probe_air(g: &Graphics, air: Air, at: Ms, t: &Thresholds, p: &mut Probes, ctx: &str) {
    let s = g.state();
    assert_eq!(
        kind(s) == Kind::Ready,
        air == Air::Live,
        "{ctx}: the harness's {air:?} beside {s:?}"
    );
    let at_a_bound =
        |a: Option<GraphicsAction>| matches!(a, Some(Hide { reason, .. }) if reason != HideReason::Failed);
    match frame(s, air, g.generation()) {
        Some((page, proof, reason)) => {
            let end = proof + t.cef_hang_after_ms;
            let a = g.clone().on_tick(end.max(at), t);
            assert_eq!(
                a,
                Some(hide(reason, page)),
                "{ctx}: probe, the bound at {end} of {air:?}"
            );
            if end > at {
                let a = g.clone().on_tick(end - 1, t);
                assert!(
                    !at_a_bound(a),
                    "{ctx}: probe, {air:?} hidden 1 ms before its bound: {a:?}"
                );
                p.early += 1;
            }
            match reason {
                HideReason::Hang => p.live += 1,
                HideReason::Crash => p.held_crash += 1,
                HideReason::Recycle => p.held_recycle += 1,
                HideReason::Reload => p.held_reload += 1,
                HideReason::Failed => panic!("{ctx}: a frame held as Failed"),
            }
        }
        None => {
            let a = g.clone().on_tick(at + t.cef_hang_after_ms, t);
            assert!(!at_a_bound(a), "{ctx}: probe, nothing on air, but {a:?}");
            p.off += 1;
        }
    }
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

/// How a setup's earlier pages reached air.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// No page was Ready before the setup's own state (a Ready setup shows its own page).
    Never,
    /// Generation 1 was Ready and recycled 1 ms after its last beat, so its last frame is held.
    Recycled,
    /// The first failure is a crash of a Ready page, so its frame is held.
    Crashed,
    /// Generation 1 was Ready and reloaded after a GPU-process death a beat's spacing less 1 ms after
    /// its ready (Owed 96), so its last frame is held.
    Reloaded,
}

/// Every reachable (state, failures already in the window, beat, shown) setup. Starting and Ready
/// can hold up to `cef_fail_count - 1` earlier failures; Backoff holds one fewer, since its own
/// failure is in the window too; Failed exists only at the count, with nothing on air. A Ready setup
/// comes in each `Beat`, so a hang lands both below the count and at it, and both with and without
/// the recycle due beside it. A loading or backing-off setup comes with nothing on air, a recycled
/// or a reloaded page's frame held, and a crashed page's frame held (which needs one failure, the
/// crash, in its window), so a crash lands on each and each bound ends both below the count and at
/// it.
fn setups(t: &Thresholds) -> Vec<(Kind, usize, Beat, Shown)> {
    let count = t.cef_fail_count;
    assert!(count >= 2, "the setups need a back-off below the count");
    let mut s = Vec::new();
    for shown in [Shown::Never, Shown::Recycled, Shown::Reloaded] {
        s.push((Kind::Starting, 0, Beat::Silent, shown));
        s.push((Kind::Starting, count - 1, Beat::Silent, shown));
    }
    s.push((Kind::Starting, 1, Beat::Silent, Shown::Crashed));
    s.push((Kind::Starting, count - 1, Beat::Silent, Shown::Crashed));
    for beat in [Beat::Kept, Beat::Silent, Beat::Both] {
        s.push((Kind::Ready, 0, beat, Shown::Never));
        s.push((Kind::Ready, count - 1, beat, Shown::Never));
    }
    for shown in [Shown::Never, Shown::Recycled, Shown::Crashed, Shown::Reloaded] {
        s.push((Kind::Backoff, 0, Beat::Silent, shown));
        s.push((Kind::Backoff, count - 2, Beat::Silent, shown));
    }
    s.push((Kind::Failed, count - 1, Beat::Silent, Shown::Never));
    s
}

/// One failure of the current page, which restarted at `now`: a crash 5 ms after its ready if
/// `ready_first`, or else a crash 10 ms after its restart (a reload's crash, which keeps the frame
/// on air as it was). Records the stamp and the harness's `Air`, and returns the failure's instant
/// and action.
fn setup_failure(
    g: &mut Graphics,
    stamps: &mut Vec<Ms>,
    air: &mut Air,
    now: Ms,
    ready_first: bool,
    t: &Thresholds,
) -> (Ms, Option<GraphicsAction>) {
    let gen = g.generation();
    if ready_first {
        g.on_ready(now + 5, gen);
        *air = Air::Live;
    }
    let from = g.state();
    let at = now + 10;
    let a = apply(g, GEvent::Crashed, at, gen, t);
    stamps.push(at);
    *air = air_after(*air, from, GEvent::Crashed, gen, &(g.state(), a));
    (at, a)
}

/// A machine in `kind` with `prior` failures in its window and its earlier pages shown as `shown`,
/// beating as `beat` says if Ready (see `setup_failure` for how each failure comes; each is followed
/// by its restart). Returns it with the harness's own failure stamps, the instant of its last setup
/// event (the entry, the failure, or the Ready page's last beat), and the harness's own `Air`.
fn graphics_in(
    kind_want: Kind,
    prior: usize,
    beat: Beat,
    shown: Shown,
    t: &Thresholds,
) -> (Graphics, Vec<Ms>, Ms, Air) {
    let mut g = Graphics::new();
    let mut stamps = Vec::new();
    let mut air = Air::Nothing;
    let mut now = 0;
    assert_eq!(g.on_tick(now, t), Some(Restart));
    if shown == Shown::Recycled {
        g.on_ready(now + 50, 1);
        let recycle = now + 50 + t.cef_recycle_every_ms;
        g.on_alive(recycle - 1, 1);
        assert_eq!(g.on_tick(recycle, t), Some(Restart), "setup: the recycle");
        air = Air::Held {
            gen: 1,
            proof: recycle - 1,
            ended: HideReason::Recycle,
        };
        now = recycle;
    }
    if shown == Shown::Reloaded {
        g.on_ready(now + 50, 1);
        let death = now + 50 + t.cef_alive_every_ms - 1;
        assert_eq!(g.on_gpu_death(death, t), Some(Restart), "setup: the reload");
        air = Air::Held {
            gen: 1,
            proof: now + 50,
            ended: HideReason::Reload,
        };
        now = death;
    }
    for i in 0..prior {
        let ready_first = shown == Shown::Crashed && i == 0;
        let (_, a) = setup_failure(&mut g, &mut stamps, &mut air, now, ready_first, t);
        assert_eq!(a, None, "setup: prior failures stay below the count");
        now = due(g.state(), t);
        assert_eq!(
            g.on_tick(now, t),
            Some(Restart),
            "setup: each restart comes before any bound"
        );
    }
    let gen = g.generation();
    let last = match kind_want {
        Kind::Starting => now + 50,
        Kind::Ready => {
            let entered = now + 50;
            g.on_ready(entered, gen);
            air = Air::Live;
            let (recycle, hang) = (t.cef_recycle_every_ms, t.cef_hang_after_ms);
            let last = match beat {
                Beat::Silent => entered,
                Beat::Kept => entered + recycle - hang + 1,
                Beat::Both => entered + recycle - hang,
            };
            if last > entered {
                g.on_alive(last, gen);
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
            last
        }
        Kind::Backoff | Kind::Failed => {
            let ready_first = shown == Shown::Crashed && prior == 0;
            let (at, a) = setup_failure(&mut g, &mut stamps, &mut air, now, ready_first, t);
            let want = (kind_want == Kind::Failed).then(|| hide(HideReason::Failed, gen));
            assert_eq!(a, want, "setup {kind_want:?}");
            at
        }
    };
    let gens = prior as u64 + 1 + u64::from(matches!(shown, Shown::Recycled | Shown::Reloaded));
    assert_eq!(
        (kind(g.state()), g.generation()),
        (kind_want, gens),
        "setup {shown:?}"
    );
    let air_ok = match (kind_want, shown) {
        (Kind::Ready, _) => air == Air::Live,
        (_, Shown::Never) => air == Air::Nothing,
        (_, Shown::Recycled) => matches!(
            air,
            Air::Held {
                ended: HideReason::Recycle,
                ..
            }
        ),
        (_, Shown::Crashed) => matches!(
            air,
            Air::Held {
                ended: HideReason::Crash,
                ..
            }
        ),
        (_, Shown::Reloaded) => matches!(
            air,
            Air::Held {
                ended: HideReason::Reload,
                ..
            }
        ),
    };
    assert!(air_ok, "setup {kind_want:?} {shown:?}: {air:?}");
    (g, stamps, last, air)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GEvent {
    Tick,
    Ready,
    Crashed,
    Alive,
    /// The process counted a GPU-process death below the in-place restart's count (Owed 96): a fact
    /// about the process, so it carries no generation.
    GpuDeath,
}

const G_EVENTS: [GEvent; 5] = [
    GEvent::Tick,
    GEvent::Ready,
    GEvent::Crashed,
    GEvent::Alive,
    GEvent::GpuDeath,
];

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
        GEvent::GpuDeath => g.on_gpu_death(now, t),
    }
}

/// The state a failure of generation `gen` at `at` must leave, given `n` failures in the window, this
/// one included: the count-th or later enters Failed for one window and hides, naming `gen`; any
/// other waits the back-off.
fn after_failure(n: usize, at: Ms, gen: u64, t: &Thresholds) -> (GraphicsState, Option<GraphicsAction>) {
    if n >= t.cef_fail_count {
        let retry_at = at + t.cef_fail_window_ms;
        (
            GraphicsState::Failed { retry_at },
            Some(hide(HideReason::Failed, gen)),
        )
    } else {
        let until = at + wait(t, n - 1);
        let attempt = (n - 1) as u32;
        (GraphicsState::Backoff { attempt, until }, None)
    }
}

/// The briefs' transitions (C05 Context and Step 3; C05b Rules; the bound, coordinator decision
/// 2026-10-18), written from their prose: the state and action after `e` at `at`, from `from` at
/// generation `gen` with `air` on air, where `n` is the failures in the window if `e` is a failure. A
/// hide names its reason (PR #22 review I2: a hang is `Hang` at the count too; a crash or load timeout
/// entering Failed is `Failed`, naming the generation that failed; a crashed, recycled or reloaded
/// page's frame at its bound is `Crash`, `Recycle` or `Reload`, naming that page). A GPU-process
/// death reloads a Ready or loading page at once, records no failure (Owed 96, owner ruling
/// 2026-10-25 "A"), and never launders a Ready page's hang: at its bound it changes nothing. One row is NOT the C05 brief:
/// `(Failed, Tick)` encodes C05's deliberate deviation (docs/_INDEX.md, the C05 decision row), so
/// this matrix is no independent witness of the brief there.
fn brief(
    from: GraphicsState,
    air: Air,
    e: GEvent,
    at: Ms,
    n: usize,
    gen: u64,
    t: &Thresholds,
) -> (GraphicsState, Option<GraphicsAction>) {
    use GEvent as E;
    use Kind as K;
    let restart = (GraphicsState::Starting { since: at }, Some(Restart));
    match (kind(from), e) {
        // C05b: the Ready page's own frame at its bound is a hang, a counted failure, hidden at once,
        // below the count too; it wins over a recycle that is due on the same tick.
        (K::Ready, E::Tick) if bound_due(from, air, at, t) => {
            (after_failure(n, at, gen, t).0, Some(hide(HideReason::Hang, gen)))
        }
        // A crashed or recycled page's frame at its bound: off air, and nothing else moves; a timer
        // due on the same tick follows on the next one.
        (_, E::Tick) if bound_due(from, air, at, t) => match air {
            Air::Held { gen: page, ended, .. } => (from, Some(hide(ended, page))),
            other => panic!("a bound beside {other:?} out of Ready"),
        },
        (K::Backoff, E::Tick) => restart, // the back-off is over
        (K::Ready, E::Tick) => restart,   // the planned recycle
        // C05 DEVIATION, not Step 3: the brief went Failed → `Backoff { 0, now }` with no action and
        // restarted on the next tick; C05 restarts on the tick the cooldown ends (docs/_INDEX.md).
        (K::Failed, E::Tick) => restart,
        (K::Starting, E::Tick) => after_failure(n, at, gen, t), // the load timed out
        (K::Starting | K::Ready, E::Crashed) => after_failure(n, at, gen, t),
        (K::Ready, E::GpuDeath) if bound_due(from, air, at, t) => (from, None),
        (K::Starting | K::Ready, E::GpuDeath) => restart,
        (K::Starting, E::Ready) => (
            GraphicsState::Ready {
                since: at,
                last_alive: at,
            },
            None,
        ),
        // C05b: a beat moves the bound, never the recycle clock, and never earlier.
        (K::Ready, E::Alive) => match from {
            GraphicsState::Ready { since, last_alive } => (
                GraphicsState::Ready {
                    since,
                    last_alive: at.max(last_alive),
                },
                None,
            ),
            _ => unreachable!("kind() said Ready"),
        },
        _ => (from, None), // everything else changes nothing
    }
}

/// Whether `e` at `at` from `from` with `air` is a failure the window must record (the tick only when
/// its timer is due and no held frame's bound comes first: a load timeout; or the Ready page's own
/// bound: a hang).
fn is_failure(from: GraphicsState, air: Air, e: GEvent, at: Ms, t: &Thresholds) -> bool {
    match (kind(from), e) {
        (Kind::Ready, GEvent::Tick) => bound_due(from, air, at, t),
        (_, GEvent::Tick) if bound_due(from, air, at, t) => false,
        (Kind::Starting, GEvent::Tick) => at >= due(from, t),
        (Kind::Starting | Kind::Ready, GEvent::Crashed) => true,
        _ => false,
    }
}

// Ticks land at the first instant anything is due (the state's timer, or the bound); ready, crash
// and beat 1 ms after the setup's last event, inside every window. Each setup holds its own failure
// history and its own frame on air, so a failure lands both below the count and at it, a crash on each
// kind of air, and each Ready setup beats its own way, so the tick is a recycle, a hang, or both due.
// After each pair a probe checks what the machine now holds on air against the harness's own model.
#[test]
fn every_graphics_pair_follows_the_brief() {
    let t = Thresholds::default();
    let (mut checked, mut moved) = (0usize, 0usize);
    let mut hides = [0usize; REASONS.len()];
    let mut probes = Probes::default();
    for (k, prior, beat, shown) in setups(&t) {
        for e in G_EVENTS {
            let (mut g, mut stamps, last, air) = graphics_in(k, prior, beat, shown, &t);
            let from = g.state();
            let gen = g.generation();
            let at = if e == GEvent::Tick {
                due_with(from, air, &t)
            } else {
                last + 1
            };
            if is_failure(from, air, e, at, &t) {
                stamps.push(at);
            }
            let n = within(&stamps, at, t.cef_fail_window_ms);
            let a = apply(&mut g, e, at, gen, &t);
            let want = brief(from, air, e, at, n, gen, &t);
            let ctx = format!("({k:?} prior {prior} {beat:?} {shown:?}, {e:?}) at {at}");
            assert_eq!((g.state(), a), want, "{ctx}");
            let restarted = want.1 == Some(Restart);
            assert_eq!(g.generation(), gen + u64::from(restarted), "{ctx}");
            let air = air_after(air, from, e, gen, &want);
            probe_air(&g, air, at, &t, &mut probes, &ctx);
            checked += 1;
            moved += usize::from(kind(want.0) != k || want.0 != from);
            if let Some(Hide { reason, .. }) = want.1 {
                hides[reason_index(reason)] += 1;
            }
        }
    }
    eprintln!(
        "graphics pairs checked: {checked} ({moved} move the state; hides by reason {:?}: {hides:?}); \
         air probes: {probes:?}",
        REASONS
    );
    assert_eq!(checked, setups(&t).len() * G_EVENTS.len(), "every pair, once");
    assert!(moved > 0, "a matrix where nothing moves is vacuous");
    for r in REASONS {
        assert!(
            hides[reason_index(r)] > 0,
            "no {r:?} hide: those rows are vacuous"
        );
    }
    let Probes {
        live,
        held_crash,
        held_recycle,
        held_reload,
        early,
        off,
    } = probes;
    assert!(
        live > 0 && held_crash > 0 && held_recycle > 0 && held_reload > 0 && early > 0 && off > 0,
        "an air probe never ran: {probes:?}"
    );
}

// A tick 1 ms before anything is due (a timer or the bound), and every ready, crash or beat from
// the previous generation and from one never built, change nothing in any state: not the state, the
// generation, the window or what is on air (the whole machine is compared).
#[test]
fn graphics_early_ticks_and_other_generations_change_nothing() {
    let t = Thresholds::default();
    let other = [GEvent::Ready, GEvent::Crashed, GEvent::Alive];
    let mut checked = 0usize;
    for (k, prior, beat, shown) in setups(&t) {
        let (mut g, _, last, air) = graphics_in(k, prior, beat, shown, &t);
        let before = g.clone();
        let ctx = format!("{k:?} {beat:?} {shown:?}");
        assert_eq!(
            g.on_tick(due_with(g.state(), air, &t) - 1, &t),
            None,
            "{ctx}: acted 1 ms early"
        );
        assert_eq!(g, before, "{ctx}: early tick");
        checked += 1;
        for e in other {
            for gen in [before.generation() - 1, before.generation() + 1] {
                let (mut g, _, _, _) = graphics_in(k, prior, beat, shown, &t);
                assert_eq!(apply(&mut g, e, last + 1, gen, &t), None);
                assert_eq!(g, before, "({ctx}, {e:?}) from generation {gen}");
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
    crashes_leaving_a_frame: usize,
    reload_crashes_beside_a_held_frame: usize,
    crashes_off_air: usize,
    timeouts_beside_a_held_frame: usize,
    held_crash_frames_lifted_by_ready: usize,
    held_recycle_frames_lifted_by_ready: usize,
    crash_hides: usize,
    recycle_hides: usize,
    bound_hides_beside_a_timer: usize,
    crash_hides_lifted_by_ready: usize,
    recycle_hides_lifted_by_ready: usize,
    bound_hides_then_failed: usize,
    reloads_ready: usize,
    reloads_loading: usize,
    gpu_deaths_ignored: usize,
    gpu_deaths_at_a_hang: usize,
    reload_hides: usize,
    held_reload_frames_lifted_by_ready: usize,
    reload_hides_lifted_by_ready: usize,
}

/// What no tick may leave on air: a frame `cef_hang_after_ms` or more after its page's last proof of
/// life. Kept from the machine's own outputs alone (a page's frames reach air at its Ready, with its
/// `last_alive` as the proof, and leave at a `Hide` or at the next page's Ready), so it witnesses the
/// machine, not the harness's matrix.
#[derive(Debug, Default)]
struct Bound {
    /// Ticks after which a frame was on air, each checked against the bound.
    ticks: usize,
    /// Of those, the ones whose frame's page was no longer Ready: held through a reload.
    held: usize,
    /// Of those, the ones 1 ms inside the bound: the check ran at its edge.
    at_the_edge: usize,
    /// The oldest frame left on air after a tick.
    oldest: Ms,
}

/// The sweep's thresholds: the defaults, and a load timeout under `cef_hang_after_ms`, so a held
/// frame's reloads time out inside its bound (under the defaults both are 10 s, and a held frame's
/// bound always comes before its reload's load timeout).
fn sweep_thresholds() -> [(&'static str, Thresholds); 2] {
    let d = Thresholds::default();
    let short = Thresholds {
        cef_load_timeout_ms: 3_000,
        ..d
    };
    assert!(short.cef_load_timeout_ms < short.cef_hang_after_ms);
    [("default", d), ("short load", short)]
}

#[test]
fn graphics_keeps_its_rules_over_generated_schedules() {
    let mut c = GraphicsCounts::default();
    let mut probes = Probes::default();
    for (set, t) in sweep_thresholds() {
        let span = t.cef_fail_window_ms;
        let mut b = Bound::default();
        for case in 0..CASES {
            let mut r = Lcg(case.wrapping_mul(7_919).wrapping_add(29));
            let mut g = Graphics::new();
            let mut now: Ms = 0;
            // Facts kept by the harness: its own failure stamps, the restarts it saw, the last
            // recycle, the Ready page it is driving, what is on air (its own `Air`, from the rules
            // alone, and `shown`, from the machine's outputs alone), the reason of the last hide
            // (cleared by the next Ready) and of a hide with neither a restart nor a Ready since, and
            // whether the page now backing off or Failed left Ready by a hang.
            let mut stamps: Vec<Ms> = Vec::new();
            let mut restarts: u64 = 0;
            let mut recycled_at: Option<Ms> = None;
            let mut page: Option<Page> = None;
            let mut air = Air::Nothing;
            let mut shown: Option<(u64, Ms)> = None;
            let mut hidden_by: Option<HideReason> = None;
            let mut hid_since: Option<HideReason> = None;
            let mut left_by_hang = false;
            for _ in 0..STEPS {
                let aim = match &page {
                    Some(p) => p.deadline(&t),
                    None => due_with(g.state(), air, &t),
                };
                now = advance(&mut r, now, aim);
                let gen = g.generation();
                let ctx = format!("{set} case {case}");
                assert_eq!(gen, restarts, "{ctx}: one generation per restart");
                // The Ready page's beats up to `now`, in order, before this step's event.
                if let Some(p) = page.as_mut() {
                    while let Some(at) = p.beat_by(&mut r, now, &t) {
                        g.on_alive(at, gen);
                        let want = GraphicsState::Ready {
                            since: p.ready_at,
                            last_alive: at,
                        };
                        assert_eq!(g.state(), want, "{ctx}: the beat at {at}");
                        c.beats += 1;
                    }
                }
                let before = g.state();
                let k = kind(before);
                match (&page, before) {
                    (Some(p), GraphicsState::Ready { since, last_alive }) => assert_eq!(
                        (since, last_alive),
                        (p.ready_at, p.last_beat),
                        "{ctx}: the machine's Ready is the harness's own"
                    ),
                    (None, GraphicsState::Ready { .. }) => panic!("{ctx}: Ready with no page"),
                    (Some(_), other) => panic!("{ctx}: a page, but {other:?}"),
                    (None, _) => {}
                }
                if let GraphicsState::Ready { last_alive, .. } = before {
                    shown = Some((gen, last_alive));
                }
                let (e, ev_gen) = match r.next(13) {
                    0..=4 => (GEvent::Tick, gen),
                    5..=6 => (GEvent::Ready, gen),
                    7..=8 => (GEvent::Crashed, gen),
                    9 => (GEvent::Alive, gen),
                    // A process fact, with no generation: it always reaches the current page.
                    10 => (GEvent::GpuDeath, gen),
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
                        "{ctx}: generation {ev_gen} while at {gen}"
                    );
                    c.stale += 1;
                    continue;
                }
                let due_now = now >= due_with(before, air, &t);
                let bound_now = e == GEvent::Tick && bound_due(before, air, now, &t);
                let hang = bound_now && k == Kind::Ready;
                let failure = is_failure(before, air, e, now, &t);
                if failure {
                    stamps.push(now);
                }
                let n = within(&stamps, now, span);
                let want = match (k, e) {
                    (_, GEvent::Tick) if !due_now => (before, None),
                    _ => brief(before, air, e, now, n, gen, &t),
                };
                assert_eq!(
                    (g.state(), a),
                    want,
                    "{ctx}: ({k:?}, {e:?}) at {now}, {n} in the window, {air:?}"
                );
                let held = matches!(air, Air::Held { .. });
                match (k, e, a) {
                    (_, GEvent::Tick, None) if !due_now => {
                        c.quiet_ticks += 1;
                        // A quiet Ready tick the ready alone would have hung: a beat kept the page.
                        if let (Kind::Ready, Some(p)) = (k, &page) {
                            c.beaten_ready_ticks += usize::from(now >= p.ready_at + t.cef_hang_after_ms);
                        }
                    }
                    (Kind::Ready, GEvent::Tick, _) if hang => {
                        c.hangs += 1;
                        c.hangs_to_failed += usize::from(kind(g.state()) == Kind::Failed);
                        c.hang_at_recycle += usize::from(now >= due(before, &t));
                    }
                    (_, GEvent::Tick, Some(Hide { reason, .. })) if bound_now => {
                        match reason {
                            HideReason::Crash => c.crash_hides += 1,
                            HideReason::Recycle => c.recycle_hides += 1,
                            HideReason::Reload => c.reload_hides += 1,
                            other => panic!("{ctx}: a {other:?} hide at a held frame's bound"),
                        }
                        c.bound_hides_beside_a_timer += usize::from(now >= due(before, &t));
                    }
                    (Kind::Backoff, GEvent::Tick, _) => c.restarts_after_backoff += 1,
                    (Kind::Ready, GEvent::Tick, _) => {
                        c.recycles += 1;
                        recycled_at = Some(now);
                    }
                    (Kind::Failed, GEvent::Tick, _) => c.retries_after_failed += 1,
                    (Kind::Starting, GEvent::Tick, a) => {
                        match a {
                            None => c.timeouts_to_backoff += 1,
                            Some(_) => c.timeouts_to_failed += 1,
                        }
                        c.timeouts_beside_a_held_frame += usize::from(held);
                    }
                    (Kind::Starting, GEvent::Crashed, _) => c.crashes_loading += 1,
                    (Kind::Ready, GEvent::Crashed, _) => c.crashes_ready += 1,
                    (Kind::Ready, GEvent::GpuDeath, None) => c.gpu_deaths_at_a_hang += 1,
                    (Kind::Ready, GEvent::GpuDeath, _) => c.reloads_ready += 1,
                    (Kind::Starting, GEvent::GpuDeath, _) => c.reloads_loading += 1,
                    (_, GEvent::GpuDeath, _) => c.gpu_deaths_ignored += 1,
                    (Kind::Starting, GEvent::Ready, _) => c.readies += 1,
                    (_, GEvent::Ready, _) => c.readies_ignored += 1,
                    (Kind::Backoff, GEvent::Crashed, _) => c.crashes_ignored_backing_off += 1,
                    (Kind::Failed, GEvent::Crashed, _) => c.crashes_ignored_failed += 1,
                    (Kind::Ready, GEvent::Alive, _) => {
                        // A beat off the page's schedule (a late one): the page is silent again after
                        // it if it had stopped. The machine keeps the later of it and the last beat.
                        let p = page.as_mut().expect("Ready has a page");
                        p.last_beat = p.last_beat.max(now);
                        c.beats += 1;
                    }
                    (_, GEvent::Alive, _) => {
                        c.beats_ignored_not_ready += 1;
                        c.beats_after_a_hang += usize::from(left_by_hang);
                    }
                }
                if failure {
                    c.crashes_to_failed +=
                        usize::from(e == GEvent::Crashed && a == Some(hide(HideReason::Failed, gen)));
                    c.waits_doubled += usize::from(a.is_none() && n >= 2);
                    c.failures_within_a_window_of_a_recycle +=
                        usize::from(recycled_at.is_some_and(|at| now - at <= span));
                    if e == GEvent::Crashed && a.is_none() {
                        match (k, air) {
                            (Kind::Ready, _) => c.crashes_leaving_a_frame += 1,
                            (_, Air::Held { .. }) => c.reload_crashes_beside_a_held_frame += 1,
                            _ => c.crashes_off_air += 1,
                        }
                    }
                }
                // The page the harness drives: made at the machine's Ready, gone when it leaves Ready.
                let after = kind(g.state());
                if k == Kind::Starting && after == Kind::Ready {
                    page = Some(Page::new(&mut r, now, &t));
                    hid_since = None;
                    if let Air::Held { ended, .. } = air {
                        match ended {
                            HideReason::Crash => c.held_crash_frames_lifted_by_ready += 1,
                            HideReason::Recycle => c.held_recycle_frames_lifted_by_ready += 1,
                            HideReason::Reload => c.held_reload_frames_lifted_by_ready += 1,
                            other => panic!("{ctx}: a frame held as {other:?}"),
                        }
                    }
                    match hidden_by.take() {
                        Some(HideReason::Hang) => c.hang_hides_lifted_by_ready += 1,
                        Some(HideReason::Crash) => c.crash_hides_lifted_by_ready += 1,
                        Some(HideReason::Recycle) => c.recycle_hides_lifted_by_ready += 1,
                        Some(HideReason::Reload) => c.reload_hides_lifted_by_ready += 1,
                        Some(HideReason::Failed) | None => {}
                    }
                } else if after != Kind::Ready {
                    page = None;
                }
                match a {
                    Some(Hide { reason, .. }) => {
                        // After a `Hang` or `Failed` hide the next action is a restart. Only a `Crash`,
                        // `Recycle` or `Reload` hide leaves the reload loading, so only it can be
                        // followed by another hide with neither a restart nor a Ready (which lifts it)
                        // between: that reload's `Failed`.
                        if let Some(prev) = hid_since {
                            assert!(
                                matches!(prev, HideReason::Crash | HideReason::Recycle | HideReason::Reload)
                                    && reason == HideReason::Failed,
                                "{ctx}: {prev:?} then {reason:?} at {now} with no restart or Ready between"
                            );
                            c.bound_hides_then_failed += 1;
                        }
                        hid_since = Some(reason);
                        hidden_by = Some(reason);
                        shown = None;
                    }
                    Some(Restart) => {
                        restarts += 1;
                        hid_since = None;
                        left_by_hang = false;
                    }
                    None => {}
                }
                left_by_hang |= hang;
                // The bound, from the machine's outputs alone: a Ready page's frames are on air from
                // its last beat; no tick leaves any frame on air `cef_hang_after_ms` after its proof.
                if let GraphicsState::Ready { last_alive, .. } = g.state() {
                    shown = Some((g.generation(), last_alive));
                }
                if let (GEvent::Tick, Some((from, proof))) = (e, shown) {
                    let age = now - proof;
                    assert!(
                        age < t.cef_hang_after_ms,
                        "{ctx}: the tick at {now} left generation {from}'s frame on air {age} ms after \
                         its last proof of life"
                    );
                    b.ticks += 1;
                    b.held += usize::from(kind(g.state()) != Kind::Ready);
                    b.at_the_edge += usize::from(age + 1 == t.cef_hang_after_ms);
                    b.oldest = b.oldest.max(age);
                }
                // What the machine holds on air, against the harness's own model, after every step:
                // this replaces the hidden-flag checks the harness itself made true (PR #22 review M1).
                air = air_after(air, before, e, gen, &want);
                probe_air(&g, air, now, &t, &mut probes, &format!("{ctx} at {now}"));
            }
        }
        eprintln!(
            "graphics schedules ({set}): {CASES} cases × {STEPS} steps; the bound checked after {} ticks with \
             a frame on air ({} of a page no longer Ready, {} 1 ms inside the bound; oldest {} ms of {})",
            b.ticks, b.held, b.at_the_edge, b.oldest, t.cef_hang_after_ms
        );
        assert!(
            b.ticks > 0 && b.held > 0 && b.at_the_edge > 0,
            "{set}: the bound was never checked, or never on a held frame, or never at its edge: {b:?}"
        );
    }
    eprintln!("graphics schedules, both sets: {c:?}; air probes: {probes:?}");
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
        ("crashes that left a frame on air", c.crashes_leaving_a_frame),
        (
            "reload crashes beside a held frame",
            c.reload_crashes_beside_a_held_frame,
        ),
        ("crashes with nothing on air", c.crashes_off_air),
        (
            "load timeouts beside a held frame",
            c.timeouts_beside_a_held_frame,
        ),
        (
            "crashed frames a Ready replaced inside the bound",
            c.held_crash_frames_lifted_by_ready,
        ),
        (
            "recycled frames a Ready replaced inside the bound",
            c.held_recycle_frames_lifted_by_ready,
        ),
        ("crash hides", c.crash_hides),
        ("recycle hides", c.recycle_hides),
        ("bound hides beside a due timer", c.bound_hides_beside_a_timer),
        (
            "crash hides lifted by the next Ready",
            c.crash_hides_lifted_by_ready,
        ),
        (
            "recycle hides lifted by the next Ready",
            c.recycle_hides_lifted_by_ready,
        ),
        ("bound hides then the reload's Failed", c.bound_hides_then_failed),
        ("GPU-death reloads of a Ready page", c.reloads_ready),
        ("GPU-death reloads of a loading page", c.reloads_loading),
        (
            "GPU deaths ignored while backing off or Failed",
            c.gpu_deaths_ignored,
        ),
        ("GPU deaths at a Ready page's hang", c.gpu_deaths_at_a_hang),
        ("reload hides", c.reload_hides),
        (
            "reloaded frames a Ready replaced inside the bound",
            c.held_reload_frames_lifted_by_ready,
        ),
        (
            "reload hides lifted by the next Ready",
            c.reload_hides_lifted_by_ready,
        ),
        ("air probes: the Ready page's frame", probes.live),
        ("air probes: a crashed page's frame", probes.held_crash),
        ("air probes: a recycled page's frame", probes.held_recycle),
        ("air probes: a reloaded page's frame", probes.held_reload),
        ("air probes: 1 ms before a bound", probes.early),
        ("air probes: nothing on air", probes.off),
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
                    Some(h) => {
                        // The count-th load timeout of each set of attempts: Failed, naming it.
                        let gen = (hides.len() as u64 + 1) * count as u64;
                        assert_eq!(h, hide(HideReason::Failed, gen), "{mode:?} φ{phase} at {now}");
                        hides.push(now);
                    }
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
            assert_eq!(kinds, vec![Restart, hide(HideReason::Hang, 1), Restart], "{ctx}");
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

// The bound on the frame tick grid of both modes, at every phase, for every way a Ready page's life
// ends with its frame on air (PR #22 review G1: its probe held a crashed page's frame ~23 s, and a
// recycle's ~33 s; Owed 96 added the reload after a GPU-process death). A page that is ready and
// beating off the grid crashes off the grid, is recycled on the grid, or is reloaded after a
// GPU-process death off the grid, and none of its reloads signals ready. Its frame comes off air on the first
// tick at or after its last beat plus `cef_hang_after_ms` (late by under one tick spacing), naming
// the path and generation 1, while the reload loads on; the reloads then run out their attempts, and
// the `cef_fail_count`-th enters Failed and hides again, as `Failed`. The last beat comes well
// before the crash or the recycle, so a bound timed from either would land a beat or more later
// than the window allowed. Each run counts the beats it delivered, so the frame was on air to hold.
// The recycle runs at 60 s here: its length is not under test, and the default 2 h would take
// 218 000 ticks a phase.
#[test]
fn a_crashed_or_recycled_page_whose_reloads_never_ready_hides_on_the_frame_tick_grid() {
    let t = Thresholds {
        cef_recycle_every_ms: 60_000,
        ..Thresholds::default()
    };
    let count = t.cef_fail_count as u64;
    let (hang, base, every) = (t.cef_hang_after_ms, t.cef_backoff_base_ms, t.cef_alive_every_ms);
    let (mut cases, mut beats_delivered) = (0usize, 0usize);
    for ending in [HideReason::Crash, HideReason::Recycle, HideReason::Reload] {
        // A crash is the first failure; a recycle and a reload record none.
        let before = u64::from(ending == HideReason::Crash);
        let failed_gen = count + 1 - before;
        for mode in BOTH_MODES {
            let spacing = widest(mode);
            for phase in 0..spacing {
                let mut g = Graphics::new();
                let ready = phase + t.cef_load_timeout_ms / 2 + 7; // off the grid
                                                                   // `crash` is the off-grid end: the crash, or the GPU-process death.
                let (last_beat, crash) = match ending {
                    HideReason::Crash | HideReason::Reload => {
                        (ready + 3 * every, Some(ready + 3 * every + 1_511))
                    }
                    _ => (ready + t.cef_recycle_every_ms - 1_500, None),
                };
                let beats: Vec<Ms> = (1..)
                    .map(|k| ready + k * every)
                    .take_while(|&b| b <= last_beat)
                    .collect();
                let last = *beats.last().unwrap();
                let ends = crash.unwrap_or(ready + t.cef_recycle_every_ms);
                assert!(
                    last + every >= ends && ends - last < hang,
                    "the page beats until shortly before its end"
                );
                let (mut next, mut is_ready, mut crashed) = (0usize, false, false);
                let mut acts = Vec::new();
                let horizon = ends + count * (t.cef_load_timeout_ms + t.cef_backoff_cap_ms + 2 * spacing);
                for now in ticks(mode, phase) {
                    assert!(
                        now <= horizon,
                        "{ending:?} {mode:?} φ{phase}: no Failed by {horizon} ms"
                    );
                    if !is_ready && now >= ready {
                        g.on_ready(ready, 1);
                        is_ready = true;
                    }
                    while next < beats.len() && beats[next] <= now {
                        g.on_alive(beats[next], 1);
                        next += 1;
                        beats_delivered += 1;
                    }
                    if let Some(c) = crash.filter(|&c| !crashed && now >= c) {
                        if ending == HideReason::Reload {
                            // The reload restarts at the death itself, off the grid.
                            assert_eq!(g.on_gpu_death(c, &t), Some(Restart), "{mode:?} φ{phase}");
                            acts.push((c, Restart));
                        } else {
                            assert_eq!(
                                g.on_crashed(c, 1, &t),
                                None,
                                "{mode:?} φ{phase}: the first failure"
                            );
                        }
                        crashed = true;
                    }
                    if let Some(a) = g.on_tick(now, &t) {
                        acts.push((now, a));
                    }
                    if acts
                        .last()
                        .is_some_and(|&(_, a)| a == hide(HideReason::Failed, failed_gen))
                    {
                        break;
                    }
                }
                let ctx = format!("{ending:?} {mode:?} φ{phase}: {acts:?}");
                assert_eq!(next, beats.len(), "{ctx}: every beat delivered");
                let kinds: Vec<GraphicsAction> = acts.iter().map(|&(_, a)| a).collect();
                let mut want = vec![Restart, Restart, hide(ending, 1)];
                want.extend((before + 2..=count).map(|_| Restart));
                want.push(hide(HideReason::Failed, failed_gen));
                assert_eq!(kinds, want, "{ctx}");
                assert_eq!(acts[0].0, phase, "{ctx}: the first tick restarts");
                let reload = acts[1].0;
                let reload_due = match (ending, crash) {
                    (HideReason::Crash, Some(c)) => c + base,
                    _ => ends,
                };
                assert!(
                    (reload_due..reload_due + spacing).contains(&reload),
                    "{ctx}: reloaded {} ms after the page's end",
                    reload - ends
                );
                let off = acts[2].0;
                assert!(
                    (last + hang..last + hang + spacing).contains(&off),
                    "{ctx}: the frame came off air {} ms after its last beat",
                    off - last
                );
                cases += 1;
            }
        }
    }
    eprintln!(
        "bound runs checked: {cases} (path × mode × phase), {beats_delivered} beats delivered before the \
         crash, the recycle or the reload; the frame off air within one tick of its last beat + {hang} ms"
    );
    let phases: usize = BOTH_MODES.iter().map(|&m| widest(m) as usize).sum();
    assert_eq!(cases, 3 * phases, "every path, mode and phase, once");
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
