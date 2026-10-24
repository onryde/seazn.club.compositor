//! Owed 86: the output's starvation alarm, fed one real frame rate a beat. Every level comes from the
//! profile's own declarations (`OutputProfile::fps_floor`, `fps_recovered`), never from a number
//! typed here, and every input that is a measurement is F-024's (the starved windows of C14 case 6).
use std::fs;
use std::path::Path;

use domain::config::OutputProfile;
use domain::starvation::{Alarm, StarvationAlarm};

const BOTH_MODES: [OutputProfile; 2] = [OutputProfile::P720, OutputProfile::P1080];

/// F-024 (Owed 86): what C14 case 6's starved windows carried, the dispatch run and the PR run.
const F024_STARVED_FPS: [f64; 2] = [11.5, 24.7];

/// The rule as the ruling states it, written independently of the module: below the floor raises,
/// and once raised, at or above the recovery level clears.
fn model(raised: bool, fps: f64, p: &OutputProfile) -> (bool, Option<Alarm>) {
    if !fps.is_finite() || fps < 0.0 {
        return (raised, None);
    }
    match raised {
        false if fps < p.fps_floor() => (true, Some(Alarm::Raised)),
        true if fps >= p.fps_recovered() => (false, Some(Alarm::Cleared)),
        r => (r, None),
    }
}

/// The empty case: a fresh alarm is down, and a healthy beat (the profile's own rate) leaves it down.
#[test]
fn a_fresh_alarm_is_down_and_a_healthy_beat_keeps_it_down() {
    let mut checked = 0;
    for p in BOTH_MODES {
        let mut a = StarvationAlarm::default();
        assert!(!a.is_raised(), "{p:?}: a fresh alarm");
        assert_eq!(a.on_reading(f64::from(p.fps), &p), None, "{p:?}");
        assert!(!a.is_raised(), "{p:?}: after a healthy beat");
        checked += 1;
    }
    eprintln!("fresh alarms checked: {checked}");
    assert_eq!(checked, BOTH_MODES.len());
}

/// F-024's starved windows each raise the alarm once; a second starved beat repeats nothing.
#[test]
fn a_starved_beat_raises_the_alarm_once() {
    let mut checked = 0;
    for p in BOTH_MODES {
        for fps in F024_STARVED_FPS {
            assert!(fps < p.fps_floor(), "{p:?}: F-024's {fps} is not starved here");
            let mut a = StarvationAlarm::default();
            assert_eq!(a.on_reading(fps, &p), Some(Alarm::Raised), "{p:?}: {fps}");
            assert!(a.is_raised());
            assert_eq!(a.on_reading(fps, &p), None, "{p:?}: {fps}, the second beat");
            assert!(a.is_raised());
            checked += 1;
        }
    }
    eprintln!("starved readings checked: {checked}");
    assert_eq!(checked, BOTH_MODES.len() * F024_STARVED_FPS.len());
}

/// "Falls below": a beat exactly at the floor is not starved, and the next representable rate under
/// it is.
#[test]
fn the_floor_itself_is_not_starved_and_just_under_it_is() {
    for p in BOTH_MODES {
        let floor = p.fps_floor();
        let mut a = StarvationAlarm::default();
        assert_eq!(a.on_reading(floor, &p), None, "{p:?}: at the floor {floor}");
        assert!(!a.is_raised());
        let under = f64::from_bits(floor.to_bits() - 1);
        assert!(under < floor);
        assert_eq!(a.on_reading(under, &p), Some(Alarm::Raised), "{p:?}: {under}");
    }
}

/// The hysteresis: once raised, a beat between the floor and the recovery level keeps it raised, the
/// recovery level clears it, and the same band then keeps it down. Then the SEQUENCE again: a second
/// starvation raises it a second time, and a second recovery clears it.
#[test]
fn a_raised_alarm_clears_only_at_the_recovery_level_and_can_be_raised_again() {
    for p in BOTH_MODES {
        let (floor, recovered) = (p.fps_floor(), p.fps_recovered());
        let band = (floor + recovered) / 2.0;
        assert!(floor < band && band < recovered, "{p:?}: no band to test");
        let mut a = StarvationAlarm::default();
        for round in 1..=2 {
            assert_eq!(
                a.on_reading(F024_STARVED_FPS[0], &p),
                Some(Alarm::Raised),
                "{p:?} r{round}"
            );
            assert_eq!(
                a.on_reading(floor, &p),
                None,
                "{p:?} r{round}: the floor itself, raised"
            );
            assert_eq!(
                a.on_reading(band, &p),
                None,
                "{p:?} r{round}: in the band, raised"
            );
            assert!(a.is_raised(), "{p:?} r{round}");
            let under = f64::from_bits(recovered.to_bits() - 1);
            assert_eq!(
                a.on_reading(under, &p),
                None,
                "{p:?} r{round}: just under recovery"
            );
            assert_eq!(
                a.on_reading(recovered, &p),
                Some(Alarm::Cleared),
                "{p:?} r{round}"
            );
            assert!(!a.is_raised(), "{p:?} r{round}");
            assert_eq!(a.on_reading(band, &p), None, "{p:?} r{round}: in the band, down");
            assert!(!a.is_raised(), "{p:?} r{round}");
        }
    }
}

/// A reading that is not a measurement (not finite, or negative: the contract's `minimum: 0`) changes
/// nothing, raised or down.
#[test]
fn a_reading_that_is_not_a_measurement_changes_nothing() {
    let mut checked = 0;
    for p in BOTH_MODES {
        for raised in [false, true] {
            for bad in [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                -1.0,
                -f64::MIN_POSITIVE,
            ] {
                let mut a = StarvationAlarm::default();
                if raised {
                    assert_eq!(
                        a.on_reading(0.0, &p),
                        Some(Alarm::Raised),
                        "a measured 0 is starved"
                    );
                }
                assert_eq!(a.on_reading(bad, &p), None, "{p:?} raised={raised}: {bad}");
                assert_eq!(a.is_raised(), raised, "{p:?} raised={raised}: {bad}");
                checked += 1;
            }
        }
    }
    eprintln!("non-measurements checked: {checked}");
    assert_eq!(checked, BOTH_MODES.len() * 2 * 5);
}

/// A sweep against the model: every rate from 0 to twice the profile's in quarter-frame steps, fed
/// in a walk that goes down, up and down again, so both transitions and both holds are reached. Each
/// beat's change and state must be the model's.
#[test]
fn every_rate_on_a_walk_matches_the_rule() {
    let mut checked = 0;
    let (mut raises, mut clears, mut held_raised, mut held_down) = (0, 0, 0, 0);
    for p in BOTH_MODES {
        let top = 8 * p.fps; // quarter frames, up to twice the profile's rate
        let down: Vec<f64> = (0..=top).rev().map(|q| f64::from(q) / 4.0).collect();
        let up: Vec<f64> = (0..=top).map(|q| f64::from(q) / 4.0).collect();
        let walk = down.iter().chain(&up).chain(&down);
        let mut a = StarvationAlarm::default();
        let mut raised = false;
        for &fps in walk {
            let (next, want) = model(raised, fps, &p);
            assert_eq!(a.on_reading(fps, &p), want, "{p:?}: {fps} (raised {raised})");
            assert_eq!(a.is_raised(), next, "{p:?}: {fps}");
            match want {
                Some(Alarm::Raised) => raises += 1,
                Some(Alarm::Cleared) => clears += 1,
                None if next => held_raised += 1,
                None => held_down += 1,
            }
            raised = next;
            checked += 1;
        }
    }
    eprintln!(
        "rates checked: {checked}; raises {raises}, clears {clears}, held raised {held_raised}, held down {held_down}"
    );
    // Each profile's walk raises twice (two descents) and clears once (one ascent).
    assert_eq!((raises, clears), (2 * BOTH_MODES.len(), BOTH_MODES.len()));
    assert!(held_raised > 0 && held_down > 0, "a hold was never reached");
    assert!(checked > 0);
}

/// The recovery level is the harness's fps bar, one number held in two places: here
/// (`OUTPUT_FPS_RECOVERED_SHARE` of a 30 fps profile) and in the harness (`BUDGET_FPS_MEAN_MIN`,
/// which the harness's own drift test keeps equal to the committed `harness/thresholds.json`). Read
/// from that file, so neither moves alone (the PR #69 review's m2).
#[test]
fn the_recovery_level_is_the_harness_fps_bar() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../harness/thresholds.json");
    let text = fs::read_to_string(&path).expect("harness/thresholds.json is committed");
    let v: serde_json::Value = serde_json::from_str(&text).expect("harness/thresholds.json is JSON");
    let bar = v["budget_fps_mean_min"]
        .as_f64()
        .expect("budget_fps_mean_min is a number");
    let mut checked = 0;
    for p in BOTH_MODES {
        assert!(
            (p.fps_recovered() - bar).abs() < 1e-12,
            "{p:?}: recovered {} against the harness's {bar}",
            p.fps_recovered()
        );
        checked += 1;
    }
    eprintln!("profiles checked against the harness's fps bar ({bar}): {checked}");
    assert_eq!(checked, BOTH_MODES.len());
}
