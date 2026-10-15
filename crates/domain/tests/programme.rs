//! Sweeps over C03's programme sub-machine: the transition table against the PDF's state machine, a
//! generated-schedule sweep against an independent reference model, and the PDF timing budgets in
//! both output modes at every grid phase. They live here, not inline, because the domain's crate root
//! forbids `clippy::print_stderr`, and a sweep must print the count it checked (AGENTS.md §5).
use domain::config::{OutputProfile, Thresholds};
use domain::programme::{
    choice_for, next_phase, Feed, FrameChoice, Programme, ProgrammePhase, SlateKind, PROGRAMME_TABLE,
};
use domain::time::Ms;
use ProgrammePhase::{Frozen, Live, Reconnecting, Waiting};

const PHASES: [ProgrammePhase; 4] = [Waiting, Live, Frozen, Reconnecting];
const FEEDS: [Feed; 4] = [Feed::Continuous, Feed::Fresh, Feed::Gap, Feed::LongGap];

/// Exhaustive `match`es, so a new variant is a compile error here and `PHASES` / `FEEDS` cannot go
/// stale behind a sweep that still reports 16 pairs.
fn phase_index(p: ProgrammePhase) -> usize {
    match p {
        Waiting => 0,
        Live => 1,
        Frozen => 2,
        Reconnecting => 3,
    }
}

fn feed_index(f: Feed) -> usize {
    match f {
        Feed::Continuous => 0,
        Feed::Fresh => 1,
        Feed::Gap => 2,
        Feed::LongGap => 3,
    }
}

/// The PDF robustness state machine as a full matrix, written from the diagram (C03 brief; spec
/// §4.2, §6), not from `PROGRAMME_TABLE`. Rows follow `PHASES`, columns follow `FEEDS`.
/// - Waiting: only 1 s of continuous frames takes it live; a never-live input never shows the
///   reconnecting slate.
/// - Live: 0.5 s with no frame freezes it; 3 s with no frame slates it (a stalled tick skips Frozen).
/// - Frozen: frames resuming take it live at once, however short the run; 3 s with no frame slates.
/// - Reconnecting: only 1 s of continuous frames takes it live.
const PDF: [[ProgrammePhase; 4]; 4] = [
    // Continuous, Fresh, Gap, LongGap
    [Live, Waiting, Waiting, Waiting],                // Waiting
    [Live, Live, Frozen, Reconnecting],               // Live
    [Live, Live, Frozen, Reconnecting],               // Frozen
    [Live, Reconnecting, Reconnecting, Reconnecting], // Reconnecting
];

fn pdf_next(p: ProgrammePhase, f: Feed) -> ProgrammePhase {
    PDF[phase_index(p)][feed_index(f)]
}

/// Spec §3.3: what the encoder gets in each phase. Written from the spec, not from `choice_for`.
fn spec_choice(p: ProgrammePhase) -> FrameChoice {
    match p {
        Waiting => FrameChoice::Slate(SlateKind::Waiting),
        Live => FrameChoice::Emit,
        Frozen => FrameChoice::Repeat,
        Reconnecting => FrameChoice::Slate(SlateKind::Reconnecting),
    }
}

fn pairs() -> impl Iterator<Item = (ProgrammePhase, Feed)> {
    PHASES
        .into_iter()
        .flat_map(|p| FEEDS.into_iter().map(move |f| (p, f)))
}

#[test]
fn the_variant_lists_name_every_variant_once() {
    for (i, &p) in PHASES.iter().enumerate() {
        assert_eq!(phase_index(p), i, "{p:?}");
    }
    for (i, &f) in FEEDS.iter().enumerate() {
        assert_eq!(feed_index(f), i, "{f:?}");
    }
}

// ---- the table itself (source: the PDF robustness diagram) ----

#[test]
fn every_pair_follows_the_pdf_state_machine() {
    let mut checked = 0usize;
    for (p, f) in pairs() {
        assert_eq!(next_phase(p, f), pdf_next(p, f), "({p:?}, {f:?})");
        checked += 1;
    }
    eprintln!("pdf state machine: checked {checked} (phase, feed) pairs");
    assert_eq!(checked, PHASES.len() * FEEDS.len(), "every pair, once");
    assert!(checked > 0, "checked 0 pairs");
}

#[test]
fn every_table_row_is_a_pdf_transition_that_moves() {
    let mut checked = 0usize;
    for &(from, feed, to) in PROGRAMME_TABLE {
        assert_eq!(to, pdf_next(from, feed), "row ({from:?}, {feed:?}) → {to:?}");
        assert_ne!(
            to, from,
            "row ({from:?}, {feed:?}) keeps its phase: unlisted pairs already do"
        );
        checked += 1;
    }
    eprintln!("table rows checked: {checked}");
    assert!(checked > 0, "PROGRAMME_TABLE is empty");
}

#[test]
fn table_has_no_duplicate_keys() {
    let mut compared = 0usize;
    for (i, a) in PROGRAMME_TABLE.iter().enumerate() {
        for b in &PROGRAMME_TABLE[i + 1..] {
            assert!((a.0, a.1) != (b.0, b.1), "duplicate key ({:?}, {:?})", a.0, a.1);
            compared += 1;
        }
    }
    eprintln!(
        "table keys: compared {compared} row pairs over {} rows",
        PROGRAMME_TABLE.len()
    );
    assert!(compared > 0, "compared 0 row pairs");
}

#[test]
fn unlisted_pairs_hold_their_phase() {
    let mut checked = 0;
    for (ph, feed) in pairs() {
        if !PROGRAMME_TABLE.iter().any(|&(f, s, _)| f == ph && s == feed) {
            assert_eq!(next_phase(ph, feed), ph, "({ph:?}, {feed:?})");
            checked += 1;
        }
    }
    eprintln!("unlisted pairs checked: {checked}");
    assert!(checked > 0, "checked 0 unlisted pairs");
    assert_eq!(checked, 16 - PROGRAMME_TABLE.len());
}

#[test]
fn every_phase_is_reachable_from_waiting() {
    let mut seen = vec![Waiting];
    loop {
        let before = seen.len();
        for &(f, _, to) in PROGRAMME_TABLE {
            if seen.contains(&f) && !seen.contains(&to) {
                seen.push(to);
            }
        }
        if seen.len() == before {
            break;
        }
    }
    eprintln!("reachable from Waiting: {seen:?}");
    assert_eq!(seen.len(), PHASES.len(), "reachable: {seen:?}");
}

#[test]
fn waiting_never_jumps_to_reconnecting() {
    // never-live input stays on the WAITING slate, not "reconnecting"
    let mut checked = 0;
    for feed in FEEDS {
        assert_ne!(next_phase(Waiting, feed), Reconnecting, "{feed:?}");
        checked += 1;
    }
    eprintln!("waiting feeds checked: {checked}");
    assert_eq!(checked, FEEDS.len());
}

// A second call: C08's `tick_due` can run several ticks at one `now`, and they all see the same feed.
// A second tick with the same feed must land where the first did.
#[test]
fn a_second_tick_with_the_same_feed_changes_nothing() {
    let mut checked = 0;
    for (p, f) in pairs() {
        let once = next_phase(p, f);
        assert_eq!(next_phase(once, f), once, "({p:?}, {f:?}) moved twice");
        assert_eq!(choice_for(once), spec_choice(once));
        checked += 1;
    }
    eprintln!("repeated-feed pairs checked: {checked}");
    assert_eq!(checked, PHASES.len() * FEEDS.len());
}

// ---- the machine against an independent reference model, over generated schedules ----

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

    fn pick(&mut self, xs: &[Ms]) -> Ms {
        xs[self.next(xs.len() as u64) as usize]
    }
}

/// The contract as an executable model, written from the PDF and `Feed`'s documented terms rather
/// than from the code: it keeps every accepted frame and derives each tick's feed from that whole
/// history. A frame stamped at or before the latest one changes nothing.
struct Model {
    frames: Vec<Ms>,
    phase: ProgrammePhase,
    ever_live: bool,
}

impl Model {
    fn on_frame(&mut self, at: Ms) -> bool {
        let accepted = self.frames.last().is_none_or(|&l| at > l);
        if accepted {
            self.frames.push(at);
        }
        accepted
    }

    fn age(&self, now: Ms) -> Option<Ms> {
        self.frames.last().map(|&l| now.saturating_sub(l))
    }

    /// Walk back from the latest frame while consecutive frames are at most `frozen_after_ms` apart;
    /// stop as soon as the run already spans `resume_after_ms`.
    fn span(&self, t: &Thresholds) -> Ms {
        let last = *self.frames.last().expect("span of no frames");
        let mut start = last;
        for w in self.frames.windows(2).rev() {
            if w[1] - w[0] > t.frozen_after_ms || last - start >= t.resume_after_ms {
                break;
            }
            start = w[0];
        }
        last - start
    }

    fn feed(&self, now: Ms, t: &Thresholds) -> Feed {
        match self.age(now) {
            None => Feed::LongGap,
            Some(age) if age >= t.slate_after_ms => Feed::LongGap,
            Some(age) if age > t.frozen_after_ms => Feed::Gap,
            Some(_) if self.span(t) >= t.resume_after_ms => Feed::Continuous,
            Some(_) => Feed::Fresh,
        }
    }
}

/// Counters the sweep asserts on, so a generator that drifts away from a case reds instead of
/// silently shrinking the sweep.
#[derive(Default)]
struct Seen {
    ticks: usize,
    frames: usize,
    row_hits: Vec<usize>,
    phase_ticks: [usize; 4],
    late_accepted: usize,
    late_ignored: usize,
    age_at_frozen_after: usize,
    age_one_past_frozen_after: usize,
    age_one_before_slate_after: usize,
    age_at_slate_after: usize,
    span_one_short: usize,
    span_exact: usize,
}

struct Sim<'a> {
    p: Programme,
    m: Model,
    t: &'a Thresholds,
    seen: Seen,
}

impl Sim<'_> {
    fn frame(&mut self, at: Ms, late: bool) {
        let accepted = self.m.on_frame(at);
        self.p.on_frame(at, self.t);
        assert_eq!(
            self.p.last_frame_at(),
            self.m.frames.last().copied(),
            "latest frame after {at}"
        );
        self.seen.frames += 1;
        if late {
            if accepted {
                self.seen.late_accepted += 1;
            } else {
                self.seen.late_ignored += 1;
            }
        }
    }

    fn tick(&mut self, now: Ms) {
        let t = self.t;
        let feed = self.m.feed(now, t);
        assert_eq!(
            self.p.feed(now, t),
            feed,
            "feed at {now} after frames {:?}",
            self.tail()
        );
        let age = self.m.age(now);
        if feed == Feed::Continuous || feed == Feed::Fresh {
            let span = self.m.span(t);
            self.seen.span_one_short += usize::from(span == t.resume_after_ms - 1);
            self.seen.span_exact += usize::from(span == t.resume_after_ms);
        }
        if let Some(a) = age {
            self.seen.age_at_frozen_after += usize::from(a == t.frozen_after_ms);
            self.seen.age_one_past_frozen_after += usize::from(a == t.frozen_after_ms + 1);
            self.seen.age_one_before_slate_after += usize::from(a == t.slate_after_ms - 1);
            self.seen.age_at_slate_after += usize::from(a == t.slate_after_ms);
        }
        let before = self.p.phase();
        assert_eq!(before, self.m.phase, "phase before the tick at {now}");
        let want = pdf_next(before, feed);
        let choice = self.p.on_tick(now, t);
        self.m.phase = want;
        self.m.ever_live |= want == Live;
        assert_eq!(self.p.phase(), want, "({before:?}, {feed:?}) at {now}");
        assert_eq!(choice, spec_choice(want), "choice at {now}");

        // Spec-stated properties, checked on every tick on their own terms.
        let age = age.unwrap_or(Ms::MAX);
        match choice {
            FrameChoice::Emit => assert!(age <= t.frozen_after_ms, "emitted a {age} ms old frame"),
            FrameChoice::Repeat => assert!(
                t.frozen_after_ms < age && age < t.slate_after_ms,
                "repeated with a {age} ms old frame"
            ),
            FrameChoice::Slate(SlateKind::Waiting) => assert!(!self.m.ever_live, "waiting slate after live"),
            FrameChoice::Slate(SlateKind::Reconnecting) => {
                assert!(self.m.ever_live, "reconnecting slate before any live")
            }
        }
        if age >= t.slate_after_ms {
            assert!(
                matches!(choice, FrameChoice::Slate(_)),
                "{age} ms with no frame and no slate"
            );
        }

        if before != want {
            let row = PROGRAMME_TABLE
                .iter()
                .position(|&(f, s, _)| f == before && s == feed)
                .expect("a phase moved on a pair the table does not list");
            self.seen.row_hits[row] += 1;
        }
        self.seen.phase_ticks[phase_index(want)] += 1;
        self.seen.ticks += 1;
    }

    fn tail(&self) -> &[Ms] {
        &self.m.frames[self.m.frames.len().saturating_sub(4)..]
    }
}

#[test]
fn programme_matches_the_reference_model_over_generated_schedules() {
    const CASES: usize = 160;
    const SEGMENTS: usize = 14;
    let t = Thresholds::default();
    let mut rng = Lcg(0x0000_0C03_0001);
    let mut seen = Seen {
        row_hits: vec![0; PROGRAMME_TABLE.len()],
        ..Seen::default()
    };
    // Durations and intervals sit on and beside each threshold (500, 3 000, 1 000), so the ±1 ms
    // boundaries are reached by the schedule rather than set up by hand.
    let on_durations = [100, 499, 500, 501, 600, 999, 1_000, 1_001, 1_500, 3_000];
    let frame_intervals = [1, 10, 33, 34, 100, 250, 499, 500, 501];
    let off_durations = [100, 499, 500, 501, 999, 1_000, 1_001, 2_999, 3_000, 3_001, 4_000];
    let tick_intervals = [1, 10, 33, 34, 100];
    let stalls = [2_999, 3_000, 3_001, 4_000];
    let late_by = [1, 50, 100, 200, 300, 400, 499, 600, 5_000];
    let continue_by = [1, 100, 250, 400, 499, 500];
    for _case in 0..CASES {
        let mut sim = Sim {
            p: Programme::default(),
            m: Model {
                frames: Vec::new(),
                phase: Waiting,
                ever_live: false,
            },
            t: &t,
            seen: std::mem::take(&mut seen),
        };
        let mut now: Ms = rng.next(50);
        for _seg in 0..SEGMENTS {
            let ti = rng.pick(&tick_intervals);
            match rng.next(12) {
                // The camera delivers frames every `fi` while the ticks run every `ti`; at a shared
                // instant either may come first.
                0..=4 => {
                    let (end, fi) = (now + rng.pick(&on_durations), rng.pick(&frame_intervals));
                    let (mut nf, mut nt) = (now, now);
                    while nf.min(nt) < end {
                        if nf < nt || (nf == nt && rng.next(2) == 0) {
                            sim.frame(nf, false);
                            nf += fi;
                        } else {
                            sim.tick(nt);
                            nt += ti;
                        }
                    }
                    now = end;
                }
                // Camera loss: ticks only.
                5..=7 => {
                    let end = now + rng.pick(&off_durations);
                    while now < end {
                        sim.tick(now);
                        now += ti;
                    }
                }
                // A stalled tick thread: nothing at all, then one tick.
                8 => {
                    now += rng.pick(&stalls);
                    sim.tick(now);
                }
                // A frame stamped in the past (before the latest tick, perhaps before the latest
                // frame), then a tick.
                9 => {
                    sim.frame(now.saturating_sub(rng.pick(&late_by)), true);
                    sim.tick(now);
                }
                // A frame delivered late: stamped just after the latest frame but before the latest
                // tick. It is the only way a Frozen programme meets a Continuous feed (the run
                // survives the gap the tick already saw), so row (Frozen, Continuous) gets a witness.
                _ => {
                    let latest = sim.m.frames.last().copied().unwrap_or(0);
                    sim.frame((latest + rng.pick(&continue_by)).min(now), true);
                    sim.tick(now);
                }
            }
        }
        seen = sim.seen;
    }
    eprintln!(
        "reference-model sweep: {CASES} cases, {} ticks, {} frames ({} late accepted, {} late \
         ignored); ticks per phase {:?} (Waiting, Live, Frozen, Reconnecting); hits per table row \
         {:?}; boundary ticks: age 500 = {}, 501 = {}, 2 999 = {}, 3 000 = {}; span 999 = {}, \
         1 000 = {}",
        seen.ticks,
        seen.frames,
        seen.late_accepted,
        seen.late_ignored,
        seen.phase_ticks,
        seen.row_hits,
        seen.age_at_frozen_after,
        seen.age_one_past_frozen_after,
        seen.age_one_before_slate_after,
        seen.age_at_slate_after,
        seen.span_one_short,
        seen.span_exact,
    );
    assert!(seen.ticks > 0 && seen.frames > 0, "the sweep checked nothing");
    for (i, &hits) in seen.row_hits.iter().enumerate() {
        assert!(hits > 0, "table row {:?} never fired", PROGRAMME_TABLE[i]);
    }
    for (i, &n) in seen.phase_ticks.iter().enumerate() {
        assert!(n > 0, "the sweep never ticked in {:?}", PHASES[i]);
    }
    assert!(seen.late_accepted > 0, "no late frame was accepted");
    assert!(
        seen.late_ignored > 0,
        "no frame stamped before the latest was sent"
    );
    let boundaries = [
        seen.age_at_frozen_after,
        seen.age_one_past_frozen_after,
        seen.age_one_before_slate_after,
        seen.age_at_slate_after,
        seen.span_one_short,
        seen.span_exact,
    ];
    assert!(
        boundaries.iter().all(|&n| n > 0),
        "a ±1 ms boundary was never ticked: {boundaries:?}"
    );
}

// ---- the PDF timing budgets, in both output modes, at every grid phase ----

/// Spec §6 budgets: "slate after camera loss ≤ 3.1 s; return to live ≤ 1.2 s".
const SLATE_BUDGET_MS: Ms = 3_100;
const LIVE_BUDGET_MS: Ms = 1_200;

// Both modes are 30 fps (ADR 0005), so they share one tick grid; the sweep runs both so that a profile
// at another rate would be checked too. A tick lands on the first whole millisecond at or after its
// due time, i · period, as C08's `tick_due` reads them; frames use the same rounding on their grid.
// The camera's frames are on their own 30 fps grid, offset from the ticks by `phase`.
#[test]
fn the_pdf_budgets_hold_in_both_modes_at_every_grid_phase() {
    let t = Thresholds::default();
    let (mut checked, mut slated, mut frozen_only) = (0usize, 0usize, 0usize);
    let outages = [
        600, 1_000, 2_000, 2_900, 2_990, 3_000, 3_010, 3_100, 3_500, 5_000, 10_000,
    ];
    for profile in [OutputProfile::P720, OutputProfile::P1080] {
        let period = profile.frame_period_ms();
        let tick_step = period.ceil() as Ms;
        let at = |i: u64, offset: Ms| offset + (i as f64 * period).ceil() as Ms;
        for phase in 0..tick_step {
            for outage in outages {
                let loss = 5_000;
                let back = loss + outage;
                let end = back + 3_000;
                let camera = |at: Ms| at < loss || at >= back;
                let frames: Vec<Ms> = (0..)
                    .map(|k| at(k, phase))
                    .take_while(|&f| f < end)
                    .filter(|&f| camera(f))
                    .collect();
                let ticks: Vec<Ms> = (0..).map(|i| at(i, 0)).take_while(|&n| n < end).collect();
                let last_before = *frames
                    .iter()
                    .rfind(|&&f| f < loss)
                    .expect("frames before the loss");
                let first_after = *frames
                    .iter()
                    .find(|&&f| f >= back)
                    .expect("frames after the return");
                // From the schedule alone: does any tick fall where the latest frame is ≥ 3 s old?
                let expect_slate = ticks
                    .iter()
                    .any(|&n| n >= last_before + t.slate_after_ms && n < first_after);

                let (th, mut m, mut fi) = (&t, Programme::default(), 0);
                let mut out: Vec<(Ms, FrameChoice)> = Vec::with_capacity(ticks.len());
                for &n in &ticks {
                    while fi < frames.len() && frames[fi] <= n {
                        m.on_frame(frames[fi], th);
                        fi += 1;
                    }
                    out.push((n, m.on_tick(n, th)));
                }
                let first_at = |from: Ms, want: FrameChoice| {
                    out.iter()
                        .find(|&&(n, c)| n >= from && c == want)
                        .map(|&(n, _)| n)
                };
                let ctx = format!("{profile:?} phase {phase} outage {outage}");

                let live = first_at(0, FrameChoice::Emit).expect("never live");
                assert!(
                    live - frames[0] >= t.resume_after_ms,
                    "{ctx}: live after {} ms",
                    live - frames[0]
                );
                assert!(
                    live - frames[0] <= LIVE_BUDGET_MS,
                    "{ctx}: first live after {} ms",
                    live - frames[0]
                );

                let frozen = first_at(loss, FrameChoice::Repeat).expect("never froze");
                assert!(
                    frozen - last_before > t.frozen_after_ms,
                    "{ctx}: froze at +{}",
                    frozen - last_before
                );
                assert!(
                    frozen - last_before <= t.frozen_after_ms + tick_step,
                    "{ctx}: froze late, at +{}",
                    frozen - last_before
                );

                let back_live = first_at(first_after, FrameChoice::Emit).expect("never back");
                match first_at(loss, FrameChoice::Slate(SlateKind::Reconnecting)) {
                    Some(slate) => {
                        assert!(
                            expect_slate,
                            "{ctx}: slate at +{} with no 3 s gap",
                            slate - last_before
                        );
                        assert!(
                            slate - last_before >= t.slate_after_ms,
                            "{ctx}: slate at +{}",
                            slate - last_before
                        );
                        assert!(
                            slate - last_before <= SLATE_BUDGET_MS,
                            "{ctx}: PDF budget slate ≤ 3.1 s, got +{}",
                            slate - last_before
                        );
                        assert!(
                            back_live - first_after >= t.resume_after_ms,
                            "{ctx}: live {} ms after the return",
                            back_live - first_after
                        );
                        assert!(
                            back_live - back <= LIVE_BUDGET_MS,
                            "{ctx}: PDF budget live ≤ 1.2 s after the camera returns, got {}",
                            back_live - back
                        );
                        slated += 1;
                    }
                    None => {
                        assert!(!expect_slate, "{ctx}: a 3 s gap and no slate");
                        assert!(
                            back_live - first_after <= tick_step,
                            "{ctx}: resume took {}",
                            back_live - first_after
                        );
                        frozen_only += 1;
                    }
                }
                checked += 1;
            }
        }
    }
    eprintln!(
        "budget sweep: {checked} (mode, grid phase, outage) cases: {slated} slated, {frozen_only} \
         frozen only"
    );
    assert!(checked > 0, "the budget sweep checked 0 cases");
    assert!(
        slated > 0 && frozen_only > 0,
        "both outcomes must be exercised: {slated} / {frozen_only}"
    );
}
