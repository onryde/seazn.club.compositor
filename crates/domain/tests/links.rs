//! Sweeps over C04's links: every (state, event) pair against a matrix written from the brief, both
//! links over generated schedules against facts the harness tracks for itself, and the output's
//! recovery paths against F-004's 15 s broadcast budget. They live here, not inline, because the
//! domain's crate root forbids `clippy::print_stderr`, and a sweep must print the count it checked
//! (AGENTS.md §5).
use domain::config::{
    OutputProfile, Thresholds, OUTPUT_GAP_BUDGET_MS, OUTPUT_PROGRESS_POLL_MS, OUTPUT_SINK_TIMEOUT_MS,
    TICK_WIDEST_MS,
};
use domain::input_link::{InputAction, InputLink, InputState, LossReason, KICK_HISTORY_CAP};
use domain::output_link::{Encoded, OutputLink, OutputState, SwapReason};
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
    /// The output only (Owed 38): bytes flow, but the sink has not yet earned Connected.
    Proving,
    Up,
    Backoff,
}

const INPUT_KINDS: [Kind; 3] = [Kind::Connecting, Kind::Up, Kind::Backoff];
const OUTPUT_KINDS: [Kind; 4] = [Kind::Connecting, Kind::Proving, Kind::Up, Kind::Backoff];

/// Exhaustive `match`es, so a new state variant is a compile error here and the kind lists cannot go
/// stale behind a sweep that still reports its old count.
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
        OutputState::Proving { .. } => Kind::Proving,
        OutputState::Connected => Kind::Up,
        OutputState::Backoff { .. } => Kind::Backoff,
    }
}

/// The consecutive failures a state carries; Connected carries none (it reset them).
fn output_attempt(s: OutputState) -> Option<u32> {
    match s {
        OutputState::Connecting { attempt, .. }
        | OutputState::Proving { attempt, .. }
        | OutputState::Backoff { attempt, .. } => Some(attempt),
        OutputState::Connected => None,
    }
}

/// An input link in `kind` at generation 2, so generation 1 is a real torn-down pipeline and 3 is
/// one never built. Returns the link and the instant its timer is due (connect timeout, the stall
/// rule, the end of the back-off).
fn input_in(kind: Kind, t: &Thresholds) -> (InputLink, Ms) {
    let mut l = InputLink::new();
    assert_eq!(l.on_tick(0, t), Some(InputAction::Rebuild { gen: 1 }));
    l.on_connected(0, 1);
    l.on_lost(100, 1, LossReason::Error, t);
    let built = 100 + t.input_backoff_base_ms;
    assert_eq!(l.on_tick(built, t), Some(InputAction::Rebuild { gen: 2 }));
    let up = built + 50;
    match kind {
        Kind::Connecting => {}
        Kind::Up => l.on_connected(up, 2),
        Kind::Backoff => {
            l.on_connected(up, 2);
            l.on_lost(up + 50, 2, LossReason::Error, t);
        }
        Kind::Proving => panic!("an input link has no Proving state"),
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

/// Applies `e` at `now`, from generation `gen`. Returns what a tick or a stall check asked for.
fn apply_input(l: &mut InputLink, e: InputEvent, now: Ms, gen: u64, t: &Thresholds) -> Option<InputAction> {
    match e {
        InputEvent::Tick => l.on_tick(now, t),
        InputEvent::Connected => {
            l.on_connected(now, gen);
            None
        }
        InputEvent::Lost(r) => {
            l.on_lost(now, gen, r, t);
            None
        }
        InputEvent::Stall => l.check_stall(now, None, t),
    }
}

/// What a pair asks of the adapter: nothing, a build of the next generation, or (C10b) an abandon of
/// the one the link gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asks {
    Nothing,
    Build,
    Abandon,
}

/// The brief's transitions (Step 3), written from its prose: (kind after, what it asks, kicks added).
/// C10b: the link's own two rules (the connect timeout, the stall rule) give the generation up; a
/// loss the adapter reported does not.
fn brief_input(from: Kind, e: InputEvent) -> (Kind, Asks, usize) {
    use Asks as A;
    use InputEvent as E;
    use Kind as K;
    match (from, e) {
        (K::Backoff, E::Tick) => (K::Connecting, A::Build, 0),
        (K::Connecting, E::Tick) => (K::Backoff, A::Abandon, 0), // the connect timed out
        (K::Connecting, E::Connected) => (K::Up, A::Nothing, 0),
        (K::Connecting, E::Lost(_)) => (K::Backoff, A::Nothing, 0), // a failure, never a kick
        (K::Up, E::Lost(r)) => (K::Backoff, A::Nothing, usize::from(r == LossReason::Eos)),
        (K::Up, E::Stall) => (K::Backoff, A::Abandon, 0), // no frame for 5 s
        (k, _) => (k, A::Nothing, 0),                     // everything else changes nothing
    }
}

/// What the link asked for, as the matrix reads it; the generation each action names is checked
/// beside it.
fn asks(a: Option<InputAction>) -> Asks {
    match a {
        None => Asks::Nothing,
        Some(InputAction::Rebuild { .. }) => Asks::Build,
        Some(InputAction::Abandon { .. }) => Asks::Abandon,
    }
}

#[test]
fn every_input_pair_follows_the_brief() {
    let t = Thresholds::default();
    let (mut checked, mut moved, mut abandons) = (0usize, 0usize, 0usize);
    for kind in INPUT_KINDS {
        for e in INPUT_EVENTS {
            let (mut l, due) = input_in(kind, &t);
            let a = apply_input(&mut l, e, due, 2, &t);
            let (want, want_asks, want_kicks) = brief_input(kind, e);
            let got = (input_kind(l.state()), asks(a), l.kicks(due, &t));
            assert_eq!(got, (want, want_asks, want_kicks), "({kind:?}, {e:?})");
            let built = want_asks == Asks::Build;
            assert_eq!(l.generation(), 2 + u64::from(built), "({kind:?}, {e:?})");
            // Each action names its generation: the build the next one, the abandon the one given up.
            match a {
                Some(InputAction::Rebuild { gen }) => assert_eq!(gen, 3, "({kind:?}, {e:?})"),
                Some(InputAction::Abandon { gen }) => {
                    assert_eq!(gen, 2, "({kind:?}, {e:?})");
                    abandons += 1;
                }
                None => {}
            }
            checked += 1;
            moved += usize::from(want != kind);
        }
    }
    eprintln!("input pairs checked: {checked} ({moved} move the state, {abandons} abandon)");
    assert_eq!(
        checked,
        INPUT_KINDS.len() * INPUT_EVENTS.len(),
        "every pair, once"
    );
    assert!(moved > 0, "a matrix where nothing moves is vacuous");
    assert_eq!(abandons, 2, "the connect timeout and the stall rule");
}

// Ticks 1 ms before each timer is due, and every event from generation 1 (torn down) and 3 (never
// built), change nothing in any state: not the state, not the generation, not the kicks.
#[test]
fn input_early_ticks_and_other_generations_change_nothing() {
    let t = Thresholds::default();
    let mut checked = 0usize;
    for kind in INPUT_KINDS {
        let (mut l, due) = input_in(kind, &t);
        let before = l.clone();
        assert_eq!(l.on_tick(due - 1, &t), None, "{kind:?}: acted 1 ms early");
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
    assert_eq!(checked, INPUT_KINDS.len() * (1 + 2 * 4));
}

/// An output link in `kind` at sink generation 2 (1 torn down, 3 never built), one failure behind it
/// (gen 1's). Proving holds 1 000 bytes, grown 1 ms short of the bound; Connected grew to 1 000 bytes;
/// Backoff is gen 2 refused while proving. Returns the link and the instant its timer is due: the
/// connect timeout, the bound (Proving's only instant: growth there connects it, a tick does nothing),
/// the end of the back-off. Connected has no timer without the encoders' readings (Owed 87; Owed
/// 94's backstop needs them, and no reading is sent here), so its instant is the one where the retired
/// stall rule swapped it: `OUTPUT_SINK_TIMEOUT_MS` after its last growth.
fn output_in(kind: Kind, t: &Thresholds) -> (OutputLink, Ms) {
    let mut o = OutputLink::new();
    assert!(o.on_tick(0, t));
    o.on_progress(0, 1, 1_000, t);
    o.on_error(100, 1, t);
    let built = 100 + t.output_backoff_base_ms;
    assert!(o.on_tick(built, t));
    let up = built + 50;
    let n = t.output_connected_after_ms;
    match kind {
        Kind::Connecting => {}
        Kind::Proving => {
            o.on_progress(up, 2, 500, t);
            o.on_progress(up + n - 1, 2, 1_000, t);
        }
        Kind::Up => {
            o.on_progress(up, 2, 500, t);
            o.on_progress(up + n, 2, 1_000, t);
        }
        Kind::Backoff => {
            o.on_progress(up, 2, 500, t);
            o.on_error(up + 50, 2, t);
        }
    }
    assert_eq!((output_kind(o.state()), o.generation()), (kind, 2), "setup");
    let due = match o.state() {
        OutputState::Connecting { since, .. } => since + t.output_connect_timeout_ms,
        OutputState::Proving { first_progress, .. } => first_progress + n,
        OutputState::Connected => up + n + OUTPUT_SINK_TIMEOUT_MS,
        OutputState::Backoff { until, .. } => until,
    };
    (o, due)
}

#[derive(Debug, Clone, Copy)]
enum OutputEvent {
    Tick,
    /// A total above every one this sink reported (1 001: `output_in` grew it to 1 000 at most).
    Progress,
    /// A total that is not above the highest: 0 for a new sink, a repeat for a proving or connected one.
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
        OutputState::Proving { sent, .. } => sent,
        // Connected keeps no total (Owed 87: nothing reads it); `output_in` grew it to 1 000.
        OutputState::Connected => 1_000,
        _ => 0,
    };
    match e {
        OutputEvent::Tick => return o.on_tick(now, t),
        OutputEvent::Progress => o.on_progress(now, gen, 1_001, t),
        OutputEvent::NoProgress => o.on_progress(now, gen, high, t),
        OutputEvent::Error => o.on_error(now, gen, t),
    }
    false
}

/// The failures `output_in`'s state carries: gen 1's one failure, plus gen 2's refusal in Backoff.
fn setup_attempt(kind: Kind) -> Option<u32> {
    match kind {
        Kind::Connecting | Kind::Proving => Some(1),
        Kind::Up => None,
        Kind::Backoff => Some(2),
    }
}

/// The brief's transitions (Step 6) with the C04 corrections (bytes sent, F-005), Owed 38's
/// (Connected is earned; every end before it is a failure; only Connected resets the count) and Owed
/// 87's (no stall rule: no tick ends Proving or Connected, only the sink's error). No pair here reads
/// the encoders, so Owed 94's backstop, which needs their readings, never fires (it is swept in
/// `the_egress_backstop_ends_only_a_flowing_sink_and_only_while_the_encoders_deliver`):
/// (kind after, built, failures carried after).
fn brief_output(from: Kind, e: OutputEvent) -> (Kind, bool, Option<u32>) {
    use Kind as K;
    use OutputEvent as E;
    let failed = setup_attempt(from).map(|a| a + 1);
    match (from, e) {
        (K::Backoff, E::Tick) => (K::Connecting, true, setup_attempt(from)),
        (K::Connecting, E::Tick) => (K::Backoff, false, failed), // the connect timed out
        (K::Connecting, E::Progress) => (K::Proving, false, setup_attempt(from)), // the first bytes
        (K::Proving, E::Progress) => (K::Up, false, None),       // growth at or past the bound
        (K::Connecting | K::Proving, E::Error) => (K::Backoff, false, failed),
        (K::Up, E::Error) => (K::Backoff, false, Some(1)), // failure 1 again: Connected reset it
        // Everything else, a tick on Proving or Connected among it (Owed 87; no reading of the
        // encoders, so no backstop), changes nothing.
        (k, _) => (k, false, setup_attempt(k)),
    }
}

#[test]
fn every_output_pair_follows_the_brief() {
    let t = Thresholds::default();
    let (mut checked, mut moved) = (0usize, 0usize);
    for kind in OUTPUT_KINDS {
        for e in OUTPUT_EVENTS {
            let (mut o, due) = output_in(kind, &t);
            let built = apply_output(&mut o, e, due, 2, &t);
            let want = brief_output(kind, e);
            let got = (output_kind(o.state()), built, output_attempt(o.state()));
            assert_eq!(got, want, "({kind:?}, {e:?})");
            assert_eq!(o.generation(), 2 + u64::from(want.1), "({kind:?}, {e:?})");
            checked += 1;
            moved += usize::from(want.0 != kind);
        }
    }
    eprintln!("output pairs checked: {checked} ({moved} move the state)");
    assert_eq!(
        checked,
        OUTPUT_KINDS.len() * OUTPUT_EVENTS.len(),
        "every pair, once"
    );
    assert!(moved > 0, "a matrix where nothing moves is vacuous");
}

#[test]
fn output_early_ticks_and_other_generations_change_nothing() {
    let t = Thresholds::default();
    let mut checked = 0usize;
    for kind in OUTPUT_KINDS {
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
    assert_eq!(checked, OUTPUT_KINDS.len() * (1 + 2 * 2));
}

/// What the encoders' readings showed before a sweep's ticks (Owed 94).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// Two readings, both tracks grown: the encoders deliver.
    Delivering,
    /// Two readings, both tracks repeated: feed starvation.
    Starving,
    /// No reading at all.
    Unread,
}

const DELIVERIES: [Delivery; 3] = [Delivery::Delivering, Delivery::Starving, Delivery::Unread];

// Owed 94 (the owner, 2026-10-24, "B"): the egress backstop against every output state and every
// reading of the encoders. Only a sink whose bytes flowed (Proving, Connected), and only while both
// encoders deliver, backs off at `output_egress_flat_ms` after its last growth, and not 1 ms before:
// failure 1 from Connected, one more from Proving. Every other pair does what it does with no reading
// at all (its own timer, or nothing).
#[test]
fn the_egress_backstop_ends_only_a_flowing_sink_and_only_while_the_encoders_deliver() {
    let t = Thresholds::default();
    let flat = t.output_egress_flat_ms;
    let (mut checked, mut fired) = (0usize, 0usize);
    for kind in OUTPUT_KINDS {
        for delivery in DELIVERIES {
            let (mut o, _) = output_in(kind, &t);
            let (mut unread, _) = output_in(kind, &t);
            // `output_in` builds generation 2 at 100 + base and sends its first bytes 50 ms later:
            // these two readings come between, so a delivering run is already going at the growth.
            let built = 100 + t.output_backoff_base_ms;
            for (i, at) in [built + 10, built + 20].into_iter().enumerate() {
                let k = match delivery {
                    Delivery::Delivering => i as u64,
                    Delivery::Starving | Delivery::Unread => 0,
                };
                if delivery != Delivery::Unread {
                    o.on_delivery(at, 2, Encoded { video: k, audio: k });
                }
            }
            // The last growth `output_in` sent: Proving's at the bound less 1 ms, Connected's at it.
            let up = built + 50;
            let last = match kind {
                Kind::Proving => up + t.output_connected_after_ms - 1,
                _ => up + t.output_connected_after_ms,
            };
            let at = last + flat;
            let early = o.on_tick(at - 1, &t);
            unread.on_tick(at - 1, &t);
            let backstop = matches!(kind, Kind::Proving | Kind::Up) && delivery == Delivery::Delivering;
            if backstop {
                assert!(!early, "({kind:?}, {delivery:?}) built 1 ms early");
                assert_eq!(
                    output_kind(o.state()),
                    kind,
                    "({kind:?}, {delivery:?}) 1 ms early"
                );
            }
            let built_now = o.on_tick(at, &t);
            unread.on_tick(at, &t);
            if backstop {
                let failures = setup_attempt(kind).map_or(1, |a| a + 1);
                assert_eq!(
                    (output_kind(o.state()), built_now, output_attempt(o.state())),
                    (Kind::Backoff, false, Some(failures)),
                    "({kind:?}, {delivery:?})"
                );
                let OutputState::Backoff { until, .. } = o.state() else {
                    unreachable!("asserted above");
                };
                assert!(o.on_tick(until, &t), "({kind:?}) the build after the back-off");
                assert_eq!(
                    o.swap_reason(),
                    SwapReason::EgressStalled,
                    "({kind:?}, {delivery:?})"
                );
                fired += 1;
            } else {
                assert_eq!(
                    (output_kind(o.state()), o.generation(), output_attempt(o.state())),
                    (
                        output_kind(unread.state()),
                        unread.generation(),
                        output_attempt(unread.state())
                    ),
                    "({kind:?}, {delivery:?}) moved where no reading leaves it"
                );
            }
            checked += 1;
        }
    }
    eprintln!("backstop pairs checked: {checked} ({fired} fire)");
    assert_eq!(checked, OUTPUT_KINDS.len() * DELIVERIES.len());
    assert_eq!(fired, 2, "Proving and Connected, delivering");
}

// The PR #73 review's P-3, measured: the multi-wake trickle. A destination that drains one RTMP
// message across several socket waits, each `wait` ms apart, completes a message every `message` ms
// (rtmp2sink counts its bytes only once a message is all written, so the total grows only then).
// rtmp2sink's own error comes only when one wait outlasts its socket timeout (OUTPUT_SINK_TIMEOUT_MS
// after the cut, the bytes flowing at full rate until it); a wait under it never errors. So:
// - a wait at or past the timeout: the sink's error, the base back-off, a build (reason `error`),
//   whatever the messages do (Owed 87's path, unchanged);
// - every wait under the timeout and a message slower than the backstop's threshold: no error ever,
//   and only the backstop swaps, output_egress_flat_ms after the cut and the base back-off later
//   (reason `egress_stalled`). Before Owed 94 nothing swapped such a sink: the viewer saw one frame
//   per message for as long as the destination trickled;
// - every wait under the timeout and a message no slower than the threshold less a poll: nothing
//   swaps (the total keeps growing), as before Owed 94.
// The readings come every OUTPUT_PROGRESS_POLL_MS with both encoders delivering, and the ticks every
// widest frame (TICK_WIDEST_MS), from the cut; each row's expected detector and window comes from
// the rule above, never from the link.
#[test]
fn a_multi_wake_trickle_is_swapped_by_the_backstop_only_where_no_socket_wait_reaches_the_timeout() {
    let t = Thresholds::default();
    let (flat, base, poll, tick) = (
        t.output_egress_flat_ms,
        t.output_backoff_base_ms,
        OUTPUT_PROGRESS_POLL_MS,
        TICK_WIDEST_MS,
    );
    let timeout = OUTPUT_SINK_TIMEOUT_MS;
    const HORIZON_MS: Ms = 30_000;
    let waits = [1_000, 2_500, timeout - 1, timeout, timeout + 2_000];
    // u64::MAX: no message completes in the horizon (a keyframe larger than the trickle moves).
    let messages = [2_000, flat - poll, flat + poll, 12_000, 20_000, u64::MAX];
    let (mut rows, mut by_error, mut by_backstop, mut none) = (0, 0, 0, 0);
    for wait in waits {
        for message in messages.into_iter().filter(|m| *m >= wait) {
            let (mut o, _) = output_in(Kind::Up, &t);
            let built = 100 + base;
            let cut = built + 50 + t.output_connected_after_ms;
            o.on_delivery(cut - 10, 2, Encoded { video: 1, audio: 1 });
            o.on_delivery(cut - 5, 2, Encoded { video: 2, audio: 2 });
            let mut k = 2;
            let mut swap = None;
            let mut errored = false;
            for now in cut + 1..=cut + HORIZON_MS {
                let since = now - cut;
                if wait >= timeout && since == timeout && !errored {
                    o.on_error(now, 2, &t);
                    errored = true;
                }
                if since % poll == 0 {
                    let done = if message == u64::MAX { 0 } else { since / message };
                    o.on_progress(now, 2, 1_000 + done, &t);
                    k += 1;
                    o.on_delivery(now, 2, Encoded { video: k, audio: k });
                }
                if since % tick == 0 && o.on_tick(now, &t) {
                    swap = Some((since, o.swap_reason(), o.generation()));
                    break;
                }
            }
            let (want, window) = if wait >= timeout {
                (Some(SwapReason::Error), (timeout + base, timeout + base + tick))
            } else if message > flat {
                (
                    Some(SwapReason::EgressStalled),
                    (flat + base, flat + base + 2 * tick),
                )
            } else {
                (None, (0, 0))
            };
            match (want, swap) {
                (Some(reason), Some((at, got, gen))) => {
                    assert_eq!((got, gen), (reason, 3), "wait {wait}, message {message}");
                    assert!(
                        at >= window.0 && at <= window.1,
                        "wait {wait}, message {message}: swap at +{at}, want {window:?}"
                    );
                    if reason == SwapReason::Error {
                        by_error += 1;
                    } else {
                        by_backstop += 1;
                    }
                    eprintln!("wait {wait:>5} ms, message {message:>20} ms: {got:?} at +{at} ms");
                }
                (None, None) => {
                    none += 1;
                    eprintln!("wait {wait:>5} ms, message {message:>20} ms: no swap in {HORIZON_MS} ms");
                }
                (want, got) => panic!("wait {wait}, message {message}: want {want:?}, got {got:?}"),
            }
            rows += 1;
        }
    }
    eprintln!("multi-wake rows checked: {rows} ({by_error} error, {by_backstop} backstop, {none} none)");
    // Each row's class came from the rule above, and the link matched it in every row; the tally
    // moves with the threshold (the PR #76 review's m-4), so only its anti-vacuity is pinned.
    assert!(
        rows > 0 && by_error > 0 && by_backstop > 0 && none > 0,
        "a class went unchecked"
    );
    assert_eq!(by_error + by_backstop + none, rows);
}

// The PR #76 review's m-2: the steady state of a persistent multi-wake trickle (every socket wait
// under the timeout, so no error ever) whose messages are slower than the backstop's threshold. The
// first swap is from Connected (failure 1, the base back-off). Each later sink sends its `connect`
// command (476 B, F-011) 31 ms after its build and then trickles the same way, so it never earns
// Connected (its next growth would come a message later, past the threshold) and the backstop swaps
// it from Proving, one more failure each time: the back-off grows to its cap and the compositor
// churns sinks every threshold plus back-off, the link never Connected (the beat never reads
// `playing`). Before Owed 94 the Connected sink was never swapped, and read `playing` over one frame
// per message. Each interval's window comes from the back-off rule ("0.5, 1, 2, 2 s": the base
// doubling to the cap) and the grids, never from the link.
#[test]
fn a_persistent_multi_wake_trickle_swaps_every_later_sink_from_proving_and_none_connects() {
    let t = Thresholds::default();
    let (flat, base, cap, poll, tick) = (
        t.output_egress_flat_ms,
        t.output_backoff_base_ms,
        t.output_backoff_cap_ms,
        OUTPUT_PROGRESS_POLL_MS,
        TICK_WIDEST_MS,
    );
    const LATER_SINKS: usize = 4;
    const CONNECT_AT: Ms = 31;
    let mut checked = 0;
    for message in [flat + poll, 2 * flat, u64::MAX] {
        let (mut o, _) = output_in(Kind::Up, &t);
        let built = 100 + base;
        let cut = built + 50 + t.output_connected_after_ms;
        o.on_delivery(cut - 10, 2, Encoded { video: 1, audio: 1 });
        o.on_delivery(cut - 5, 2, Encoded { video: 2, audio: 2 });
        let (mut k, mut gen, mut origin) = (2, 2, cut);
        let mut builds: Vec<(Ms, SwapReason)> = Vec::new();
        let mut now = cut;
        while builds.len() <= LATER_SINKS {
            now += 1;
            assert!(
                now < cut + 20 * (flat + cap),
                "message {message}: stuck at {builds:?}"
            );
            if (now - cut) % poll == 0 {
                let done = match message {
                    u64::MAX => 0,
                    m => now.saturating_sub(origin) / m,
                };
                let total = match gen {
                    2 => 1_000 + done,
                    _ if now >= origin => 476 + done,
                    _ => 0,
                };
                o.on_progress(now, gen, total, &t);
                k += 1;
                o.on_delivery(now, gen, Encoded { video: k, audio: k });
            }
            if (now - cut) % tick == 0 {
                if o.on_tick(now, &t) {
                    builds.push((now, o.swap_reason()));
                    gen = o.generation();
                    origin = now + CONNECT_AT;
                } else if gen > 2 {
                    assert!(
                        !o.is_connected(),
                        "message {message}: gen {gen} connected at {now}"
                    );
                }
            }
        }
        let reasons: Vec<SwapReason> = builds.iter().map(|b| b.1).collect();
        assert!(
            reasons.iter().all(|r| *r == SwapReason::EgressStalled),
            "message {message}: {reasons:?}"
        );
        let mut cadence = Vec::new();
        for (i, pair) in builds.windows(2).enumerate() {
            // The sink built at pair[0] is failure i + 2's: the first later sink follows failure 1.
            let failure = i as u32 + 2;
            let wait = (base << (failure - 1)).min(cap);
            let (lo, hi) = (
                CONNECT_AT + flat + wait,
                CONNECT_AT + poll + flat + wait + 2 * tick,
            );
            let gap = pair[1].0 - pair[0].0;
            assert!(
                (lo..=hi).contains(&gap),
                "message {message}, failure {failure}: {gap} ms between builds, want {lo}..={hi}"
            );
            cadence.push(gap);
            checked += 1;
        }
        eprintln!("persistent trickle, message {message} ms: builds every {cadence:?} ms");
    }
    eprintln!("persistent-trickle intervals checked: {checked}");
    assert_eq!(checked, 3 * LATER_SINKS);
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
    connect_timeouts: usize,
    early_or_quiet_ticks: usize,
    /// Ticks at or past the instant the retired stall rule fired (Owed 87): each changed nothing.
    silent_ticks_proving: usize,
    silent_ticks_connected: usize,
    first_bytes: usize,
    proving_growth: usize,
    growth_1_ms_short: usize,
    connects: usize,
    connects_at_the_bound: usize,
    growth_while_connected: usize,
    repeats_connecting: usize,
    repeats_proving: usize,
    repeats_connected: usize,
    backwards: usize,
    errors_connecting: usize,
    errors_proving: usize,
    errors_connected: usize,
    ignored_while_backing_off: usize,
    stale_connecting: usize,
    stale_proving: usize,
    stale_connected: usize,
    stale_backoff: usize,
    waits_at_cap: usize,
}

#[test]
fn output_link_keeps_its_rules_over_generated_schedules() {
    let t = Thresholds::default();
    // The retired stall rule's instant (Owed 87): a tick there is still drawn, and must change nothing.
    let (quiet, timeout) = (OUTPUT_SINK_TIMEOUT_MS, t.output_connect_timeout_ms);
    let n = t.output_connected_after_ms;
    let mut c = OutputCounts::default();
    for case in 0..CASES {
        let mut g = Lcg(case.wrapping_mul(7_919).wrapping_add(17));
        let mut o = OutputLink::new();
        let mut now: Ms = 0;
        // Facts kept by the harness: the highest total delivered to the current sink, its first and
        // latest growth, and the consecutive failures since the last time a sink earned Connected
        // (Owed 38: growth for `output_connected_after_ms` since the first, by the harness's clock).
        let mut high: u64 = 0;
        let mut failures: u32 = 0;
        let (mut first, mut last): (Option<Ms>, Ms) = (None, 0);
        for _ in 0..STEPS {
            let due = match o.state() {
                OutputState::Connecting { since, .. } => Some(since + timeout),
                // Two instants matter while proving: the bound (on a growth), and the retired rule's.
                OutputState::Proving { .. } if g.next(2) == 0 => first.map(|f| f + n),
                OutputState::Proving { .. } | OutputState::Connected => Some(last + quiet),
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
                        // Owed 87: no tick ends a proving or connected sink, however long it is silent.
                        _ => {
                            assert!(!built, "case {case}: built from {before:?} at {now}");
                            assert_eq!(o.state(), before, "case {case}: a quiet tick changed {before:?}");
                            c.early_or_quiet_ticks += 1;
                            let silent = now - last >= quiet;
                            c.silent_ticks_proving += usize::from(silent && kind == Kind::Proving);
                            c.silent_ticks_connected += usize::from(silent && kind == Kind::Up);
                        }
                    }
                    if built {
                        assert_eq!(o.generation(), gen + 1, "case {case}: one build, one generation");
                        high = 0; // a new sink counts from 0
                        first = None;
                    }
                }
                5..=7 => {
                    let total = g.pick(&[high, high.saturating_sub(1), high + 1, high + 64_000, 0]);
                    o.on_progress(now, gen, total, &t);
                    if kind == Kind::Backoff {
                        assert_eq!(o.state(), before, "case {case}: bytes from a failed sink");
                        c.ignored_while_backing_off += 1;
                    } else if total > high {
                        match kind {
                            Kind::Connecting => {
                                let want = OutputState::Proving {
                                    attempt: failures,
                                    first_progress: now,
                                    sent: total,
                                };
                                assert_eq!(o.state(), want, "case {case}: the first bytes start the proof");
                                first = Some(now);
                                c.first_bytes += 1;
                            }
                            Kind::Proving => {
                                let f = first.expect("a proving sink grew once");
                                if now - f >= n {
                                    assert_eq!(
                                        o.state(),
                                        OutputState::Connected,
                                        "case {case}: grew {} ms after the first",
                                        now - f
                                    );
                                    failures = 0;
                                    c.connects += 1;
                                    c.connects_at_the_bound += usize::from(now - f == n);
                                } else {
                                    let want = OutputState::Proving {
                                        attempt: failures,
                                        first_progress: f,
                                        sent: total,
                                    };
                                    assert_eq!(
                                        o.state(),
                                        want,
                                        "case {case}: grew {} ms after the first",
                                        now - f
                                    );
                                    c.proving_growth += 1;
                                    c.growth_1_ms_short += usize::from(now - f == n - 1);
                                }
                            }
                            _ => {
                                assert_eq!(o.state(), before, "case {case}: Connected reads no total");
                                c.growth_while_connected += 1;
                            }
                        }
                        high = total;
                        last = now;
                    } else {
                        assert_eq!(o.state(), before, "case {case}: {total} ≤ {high} is no progress");
                        c.backwards += usize::from(total < high);
                        match kind {
                            Kind::Connecting => c.repeats_connecting += 1,
                            Kind::Proving => c.repeats_proving += 1,
                            _ => c.repeats_connected += 1,
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
                        match kind {
                            Kind::Connecting => c.errors_connecting += 1,
                            Kind::Proving => c.errors_proving += 1,
                            _ => c.errors_connected += 1,
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
                        o.on_progress(now, other, high + 64_000, &t);
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
                        Kind::Proving => c.stale_proving += 1,
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
        ("connect timeouts", c.connect_timeouts),
        ("quiet ticks", c.early_or_quiet_ticks),
        ("silent ticks while proving", c.silent_ticks_proving),
        ("silent ticks while connected", c.silent_ticks_connected),
        ("first bytes", c.first_bytes),
        ("growth while proving", c.proving_growth),
        ("growth 1 ms short of the bound", c.growth_1_ms_short),
        ("connects", c.connects),
        ("connects exactly at the bound", c.connects_at_the_bound),
        ("growth while connected", c.growth_while_connected),
        ("repeats while connecting", c.repeats_connecting),
        ("repeats while proving", c.repeats_proving),
        ("repeats while connected", c.repeats_connected),
        ("backwards totals", c.backwards),
        ("errors while connecting", c.errors_connecting),
        ("errors while proving", c.errors_proving),
        ("errors while connected", c.errors_connected),
        ("events ignored while backing off", c.ignored_while_backing_off),
        ("stale while connecting", c.stale_connecting),
        ("stale while proving", c.stale_proving),
        ("stale while connected", c.stale_connected),
        ("stale while backing off", c.stale_backoff),
        ("waits at the cap", c.waits_at_cap),
    ];
    for (name, k) in all {
        assert!(k > 0, "never exercised: {name}");
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
    /// C10b: abandons, each checked against the rule that owed it.
    abandons: usize,
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
        // C10b: the generations given up so far, each at most once.
        let mut given_up: Vec<u64> = Vec::new();
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
                    let a = l.on_tick(now, &t);
                    match before {
                        InputState::Backoff { attempt, until } if now >= until => {
                            assert_eq!(
                                a,
                                Some(InputAction::Rebuild { gen: gen + 1 }),
                                "case {case}: a due back-off must build"
                            );
                            assert_eq!(attempt, failures, "case {case}");
                            assert_eq!(l.state(), InputState::Connecting { attempt, since: now });
                            assert_eq!(l.generation(), gen + 1);
                            c.builds += 1;
                        }
                        InputState::Connecting { attempt, since } if now - since >= timeout => {
                            assert_eq!(a, Some(InputAction::Abandon { gen }), "case {case}: timed out");
                            assert!(!given_up.contains(&gen), "case {case}: {gen} given up twice");
                            given_up.push(gen);
                            c.abandons += 1;
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
                            assert_eq!(a, None, "case {case}: acted from {before:?}");
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
                    let a = l.check_stall(now, last_frame, &t);
                    match before {
                        InputState::Running { since } => {
                            let fresh = last_frame.map_or(since, |f| f.max(since));
                            if now.saturating_sub(fresh) >= stall {
                                assert_eq!(a, Some(InputAction::Abandon { gen }), "case {case}: stalled");
                                assert!(!given_up.contains(&gen), "case {case}: {gen} given up twice");
                                given_up.push(gen);
                                c.abandons += 1;
                                lost(&t, &mut c, before, l.state(), now, &mut failures, case);
                                c.stalls += 1;
                            } else {
                                assert_eq!(a, None, "case {case}: a fresh frame holds");
                                assert_eq!(l.state(), before, "case {case}: a fresh frame holds");
                                c.stall_checks_held += 1;
                            }
                        }
                        _ => {
                            assert_eq!(a, None, "case {case}: the stall rule is Running only");
                            assert_eq!(l.state(), before, "case {case}: the stall rule is Running only");
                        }
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
        ("abandons", c.abandons),
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

/// C09's poll of the sink's total (Owed 15: 250 ms), and of the encoders' totals with it (Owed 94).
const POLL_MS: Ms = OUTPUT_PROGRESS_POLL_MS;

/// How far past `last_progress` a simulation may run.
const RUN_MS: Ms = 60_000;

/// The fault at the sink's last progress.
#[derive(Debug, Clone, Copy)]
enum Fault {
    /// The sink errors `after` ms after its last progress. 0 is a reset that takes the last bytes
    /// with it; `OUTPUT_SINK_TIMEOUT_MS` is a destination stall, which since Owed 87 (the owner,
    /// 2026-10-24, "A") ends by rtmp2sink's own socket timeout, that long after its last write.
    Error { after: Ms },
    /// Owed 94 (ii): the output pipeline hangs. No error ever comes, every poll repeats the total, and
    /// the encoders keep delivering: only the egress backstop ends the sink.
    Hung,
}

const RESET: Fault = Fault::Error { after: 0 };
const STALL: Fault = Fault::Error {
    after: OUTPUT_SINK_TIMEOUT_MS,
};
const HUNG: Fault = Fault::Hung;

#[derive(Debug)]
struct Recovery {
    last_progress: Ms,
    /// The sink's error; for a hung output, the backstop's back-off.
    fault_at: Ms,
    recovered_at: Ms,
}

/// One output recovery through the real `OutputLink`, millisecond by millisecond. `ticks[ms]` says
/// whether a tick lands on that millisecond. The sink pushes, its total growing on every poll, the
/// last one at `last_progress`; it earns Connected `output_connected_after_ms` after its first poll
/// (Owed 38), well before the fault. Every poll also reads the encoders, delivering throughout (Owed
/// 94), and the polls stop at the sink's error (C09's watch reads a failed sink no more). The first `failed` sinks built after the fault hang (no bytes
/// and no error, so the domain's connect timeout ends each one). The next one sends its first bytes
/// `connect_ms` after it was built, and the recovery ends there: the output's bytes flow again, and
/// the gap the broadcast sees is over, though that sink earns Connected only later.
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
    let mut first_poll = None;
    let mut encoded = 0u64;
    for now in 0..ticks.len() as Ms {
        let poll = now >= 1 && now.abs_diff(last_progress).is_multiple_of(POLL_MS) && fault_at.is_none();
        if poll && now > last_progress {
            o.on_progress(now, 1, total, t); // the total repeats
        }
        if poll {
            encoded += 1;
            o.on_delivery(
                now,
                1,
                Encoded {
                    video: encoded,
                    audio: encoded,
                },
            );
        }
        if poll && now <= last_progress {
            total += 64_000;
            o.on_progress(now, 1, total, t);
            let first = *first_poll.get_or_insert(now);
            assert_eq!(o.generation(), 1, "the healthy push at {now}");
            assert_eq!(
                o.is_connected(),
                now - first >= t.output_connected_after_ms,
                "the healthy push at {now}, first bytes at {first}"
            );
            if now == last_progress {
                assert!(o.is_connected(), "setup: the push is Connected before its fault");
            }
        }
        if let Fault::Error { after } = fault {
            if now == last_progress + after {
                o.on_error(now, 1, t);
                fault_at = Some(now);
            }
        }
        if let Some((gen, at)) = first_bytes {
            if now == at {
                o.on_progress(now, gen, 4_096, t);
                assert!(
                    matches!(o.state(), OutputState::Proving { .. }),
                    "the recovering sink's first bytes start its proof: {:?}",
                    o.state()
                );
                return Recovery {
                    last_progress,
                    fault_at: fault_at.expect("a fault happened"),
                    recovered_at: now,
                };
            }
        }
        let built = ticks[now as usize] && o.on_tick(now, t);
        if matches!(fault, Fault::Hung) && now > last_progress && fault_at.is_none() && !o.is_connected() {
            assert!(
                matches!(o.state(), OutputState::Backoff { attempt: 1, .. }),
                "a hung output ends only by the backstop: {:?} at {now}",
                o.state()
            );
            fault_at = Some(now);
        }
        if built && o.generation() > 1 {
            assert!(
                fault_at.is_some(),
                "built generation {} before the fault",
                o.generation()
            );
            if o.generation() == 2 {
                let want = match fault {
                    Fault::Error { .. } => SwapReason::Error,
                    Fault::Hung => SwapReason::EgressStalled,
                };
                assert_eq!(o.swap_reason(), want, "{fault:?}");
            }
            builds_after_fault += 1;
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

const SHOWN: u32 = 3;

#[test]
fn output_recovery_paths_against_the_f004_budget_on_a_1_ms_grid() {
    let t = Thresholds::default();
    let worst_connect = t.output_connect_timeout_ms - 1;
    let last = 20_000;
    let ticks = vec![true; (last + RUN_MS) as usize];
    let mut rows = Vec::new();
    for k in 0..=SHOWN {
        let e = recover(&t, &ticks, last, RESET, k, worst_connect);
        let s = recover(&t, &ticks, last, STALL, k, worst_connect);
        let h = recover(&t, &ticks, last, HUNG, k, worst_connect);
        // A hung output's gap runs from its last byte, which may follow the poll before its last
        // growth by as little as 1 ms: so up to one poll before `last_progress`.
        let hung = h.recovered_at - (h.last_progress - POLL_MS);
        let row = (
            k,
            e.recovered_at - e.fault_at,
            s.recovered_at - s.last_progress,
            hung,
        );
        assert_eq!(row.1, worst_from_error(&t, k), "from the error, k = {k}");
        assert_eq!(
            e.fault_at, e.last_progress,
            "a reset takes the last bytes with it"
        );
        assert_eq!(
            row.2,
            OUTPUT_SINK_TIMEOUT_MS + worst_from_error(&t, k),
            "a stall, k = {k}"
        );
        assert_eq!(
            s.fault_at - s.last_progress,
            OUTPUT_SINK_TIMEOUT_MS,
            "a stall ends at the sink's timeout"
        );
        // Owed 94: the backstop backs off exactly its threshold after the last growth (the encoders
        // delivered throughout), then recovers as from an error from Connected.
        assert_eq!(
            h.fault_at - h.last_progress,
            t.output_egress_flat_ms,
            "the backstop, k = {k}"
        );
        assert_eq!(
            row.3,
            POLL_MS + t.output_egress_flat_ms + worst_from_error(&t, k),
            "a hung output, k = {k}"
        );
        eprintln!(
            "k = {k} failed connect(s): from the error {} ms; from the last bytes of a stall {} ms; of a \
             hung output {} ms (budget {OUTPUT_GAP_BUDGET_MS} ms)",
            row.1, row.2, row.3
        );
        rows.push(row);
    }
    let fits =
        |gap: fn(&(u32, Ms, Ms, Ms)) -> Ms| rows.iter().take_while(|r| gap(r) < OUTPUT_GAP_BUDGET_MS).count();
    let (from_error, stall, hung) = (fits(|r| r.1), fits(|r| r.2), fits(|r| r.3));
    eprintln!(
        "failed connects that still fit {OUTPUT_GAP_BUDGET_MS} ms: from the error {}, after a stall {}, \
         after a hang {} (of {} rows)",
        from_error as i64 - 1,
        stall as i64 - 1,
        hung as i64 - 1,
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
    // A single attempt fits on every path, the hung output's included (Owed 94: the threshold is
    // derived so that it does).
    assert!(rows[0].2 < OUTPUT_GAP_BUDGET_MS, "{:?}", rows[0]);
    assert!(rows[0].3 < OUTPUT_GAP_BUDGET_MS, "{:?}", rows[0]);
    // The PR #76 review's m-1: the hung path holds F-004 through one connect's worst case with no
    // failed connect, as the stall path does (Owed 87 (b)), not through the failed connect the error
    // path keeps: the threshold takes the room a failed connect would need.
    assert_eq!(
        hung,
        1,
        "failed connects that fit after a hang: {}",
        hung as i64 - 1
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
// back-off ending, a connect timing out) waits for the next tick, so the gap exceeds the 1 ms worst
// case by at most one tick spacing per timed step, and the budget still holds. The sink's error is
// an event, not a tick: it lands on its millisecond.
#[test]
fn the_budget_holds_on_the_frame_tick_grid_at_every_phase() {
    let t = Thresholds::default();
    let worst_connect = t.output_connect_timeout_ms - 1;
    let (mut cases, mut worst_seen, mut worst_hung) = (0usize, 0, 0);
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
                let e = recover(&t, &ticks, last, RESET, k, worst_connect);
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
                    let s = recover(&t, &ticks, last, STALL, k, worst_connect);
                    let gap = s.recovered_at - s.last_progress;
                    let floor = OUTPUT_SINK_TIMEOUT_MS + worst_from_error(&t, k);
                    assert!(
                        (floor..=floor + slack).contains(&gap),
                        "{mode:?} φ{phase} stall: {gap}"
                    );
                    assert!(gap < OUTPUT_GAP_BUDGET_MS, "{mode:?} φ{phase} stall: {gap} ms");
                    cases += 1;
                    // Owed 94 (the PR #73 review's P-1): a hung output, from its last byte up to a
                    // poll before its last growth. Two timed steps wait for a tick: the backstop and
                    // the build after its back-off.
                    let h = recover(&t, &ticks, last, HUNG, k, worst_connect);
                    let gap = h.recovered_at - (h.last_progress - POLL_MS);
                    let floor = POLL_MS + t.output_egress_flat_ms + worst_from_error(&t, k);
                    assert!(
                        (floor..=floor + 2 * widest).contains(&gap),
                        "{mode:?} φ{phase} hung: {gap}"
                    );
                    assert!(gap < OUTPUT_GAP_BUDGET_MS, "{mode:?} φ{phase} hung: {gap} ms");
                    worst_hung = worst_hung.max(gap);
                    cases += 1;
                }
                cases += 1;
            }
        }
    }
    eprintln!(
        "tick-grid recoveries checked: {cases}; worst from an error through {FAILED_CONNECTS_BUDGETED} \
         failed connect(s): {worst_seen} ms; worst hung output: {worst_hung} ms (budget \
         {OUTPUT_GAP_BUDGET_MS} ms)"
    );
    assert!(cases > 0, "checked 0 recoveries");
}
