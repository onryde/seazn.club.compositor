//! Sweeps over C02's foundations: every configured back-off, and `Window` against an independent
//! reference model. They live here, not inline, because the domain's crate root forbids
//! `clippy::print_stderr`, and a sweep must print the count it checked (AGENTS.md §5).
use domain::backoff::delay;
use domain::config::Thresholds;
use domain::time::Ms;
use domain::window::Window;

/// Every back-off pair the product configures, by name.
fn backoff_pairs(t: &Thresholds) -> [(&'static str, Ms, Ms); 4] {
    [
        ("input", t.input_backoff_base_ms, t.input_backoff_cap_ms),
        ("output", t.output_backoff_base_ms, t.output_backoff_cap_ms),
        ("cef", t.cef_backoff_base_ms, t.cef_backoff_cap_ms),
        (
            "bootstrap",
            t.bootstrap_backoff_base_ms,
            t.bootstrap_backoff_cap_ms,
        ),
    ]
}

/// The definition, written independently: base · 2^attempt if that fits a u64, else "too big".
fn exact(attempt: u32, base: Ms) -> Option<Ms> {
    (0..attempt).try_fold(base, |acc, _| acc.checked_mul(2))
}

#[test]
fn every_configured_backoff_starts_at_its_base_doubles_and_settles_on_its_cap() {
    let t = Thresholds::default();
    let attempts: Vec<u32> = (0..=70).chain([u32::MAX - 1, u32::MAX]).collect();
    let mut checked = 0usize;
    for (name, base, cap) in backoff_pairs(&t) {
        assert!(
            0 < base && base < cap,
            "{name}: base {base} must be positive and below its cap {cap}"
        );
        let mut prev = 0;
        for &a in &attempts {
            let d = delay(a, base, cap);
            assert!(d <= cap, "{name}: attempt {a} waits {d} > cap {cap}");
            assert!(
                d >= prev,
                "{name}: attempt {a} waits {d}, less than the attempt before ({prev})"
            );
            match exact(a, base) {
                Some(v) if v <= cap => assert_eq!(d, v, "{name}: attempt {a} is base·2^{a}"),
                _ => assert_eq!(d, cap, "{name}: attempt {a} is past the cap"),
            }
            prev = d;
            checked += 1;
        }
        assert_eq!(delay(0, base, cap), base, "{name}: the first wait is the base");
        assert_eq!(
            delay(u32::MAX, base, cap),
            cap,
            "{name}: retried forever, it waits the cap"
        );
    }
    eprintln!("backoff sweep checked {checked} (pair, attempt) cases");
    assert!(checked > 0, "backoff sweep checked 0 cases");
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
}

/// The reference model, from the window's contract rather than its code: with a fixed span and time
/// that never goes backwards, the window holds the last `cap` events that were inside the span at
/// the latest record, and a count is those still inside the span now.
fn model_count(all: &[Ms], cap: usize, now: Ms, span: Ms) -> usize {
    let kept = &all[all.len().saturating_sub(cap)..];
    kept.iter().filter(|&&s| now - s <= span).count()
}

#[test]
fn window_matches_the_reference_model_over_generated_sequences() {
    let mut rng = Lcg(0x05EA_2C02);
    let mut checked = 0usize;
    for cap in 1..=6usize {
        for span in [0, 1, 3, 10, 100] {
            for _seed in 0..4 {
                let mut w = Window::new(cap);
                let mut all: Vec<Ms> = Vec::new();
                let mut last_record = 0;
                let mut now: Ms = rng.next(1_000);
                for _op in 0..80 {
                    now += [0, 1, 2, 5, span, span + 1, 37][rng.next(7) as usize];
                    if rng.next(3) < 2 {
                        all.push(now);
                        last_record = now;
                        let want = model_count(&all, cap, now, span);
                        assert_eq!(
                            w.record(now, span),
                            want,
                            "record cap={cap} span={span} now={now}"
                        );
                    } else {
                        let want = model_count(&all, cap, now, span);
                        assert_eq!(w.count(now, span), want, "count cap={cap} span={span} now={now}");
                    }
                    let held = w.count(now, Ms::MAX);
                    let want_held = model_count(&all, cap, last_record, span);
                    assert_eq!(held, want_held, "retained cap={cap} span={span} now={now}");
                    assert!(held <= cap, "bounded: {held} retained with cap {cap}");
                    checked += 1;
                }
            }
        }
    }
    eprintln!("window model sweep checked {checked} operations");
    assert!(checked > 0, "window model sweep checked 0 operations");
}

/// The contract as an executable model for ANY arrival order, written without the window's data
/// structure: on a record at `t`, forget every stamp more than `span` older than `t`, add `t`, and
/// past `cap` forget the oldest stamp BY VALUE (possibly `t` itself). `held` stays in arrival order,
/// so the two counters see the cases where "the first to arrive" is not "the oldest": a stale stamp
/// behind a fresh one, and an eviction that front-eviction would have got wrong.
struct Model {
    held: Vec<Ms>,
    cap: usize,
    pruned_out_of_turn: usize,
    evicted_out_of_turn: usize,
}

impl Model {
    fn age(now: Ms, s: Ms) -> Ms {
        now.saturating_sub(s)
    }

    fn record(&mut self, t: Ms, span: Ms) -> usize {
        let first_fresh = self.held.iter().position(|&s| Self::age(t, s) <= span);
        if first_fresh.is_some_and(|f| self.held[f..].iter().any(|&s| Self::age(t, s) > span)) {
            self.pruned_out_of_turn += 1;
        }
        self.held.retain(|&s| Self::age(t, s) <= span);
        self.held.push(t);
        if self.held.len() > self.cap {
            let oldest = self.held.iter().copied().min().expect("held is non-empty");
            let at = self
                .held
                .iter()
                .position(|&s| s == oldest)
                .expect("the minimum is held");
            if at != 0 {
                self.evicted_out_of_turn += 1;
            }
            self.held.remove(at);
        }
        self.count(t, span)
    }

    fn count(&self, now: Ms, span: Ms) -> usize {
        self.held.iter().filter(|&&s| Self::age(now, s) <= span).count()
    }
}

#[test]
fn window_matches_the_contract_model_when_stamps_arrive_out_of_order() {
    let mut rng = Lcg(0x0000_0C02_0002);
    let (mut checked, mut backwards) = (0usize, 0usize);
    let (mut pruned_out_of_turn, mut evicted_out_of_turn) = (0usize, 0usize);
    for cap in 1..=6usize {
        for span in [0, 1, 3, 10, 100] {
            for _seed in 0..4 {
                let mut w = Window::new(cap);
                let mut m = Model {
                    held: Vec::new(),
                    cap,
                    pruned_out_of_turn: 0,
                    evicted_out_of_turn: 0,
                };
                let mut now: Ms = 1_000 + rng.next(1_000);
                for _op in 0..80 {
                    if rng.next(4) == 0 {
                        now = now.saturating_sub([1, span, span + 1, 50][rng.next(4) as usize]);
                        backwards += 1;
                    } else {
                        now += [0, 1, 2, 5, span, span + 1, 37][rng.next(7) as usize];
                    }
                    if rng.next(3) < 2 {
                        let want = m.record(now, span);
                        assert_eq!(
                            w.record(now, span),
                            want,
                            "record cap={cap} span={span} now={now}"
                        );
                    } else {
                        let want = m.count(now, span);
                        assert_eq!(w.count(now, span), want, "count cap={cap} span={span} now={now}");
                    }
                    let held = w.count(now, Ms::MAX);
                    assert_eq!(held, m.held.len(), "retained cap={cap} span={span} now={now}");
                    assert!(held <= cap, "bounded: {held} retained with cap {cap}");
                    checked += 1;
                }
                pruned_out_of_turn += m.pruned_out_of_turn;
                evicted_out_of_turn += m.evicted_out_of_turn;
            }
        }
    }
    eprintln!(
        "out-of-order window sweep checked {checked} operations: {backwards} backward steps, \
         {pruned_out_of_turn} stale stamps behind a fresh one, {evicted_out_of_turn} evictions of a \
         stamp that was not the first to arrive"
    );
    assert!(checked > 0, "out-of-order window sweep checked 0 operations");
    assert!(backwards > 0, "the sweep never stepped back in time");
    assert!(
        pruned_out_of_turn > 0,
        "the sweep never put a stale stamp behind a fresh one"
    );
    assert!(
        evicted_out_of_turn > 0,
        "the sweep never reached an eviction where arrival order is wrong"
    );
}
