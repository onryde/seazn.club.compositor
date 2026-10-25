//! The Machine's environment (C12 Step 1), the stop grace (amendment 2026-10-15) and the clock
//! (Step 5). Expected values are written from their sources (R1's ISO deadline, the calendar, D15),
//! never read back from the parser.
use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use compositor::clock::Clock;
use compositor::env::{self, Drivers, EnvError};
use compositor::{EXIT_HEADROOM_MS, R1_STOP_GRACE_MS};
use domain::config::Thresholds;

const TOKEN: &str = "SECRET-TOKEN-VALUE";

fn base() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        (env::SESSION_ID, "3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c".to_owned()),
        (env::JOB_TOKEN, TOKEN.to_owned()),
        (env::APP_URL, "https://stg.seazn.club".to_owned()),
        (env::RELAY_DEADLINE_AT, "2026-10-14T18:30:00.000Z".to_owned()),
        (env::COMPOSITOR_DRIVERS, "live".to_owned()),
    ])
}

fn parse(m: &BTreeMap<&'static str, String>, release: bool) -> Result<env::Env, EnvError> {
    env::parse(|k| m.get(k).cloned(), release)
}

#[test]
fn parses_r1_iso_deadline_with_millis() {
    // 2026-10-14 is day 20 740 since 1970-01-01 (56 years, 14 of them leap: 56 × 365 + 14 = 20 454;
    // then Jan..Sep 2026 = 273 days, + 13 = 20 740).
    let day_ms = 20_740 * 86_400_000;
    let at = day_ms + 18 * 3_600_000 + 30 * 60_000;
    assert_eq!(at, 1_792_002_600_000);
    let mut m = base();
    assert_eq!(parse(&m, true).unwrap().deadline_ms, at);
    m.insert(env::RELAY_DEADLINE_AT, "2026-10-14T18:30:00.250Z".to_owned());
    assert_eq!(
        parse(&m, true).unwrap().deadline_ms,
        at + 250,
        "the fraction is kept"
    );
    for bad in ["2026-10-14 18:30:00", "tomorrow", "1969-12-31T23:59:59.000Z"] {
        m.insert(env::RELAY_DEADLINE_AT, bad.to_owned());
        assert_eq!(
            parse(&m, true).err(),
            Some(EnvError::Invalid(env::RELAY_DEADLINE_AT)),
            "{bad}"
        );
    }
}

#[test]
fn missing_each_required_key_is_refused_naming_the_key() {
    let mut checked = 0;
    for key in env::REQUIRED {
        for gone in [None, Some(String::new())] {
            let mut m = base();
            match &gone {
                None => m.remove(key),
                Some(v) => m.insert(key, v.clone()),
            };
            let err = parse(&m, true).expect_err(key);
            assert_eq!(err, EnvError::Missing(key));
            let text = err.to_string();
            assert!(text.contains(key), "{text:?} does not name {key}");
            for (k, v) in &base() {
                assert!(!text.contains(v.as_str()), "{text:?} carries {k}'s value");
            }
            assert!(!text.contains(TOKEN), "{text:?}");
            checked += 1;
        }
    }
    eprintln!("refusals checked: {checked}");
    assert_eq!(checked, 8, "anti-vacuity: 4 keys, unset and empty");
}

#[test]
fn drivers_unset_refuses_in_release_and_defaults_fake_in_debug() {
    let mut m = base();
    m.remove(env::COMPOSITOR_DRIVERS);
    let err = parse(&m, true).expect_err("release refuses");
    assert_eq!(err, EnvError::DriversUnset);
    assert_eq!(err.to_string(), "COMPOSITOR_DRIVERS must be set (live|fake)");
    assert_eq!(parse(&m, false).unwrap().drivers, Drivers::Fake);
    m.insert(env::COMPOSITOR_DRIVERS, String::new());
    assert_eq!(
        parse(&m, true).err(),
        Some(EnvError::DriversUnset),
        "empty is unset"
    );
    // The positive pair: a set value is taken in both builds.
    for (word, want) in [("live", Drivers::Live), ("fake", Drivers::Fake)] {
        m.insert(env::COMPOSITOR_DRIVERS, word.to_owned());
        assert_eq!(parse(&m, true).unwrap().drivers, want);
        assert_eq!(parse(&m, false).unwrap().drivers, want);
    }
}

#[test]
fn drivers_rejects_unknown_value() {
    let mut m = base();
    for bad in ["Live", "fakes", "mock", " live"] {
        m.insert(env::COMPOSITOR_DRIVERS, bad.to_owned());
        assert_eq!(parse(&m, false).err(), Some(EnvError::DriversUnknown), "{bad}");
        assert_eq!(parse(&m, true).err(), Some(EnvError::DriversUnknown), "{bad}");
    }
}

#[test]
fn app_origin_is_scheme_host_port() {
    let mut m = base();
    for (url, origin) in [
        ("https://seazn.club", "https://seazn.club"),
        ("https://stg.seazn.club/", "https://stg.seazn.club"),
        ("http://localhost:3000/x", "http://localhost:3000"),
    ] {
        m.insert(env::APP_URL, url.to_owned());
        let e = parse(&m, true).unwrap();
        assert_eq!(e.app_origin, origin, "{url}");
        assert_eq!(e.app_url, url, "APP_URL itself is kept for the plane");
    }
    m.insert(env::APP_URL, "seazn.club".to_owned());
    assert_eq!(parse(&m, true).err(), Some(EnvError::Invalid(env::APP_URL)));
}

/// The fake mode's knobs: read in fake mode only, and a value they do not take is refused.
#[test]
fn fake_knobs_parse_in_fake_mode_only() {
    let mut m = base();
    m.insert(env::COMPOSITOR_FAKE_CONTROL, "http".to_owned());
    m.insert(env::COMPOSITOR_FAKE_START_FAIL, "boom".to_owned());
    m.insert(env::COMPOSITOR_FAKE_ZYGOTE_GONE, "1".to_owned());
    let live = parse(&m, true).unwrap();
    assert_eq!(live.fake, env::FakeKnobs::default(), "a live run ignores them");
    m.insert(env::COMPOSITOR_DRIVERS, "fake".to_owned());
    let fake = parse(&m, true).unwrap().fake;
    assert!(fake.control_http);
    assert_eq!(fake.start_fail.as_deref(), Some("boom"));
    assert!(fake.zygote_gone);
    for (key, bad) in [
        (env::COMPOSITOR_FAKE_CONTROL, "https"),
        (env::COMPOSITOR_FAKE_ZYGOTE_GONE, "yes"),
    ] {
        let mut b = m.clone();
        b.insert(key, bad.to_owned());
        assert_eq!(parse(&b, true).err(), Some(EnvError::Invalid(key)), "{key}={bad}");
    }
}

/// `Env`'s `Debug` never prints the token.
#[test]
fn env_debug_hides_the_token() {
    let e = parse(&base(), true).unwrap();
    let d = format!("{e:?}");
    assert!(!d.contains(TOKEN), "{d}");
    assert!(d.contains("****"), "{d}");
}

/// D15: the Final hold, the drain and the exit headroom fit inside R1's stop grace. Red at R1's old
/// 10 s grace (10 + 7 + 3 = 20 > 10); green at the ruled 20 s. Never `#[ignore]`d.
#[test]
fn final_hold_and_drain_fit_inside_r1_stop_grace() {
    let t = Thresholds::default();
    let used = t.final_hold_ms + t.drain_timeout_ms + EXIT_HEADROOM_MS;
    eprintln!(
        "stop grace: final_hold {} + drain {} + headroom {EXIT_HEADROOM_MS} = {used} of {R1_STOP_GRACE_MS} ms",
        t.final_hold_ms, t.drain_timeout_ms
    );
    assert!(used <= R1_STOP_GRACE_MS, "{used} > {R1_STOP_GRACE_MS}");
    assert!(
        t.final_hold_ms > 0 && t.drain_timeout_ms > 0,
        "both terms are real"
    );
}

/// The clock reads the wall once: a wall that steps back 5 s after the start (NTP, a VM resume) never
/// moves `now_ms` back. The injected wall is a day ahead of the real one, so a clock that read the
/// real `SystemTime` per call would also go back (by a day) and fail here.
#[test]
fn now_ms_is_monotonic_across_a_wall_clock_step() {
    let ahead = SystemTime::now() + Duration::from_secs(86_400);
    let wall = Cell::new(ahead);
    let clock = Clock::from_wall(|| wall.get());
    let first = clock.now_ms();
    let want = ahead.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    assert!(
        first >= want && first < want + 1_000,
        "{first} vs the injected {want}"
    );
    wall.set(ahead - Duration::from_secs(5));
    let mut last = first;
    let mut checked = 0;
    for _ in 0..200 {
        let now = clock.now_ms();
        assert!(now >= last, "now_ms went back: {last} then {now}");
        last = now;
        checked += 1;
        std::thread::sleep(Duration::from_micros(200));
    }
    assert!(last >= first, "and it advanced");
    eprintln!("clock reads checked: {checked}");
    assert_eq!(checked, 200);
}

/// Owed 82 (owner option A, 2026-10-24), a `test-hooks` build: `COMPOSITOR_TEST_HOLD_TEARDOWNS` is a
/// count of ASCII digits, in both modes (C14 case 16's legs set 2 and 3); unset, empty or 0 holds
/// none; anything else is refused naming the key, never the value.
#[cfg(feature = "test-hooks")]
#[test]
fn test_hook_hold_teardowns_takes_a_count_and_refuses_anything_else() {
    let key = env::COMPOSITOR_TEST_HOLD_TEARDOWNS;
    let mut checked = 0;
    for mode in ["live", "fake"] {
        let mut m = base();
        m.insert(env::COMPOSITOR_DRIVERS, mode.to_owned());
        assert_eq!(parse(&m, true).unwrap().hooks.hold_teardowns, 0, "{mode}: unset");
        checked += 1;
        for (v, want) in [("", 0), ("0", 0), ("2", 2), ("3", 3), ("03", 3), ("12", 12)] {
            m.insert(key, v.to_owned());
            assert_eq!(
                parse(&m, true).unwrap().hooks.hold_teardowns,
                want,
                "{mode} {v:?}"
            );
            checked += 1;
        }
        for bad in [
            "two",
            "-1",
            "+2",
            " 2",
            "2 ",
            "2.0",
            "0x2",
            "99999999999999999999999",
        ] {
            m.insert(key, bad.to_owned());
            let err = parse(&m, true).expect_err(bad);
            assert_eq!(err, EnvError::Invalid(key), "{mode} {bad:?}");
            let text = err.to_string();
            assert!(text.contains(key), "{text:?} does not name {key}");
            assert!(!text.contains(bad.trim()), "{text:?} carries the value {bad:?}");
            checked += 1;
        }
    }
    eprintln!("hold-teardowns values checked: {checked}");
    assert_eq!(checked, 2 * (1 + 6 + 8), "anti-vacuity");
}

/// Owed 82: a build without the feature does not read the key at all, so a value it would refuse
/// parses, and nothing is armed (`TestHooks` is empty).
#[cfg(not(feature = "test-hooks"))]
#[test]
fn test_hook_hold_teardowns_is_not_read_without_the_feature() {
    let mut checked = 0;
    for v in ["3", "two"] {
        let mut m = base();
        m.insert("COMPOSITOR_TEST_HOLD_TEARDOWNS", v.to_owned());
        let e = parse(&m, true).unwrap_or_else(|err| panic!("{v:?} refused: {err}"));
        assert_eq!(e.hooks, env::TestHooks::default(), "{v:?}");
        checked += 1;
    }
    assert_eq!(checked, 2, "anti-vacuity");
}

/// Owed 99 (option B, owner 2026-10-25 "Apply rec", relayed by the coordinator), a `test-hooks` build:
/// `COMPOSITOR_TEST_WITHHOLD_PAGES` is a count of ASCII digits, in both modes (C14 case 19's leg sets
/// one fewer than `cef_gpu_deaths_to_restart`); unset, empty or 0 withholds none; anything else is
/// refused naming the key, never the value.
#[cfg(feature = "test-hooks")]
#[test]
fn test_hook_withhold_pages_takes_a_count_and_refuses_anything_else() {
    let key = env::COMPOSITOR_TEST_WITHHOLD_PAGES;
    let mut checked = 0;
    for mode in ["live", "fake"] {
        let mut m = base();
        m.insert(env::COMPOSITOR_DRIVERS, mode.to_owned());
        assert_eq!(parse(&m, true).unwrap().hooks.withhold_pages, 0, "{mode}: unset");
        checked += 1;
        for (v, want) in [("", 0), ("0", 0), ("1", 1), ("2", 2), ("02", 2), ("12", 12)] {
            m.insert(key, v.to_owned());
            assert_eq!(
                parse(&m, true).unwrap().hooks.withhold_pages,
                want,
                "{mode} {v:?}"
            );
            checked += 1;
        }
        for bad in [
            "two",
            "-1",
            "+2",
            " 2",
            "2 ",
            "2.0",
            "0x2",
            "99999999999999999999999",
        ] {
            m.insert(key, bad.to_owned());
            let err = parse(&m, true).expect_err(bad);
            assert_eq!(err, EnvError::Invalid(key), "{mode} {bad:?}");
            let text = err.to_string();
            assert!(text.contains(key), "{text:?} does not name {key}");
            assert!(!text.contains(bad.trim()), "{text:?} carries the value {bad:?}");
            checked += 1;
        }
    }
    eprintln!("withhold-pages values checked: {checked}");
    assert_eq!(checked, 2 * (1 + 6 + 8), "anti-vacuity");
}

/// Owed 99: a build without the feature does not read the key at all, so a value it would refuse
/// parses, and nothing is armed (`TestHooks` is empty).
#[cfg(not(feature = "test-hooks"))]
#[test]
fn test_hook_withhold_pages_is_not_read_without_the_feature() {
    let mut checked = 0;
    for v in ["2", "two"] {
        let mut m = base();
        m.insert("COMPOSITOR_TEST_WITHHOLD_PAGES", v.to_owned());
        let e = parse(&m, true).unwrap_or_else(|err| panic!("{v:?} refused: {err}"));
        assert_eq!(e.hooks, env::TestHooks::default(), "{v:?}");
        checked += 1;
    }
    assert_eq!(checked, 2, "anti-vacuity");
}

/// Owed 94 (the owner, 2026-10-24, "B"), a `test-hooks` build: each sink-stall key takes ms of ASCII
/// digits, in both modes (C14's output-hung case); unset, empty or 0 plants nothing; anything else,
/// or both keys at once, is refused naming a key, never the value.
#[cfg(feature = "test-hooks")]
#[test]
fn test_hook_sink_stall_takes_ms_on_one_key_and_refuses_anything_else() {
    use env::SinkStallHook::{Hang, Hold};
    let (hold, hang) = (
        env::COMPOSITOR_TEST_HOLD_SINK_AFTER_MS,
        env::COMPOSITOR_TEST_HANG_SINK_AFTER_MS,
    );
    let mut checked = 0;
    for mode in ["live", "fake"] {
        for (key, kind) in [(hold, Hold), (hang, Hang)] {
            let mut m = base();
            m.insert(env::COMPOSITOR_DRIVERS, mode.to_owned());
            assert_eq!(parse(&m, true).unwrap().hooks.sink_stall, None, "{mode}: unset");
            checked += 1;
            for (v, want) in [
                ("", None),
                ("0", None),
                ("12000", Some((kind, 12_000))),
                ("015000", Some((kind, 15_000))),
            ] {
                m.insert(key, v.to_owned());
                assert_eq!(
                    parse(&m, true).unwrap().hooks.sink_stall,
                    want,
                    "{mode} {key} {v:?}"
                );
                checked += 1;
            }
            for bad in ["ten", "-1", "+5", " 5", "5 ", "5.0", "99999999999999999999999"] {
                m.insert(key, bad.to_owned());
                let err = parse(&m, true).expect_err(bad);
                assert_eq!(err, EnvError::Invalid(key), "{mode} {key} {bad:?}");
                let text = err.to_string();
                assert!(text.contains(key), "{text:?} does not name {key}");
                assert!(!text.contains(bad.trim()), "{text:?} carries the value {bad:?}");
                checked += 1;
            }
        }
        // One stall at a time: both set is refused; one of them 0 is unset, and the other plants.
        let mut m = base();
        m.insert(env::COMPOSITOR_DRIVERS, mode.to_owned());
        m.insert(hold, "12000".to_owned());
        m.insert(hang, "12000".to_owned());
        assert_eq!(
            parse(&m, true).expect_err("both"),
            EnvError::Invalid(hang),
            "{mode}"
        );
        m.insert(hang, "0".to_owned());
        assert_eq!(
            parse(&m, true).unwrap().hooks.sink_stall,
            Some((Hold, 12_000)),
            "{mode}"
        );
        checked += 2;
    }
    eprintln!("sink-stall values checked: {checked}");
    assert_eq!(checked, 2 * (2 * (1 + 4 + 7) + 2), "anti-vacuity");
}

/// Owed 94: a build without the feature does not read the sink-stall keys at all, so values it would
/// refuse parse, and nothing is armed.
#[cfg(not(feature = "test-hooks"))]
#[test]
fn test_hook_sink_stall_is_not_read_without_the_feature() {
    let mut checked = 0;
    for v in ["12000", "ten"] {
        let mut m = base();
        m.insert("COMPOSITOR_TEST_HOLD_SINK_AFTER_MS", v.to_owned());
        m.insert("COMPOSITOR_TEST_HANG_SINK_AFTER_MS", v.to_owned());
        let e = parse(&m, true).unwrap_or_else(|err| panic!("{v:?} refused: {err}"));
        assert_eq!(e.hooks, env::TestHooks::default(), "{v:?}");
        checked += 1;
    }
    assert_eq!(checked, 2, "anti-vacuity");
}
