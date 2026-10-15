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
