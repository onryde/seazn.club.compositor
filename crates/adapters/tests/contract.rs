//! The vendored contract is the arbiter: our wire output must validate against it, and its fixtures
//! must decode into our types. Expected values come from the vendored files, never from our code.
use adapters::control_http::{
    heartbeat_wire, heartbeat_wire_with, parse_reply, parse_session, programme_phase_wire,
};
use adapters::sponsor_line::parse_sponsor_line;
use app::ports::{FetchError, Measured};
use domain::airings::{AiringBatch, AiringBuffer, AiringLine, AiringSource, AiringStyle, AIRING_BUFFER_CAP};
use domain::cef_health::CefHealth;
use domain::config::{OutputProfile, Thresholds};
use domain::health::{
    BeatState, ExitDetail, ExitInfo, GraphicsStatus, HealthReport, LinkStatus, PhaseChannel,
};
use domain::ladder::Rung;
use domain::lifecycle::ExitReason;
use domain::output_link::Encoded;
use domain::programme::ProgrammePhase;
use domain::supervisor::{Desired, Effect, Event, Refusal, SignalKind, Supervisor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `docs/_INDEX.md` Owed 24: web's own pin of `docs/contracts/relay-job.v1.json`, copied from
/// `seazn.club` `apps/web/src/server/api-v1/__tests__/relay-contract.test.ts` (`SHA256`, at
/// 4a9e180958 on web main, web PR #1052's merge: `output`'s description only, 1080p30 the default
/// (owner ruling 2026-10-27, F-033); before it 228a3b7057, web PR #1040's merge, the reply's
/// `phoneLiveEdge`, Owed 108 fix (b); before that dec7b95d9, the `restart` exit word, Owed 66). A
/// contract bump moves web's constant and this one together; a vendored file that hashes otherwise
/// drifted from the contract web enforces.
const WEB_PINNED_CONTRACT_SHA256: &str = "90a21541970c566d71a0634503708643c7c1c8de1d36730b6ca907f396d321fe";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts")
}
fn fixtures_dir() -> PathBuf {
    root().join("fixtures/relay-job.v1")
}
fn fixture(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures_dir().join(name)).expect(name)).expect("json")
}
fn contract() -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join("relay-job.v1.json")).unwrap()).unwrap()
}
fn schema_def(def: &str) -> jsonschema::Validator {
    let doc = contract();
    // A fresh wrapper, never a mutated copy of the doc: 2020-12 ANDs sibling keywords beside $ref,
    // so a root that carried its own schema would leak into every def's validation.
    let s = json!({ "$schema": doc["$schema"], "$defs": doc["$defs"], "$ref": format!("#/$defs/{def}") });
    jsonschema::validator_for(&s).expect("schema compiles")
}
fn violations(v: &jsonschema::Validator, wire: &Value) -> Vec<String> {
    v.iter_errors(wire).map(|e| e.to_string()).collect()
}
/// The heartbeat property's declared words (its `anyOf`'s string branch), from the vendored schema.
fn declared_words(key: &str) -> Vec<String> {
    let prop = &contract()["$defs"]["heartbeat"]["properties"][key];
    let branch = prop["anyOf"]
        .as_array()
        .and_then(|a| a.iter().find(|b| b["type"] == "string"))
        .unwrap_or(prop);
    let words: Vec<String> = branch["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{key}: no enum in the contract"))
        .iter()
        .map(|w| w.as_str().unwrap().to_owned())
        .collect();
    assert!(!words.is_empty(), "{key}: empty enum");
    words
}

// ---- every variant, kept complete by exhaustive matches (a new variant fails to compile) ----

fn all_states() -> Vec<BeatState> {
    let all = vec![
        BeatState::Starting,
        BeatState::Playing,
        BeatState::Stalled,
        BeatState::Stopped,
    ];
    for s in &all {
        match s {
            BeatState::Starting | BeatState::Playing | BeatState::Stalled | BeatState::Stopped => {}
        }
    }
    all
}
fn all_phases() -> Vec<ProgrammePhase> {
    use ProgrammePhase::*;
    let all = vec![Waiting, Live, Frozen, Reconnecting, Final];
    for p in &all {
        match p {
            Waiting | Live | Frozen | Reconnecting | Final => {}
        }
    }
    all
}
fn all_links() -> Vec<LinkStatus> {
    let all = vec![LinkStatus::Connecting, LinkStatus::Connected, LinkStatus::Backoff];
    for l in &all {
        match l {
            LinkStatus::Connecting | LinkStatus::Connected | LinkStatus::Backoff => {}
        }
    }
    all
}
fn all_graphics() -> Vec<GraphicsStatus> {
    use GraphicsStatus::*;
    let all = vec![Starting, Ready, Restarting, Failed];
    for g in &all {
        match g {
            Starting | Ready | Restarting | Failed => {}
        }
    }
    all
}
fn all_rungs() -> Vec<Rung> {
    let all = vec![Rung::Full, Rung::OverlayHalfRate];
    for r in &all {
        match r {
            Rung::Full | Rung::OverlayHalfRate => {}
        }
    }
    all
}
fn all_channels() -> Vec<PhaseChannel> {
    let all = vec![PhaseChannel::Loopback, PhaseChannel::Web];
    for c in &all {
        match c {
            PhaseChannel::Loopback | PhaseChannel::Web => {}
        }
    }
    all
}

/// A plain report: every enum at its first variant, every counter 0, nothing optional set.
fn report() -> HealthReport {
    HealthReport {
        state: BeatState::Starting,
        programme_phase: ProgrammePhase::Waiting,
        input: LinkStatus::Connecting,
        output: LinkStatus::Connecting,
        graphics: GraphicsStatus::Starting,
        input_kicks: 0,
        input_kick_alert: false,
        pipeline_rebuilds: 0,
        sink_swaps: 0,
        dropped_frames: 0,
        rung: Rung::Full,
        score_stale: None,
        heartbeat_failures: 0,
        phase_channel: None,
        exit: None,
    }
}
fn stopped(reason: ExitReason, detail: Option<&str>) -> HealthReport {
    HealthReport {
        state: BeatState::Stopped,
        exit: Some(ExitInfo {
            reason,
            detail: detail.map(ExitDetail::new),
        }),
        ..report()
    }
}
fn full_measured() -> Measured {
    Measured {
        fps: Some(30.0),
        bitrate_kbps: Some(4_000),
        egress_bytes: Some(1_234_567),
        measured_latency_ms: Some(2_900),
        encoder_speed: Some(1.0),
        cpu_pct: Some(41.5),
        mem_mb: Some(812),
        camera_frames_replaced: Some(57),
    }
}

// ---- the files ----

#[test]
fn vendored_files_match_their_pins() {
    let pins: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("VENDORED.json")).unwrap()).unwrap();
    let files = pins["files"].as_object().expect("files map");
    eprintln!("pinned files checked: {}", files.len());
    assert!(!files.is_empty(), "anti-vacuity: no pinned files");
    for (rel, want) in files {
        let got = hex::encode(Sha256::digest(std::fs::read(root().join(rel)).expect(rel)));
        assert_eq!(
            &got,
            want.as_str().unwrap(),
            "{rel} drifted from its pin — re-vendor, never hand-edit"
        );
    }
}

/// The pin map is the directory: no unpinned file sits beside the pinned ones, and the contract plus
/// every fixture is pinned (the script's "fixtures + 1").
#[test]
fn every_vendored_file_is_pinned_and_every_pin_names_one() {
    let pins: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("VENDORED.json")).unwrap()).unwrap();
    let pinned: Vec<String> = pins["files"].as_object().unwrap().keys().cloned().collect();
    let mut on_disk = Vec::new();
    let mut stack = vec![root()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            assert!(
                !meta.file_type().is_symlink(),
                "{}: a symlink in contracts/",
                p.display()
            );
            if meta.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(root())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel != "VENDORED.json" {
                    on_disk.push(rel);
                }
            }
        }
    }
    on_disk.sort();
    let mut sorted = pinned.clone();
    sorted.sort();
    eprintln!(
        "vendored files on disk: {}, pinned: {}",
        on_disk.len(),
        pinned.len()
    );
    assert!(
        on_disk.len() > 1,
        "anti-vacuity: the contract and at least one fixture"
    );
    assert_eq!(
        on_disk, sorted,
        "contracts/ and VENDORED.json disagree: re-vendor with the script"
    );
    let fixtures = std::fs::read_dir(fixtures_dir()).unwrap().count();
    assert_eq!(pinned.len(), fixtures + 1, "every fixture plus the contract");
    assert!(
        pinned.iter().any(|p| p == "relay-job.v1.json"),
        "the contract itself is pinned"
    );
}

/// Owed 24: the vendored contract is the one web pins, from a full web commit sha.
#[test]
fn the_vendored_contract_is_the_one_web_pins() {
    let got = hex::encode(Sha256::digest(
        std::fs::read(root().join("relay-job.v1.json")).unwrap(),
    ));
    assert_eq!(
        got, WEB_PINNED_CONTRACT_SHA256,
        "relay-job.v1.json is not the contract web pins"
    );
    let pins: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("VENDORED.json")).unwrap()).unwrap();
    assert_eq!(pins["source"], "onryde/seazn.club");
    let commit = pins["commit"].as_str().unwrap();
    assert!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "VENDORED.json must name a full commit sha, got {commit:?}"
    );
}

// ---- the wire words ----

/// Owed 24 (amended 2026-10-18): the contract's `rung` is exactly `full|overlay_half_rate`, and
/// every `Rung` writes one of them.
#[test]
fn the_rung_enum_is_exactly_full_and_overlay_half_rate() {
    assert_eq!(declared_words("rung"), ["full", "overlay_half_rate"]);
    let v = schema_def("heartbeat");
    let mut checked = 0;
    for r in all_rungs() {
        let wire = heartbeat_wire(&HealthReport { rung: r, ..report() }, &Measured::default());
        assert!(violations(&v, &wire).is_empty(), "{r:?}: {wire}");
        checked += 1;
    }
    eprintln!("rungs checked: {checked}");
    assert_eq!(checked, 2);
}

/// Each enum key's wire words, over every variant, are EXACTLY the contract's words for that key:
/// none outside it (web answers 400), and none of its words unreachable (a dropped mapping).
#[test]
fn every_wire_word_is_one_the_contract_declares_and_all_are_used() {
    let v = schema_def("heartbeat");
    let mut reports: Vec<(&str, HealthReport)> = Vec::new();
    for s in all_states() {
        let r = if s == BeatState::Stopped {
            stopped(ExitReason::Ended, None)
        } else {
            HealthReport { state: s, ..report() }
        };
        reports.push(("state", r));
    }
    for p in all_phases() {
        reports.push((
            "programmePhase",
            HealthReport {
                programme_phase: p,
                ..report()
            },
        ));
    }
    for l in all_links() {
        reports.push(("input", HealthReport { input: l, ..report() }));
        reports.push((
            "output",
            HealthReport {
                output: l,
                ..report()
            },
        ));
    }
    for g in all_graphics() {
        reports.push((
            "graphics",
            HealthReport {
                graphics: g,
                ..report()
            },
        ));
    }
    for r in all_rungs() {
        reports.push(("rung", HealthReport { rung: r, ..report() }));
    }
    for c in all_channels() {
        reports.push((
            "phaseChannel",
            HealthReport {
                phase_channel: Some(c),
                ..report()
            },
        ));
    }
    for r in ExitReason::ALL {
        reports.push(("exitReason", stopped(r, None)));
    }
    let mut seen: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (key, r) in &reports {
        let wire = heartbeat_wire(r, &Measured::default());
        let errs = violations(&v, &wire);
        assert!(errs.is_empty(), "{key}: {wire} violates the contract: {errs:?}");
        let word = wire[*key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} absent in {wire}"));
        let declared = declared_words(key);
        assert!(
            declared.iter().any(|d| d == word),
            "{key}: {word:?} is not in {declared:?}"
        );
        let entry = seen.entry(key).or_default();
        if !entry.iter().any(|w| w == word) {
            entry.push(word.to_owned());
        }
    }
    for (key, mut words) in seen.clone() {
        let mut declared = declared_words(key);
        words.sort();
        declared.sort();
        assert_eq!(words, declared, "{key}: some declared word is never written");
    }
    eprintln!(
        "wire words checked: {} reports over {} keys",
        reports.len(),
        seen.len()
    );
    assert_eq!(seen.len(), 8, "anti-vacuity: every enum key");
}

/// C12's endpoint reuses this mapping: it must agree with the beat, word for word, and with the
/// contract's `phaseReply.phase` words.
#[test]
fn programme_phase_wire_is_the_beats_word() {
    let reply_words: Vec<String> = contract()["$defs"]["phaseReply"]["properties"]["phase"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_owned())
        .collect();
    let mut checked = 0;
    for p in all_phases() {
        let wire = heartbeat_wire(
            &HealthReport {
                programme_phase: p,
                ..report()
            },
            &Measured::default(),
        );
        assert_eq!(wire["programmePhase"], programme_phase_wire(p), "{p:?}");
        assert!(
            reply_words.iter().any(|w| w == programme_phase_wire(p)),
            "{p:?} not a phaseReply word"
        );
        checked += 1;
    }
    eprintln!("phases checked: {checked}");
    assert_eq!(checked, reply_words.len(), "every phaseReply word, once");
}

// ---- absent is not zero ----

#[test]
fn unmeasured_values_are_absent_not_zero() {
    let t = Thresholds::default();
    let s = Supervisor::new(t).decide(Event::Tick, 0).next;
    let wire = heartbeat_wire(&s.health(0), &Measured::default());
    for k in [
        "fps",
        "bitrateKbps",
        "egressBytes",
        "measuredLatencyMs",
        "encoderSpeed",
        "cpuPct",
        "memMb",
        "scoreStale",
        "phaseChannel",
        "exitReason",
        "exitDetail",
    ] {
        assert!(
            wire.get(k).is_none(),
            "{k} must be omitted when unmeasured, got {:?}",
            wire.get(k)
        );
    }
}

/// A present 0 is a measured zero (contract README, "Absent is not zero"): it is sent, as 0.
#[test]
fn a_measured_zero_is_sent_as_zero() {
    let m = Measured {
        fps: Some(0.0),
        bitrate_kbps: Some(0),
        egress_bytes: Some(0),
        measured_latency_ms: Some(0),
        encoder_speed: Some(0.0),
        cpu_pct: Some(0.0),
        mem_mb: Some(0),
        // Measured, but not a contract key (Owed 90): `camera_frames_replaced_stays_off_the_wire…`.
        camera_frames_replaced: Some(0),
    };
    let wire = heartbeat_wire(&report(), &m);
    let mut checked = 0;
    for k in [
        "fps",
        "bitrateKbps",
        "egressBytes",
        "measuredLatencyMs",
        "encoderSpeed",
        "cpuPct",
        "memMb",
    ] {
        assert_eq!(wire[k].as_f64(), Some(0.0), "{k}: {wire}");
        checked += 1;
    }
    // The domain's own counters are always present, 0 included.
    for k in ["inputKicks", "pipelineRebuilds", "sinkSwaps", "droppedFrames"] {
        assert_eq!(wire[k], 0, "{k}: {wire}");
        checked += 1;
    }
    assert_eq!(wire["inputKickAlert"], false);
    assert!(violations(&schema_def("heartbeat"), &wire).is_empty(), "{wire}");
    eprintln!("measured zeros checked: {checked}");
    assert_eq!(checked, 11);
}

/// Owed 81 (i), Owed 90: `camera_frames_replaced` is measured, but the vendored `heartbeat` declares
/// no key for it and refuses an unknown one (`additionalProperties: false`; web's `RelayHeartbeat` is
/// `.strict()`, so the beat would be a 400 and lost). The wire therefore never carries it, 0 and the
/// largest count included, and the beat is the same as without it. When a re-vendor brings the key
/// this test reds, and the field is wired then.
#[test]
fn camera_frames_replaced_stays_off_the_wire_until_the_contract_declares_it() {
    let hb = &contract()["$defs"]["heartbeat"];
    assert_eq!(
        hb["additionalProperties"], false,
        "the contract refuses an unknown key"
    );
    let declared = hb["properties"].as_object().expect("the heartbeat's properties");
    assert!(
        declared.get("cameraFramesReplaced").is_none(),
        "the contract declares cameraFramesReplaced now: write it on the wire (Owed 90)"
    );
    let v = schema_def("heartbeat");
    let without = heartbeat_wire(
        &report(),
        &Measured {
            camera_frames_replaced: None,
            ..full_measured()
        },
    );
    let mut checked = 0;
    for n in [0, 57, u64::MAX] {
        let m = Measured {
            camera_frames_replaced: Some(n),
            ..full_measured()
        };
        let wire = heartbeat_wire(&report(), &m);
        let keys = wire.as_object().expect("an object");
        assert!(
            keys.keys().all(|k| declared.contains_key(k)),
            "{n}: a key the contract does not declare: {wire}"
        );
        assert_eq!(wire, without, "{n}: the count changes nothing on the wire");
        assert!(violations(&v, &wire).is_empty(), "{wire}");
        checked += 1;
    }
    eprintln!("camera_frames_replaced counts kept off the wire: {checked}");
    assert_eq!(checked, 3);
}

/// "Never inserts a key whose value is unknown": a NaN or an infinity is not a measurement (JSON has
/// no such number, and serde writes `null`), and a negative one is outside the contract (`minimum:
/// 0`, a 400 that would lose the beat). All are omitted, and the beat still validates.
#[test]
fn a_non_finite_or_negative_measurement_is_omitted() {
    let v = schema_def("heartbeat");
    let mut checked = 0;
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.5] {
        let m = Measured {
            fps: Some(bad),
            encoder_speed: Some(bad),
            cpu_pct: Some(bad),
            ..full_measured()
        };
        let wire = heartbeat_wire(&report(), &m);
        for k in ["fps", "encoderSpeed", "cpuPct"] {
            assert!(wire.get(k).is_none(), "{k} = {bad} must be omitted, got {wire}");
            checked += 1;
        }
        assert_eq!(wire["memMb"], 812, "the valid ones stay");
        assert!(violations(&v, &wire).is_empty(), "{wire}");
    }
    eprintln!("bad measurements checked: {checked}");
    assert_eq!(checked, 15);
}

/// Owed 86: the beat's `fps` is the real output rate, a measured f64, and is sent to one decimal (the
/// coordinator's decision, 2026-10-24, on the PR #69 review's I2: R1's chip printed the raw value,
/// "29.998128116638 fps"). Each case's wire value is that rule worked by hand; the other measured
/// floats are untouched, a negative rate stays omitted, and each beat validates.
#[test]
fn the_beats_fps_is_sent_to_one_decimal() {
    let v = schema_def("heartbeat");
    let mut checked = 0;
    for (fps, want, text) in [
        (29.998128116638, 30.0, "30.0"),
        (30.0, 30.0, "30.0"),
        (11.46, 11.5, "11.5"),
        (11.5, 11.5, "11.5"),
        (24.66, 24.7, "24.7"),
        (28.449, 28.4, "28.4"),
        (59.774, 59.8, "59.8"),
        (4.591, 4.6, "4.6"),
        (0.04, 0.0, "0.0"),
        (0.0, 0.0, "0.0"),
    ] {
        let m = Measured {
            fps: Some(fps),
            encoder_speed: Some(0.987_654),
            cpu_pct: Some(41.234_5),
            ..full_measured()
        };
        let wire = heartbeat_wire(&report(), &m);
        assert_eq!(wire["fps"].as_f64(), Some(want), "{fps}: {wire}");
        assert_eq!(wire["fps"].to_string(), text, "{fps}: {wire}");
        assert_eq!(wire["encoderSpeed"].as_f64(), Some(0.987_654), "{wire}");
        assert_eq!(wire["cpuPct"].as_f64(), Some(41.234_5), "{wire}");
        assert!(violations(&v, &wire).is_empty(), "{wire}");
        checked += 1;
    }
    let m = Measured {
        fps: Some(-0.04),
        ..full_measured()
    };
    assert!(
        heartbeat_wire(&report(), &m).get("fps").is_none(),
        "a negative rate is omitted"
    );
    checked += 1;
    eprintln!("fps values checked on the wire: {checked}");
    assert_eq!(checked, 11);
}

/// Every integer the contract declares has `maximum` 2^53 − 1 (a JSON number web reads exactly). A
/// counter past it is written at it, never above (a 400 would lose every later beat).
#[test]
fn integers_never_exceed_the_contracts_maximum() {
    let v = schema_def("heartbeat");
    let props = &contract()["$defs"]["heartbeat"]["properties"];
    let max = props["egressBytes"]["maximum"]
        .as_u64()
        .expect("the contract's integer maximum");
    let big = HealthReport {
        pipeline_rebuilds: u64::MAX,
        sink_swaps: u64::MAX,
        dropped_frames: u64::MAX,
        input_kicks: u32::MAX,
        ..report()
    };
    let m = Measured {
        egress_bytes: Some(u64::MAX),
        measured_latency_ms: Some(u64::MAX),
        mem_mb: Some(u64::MAX),
        ..Measured::default()
    };
    let wire = heartbeat_wire(&big, &m);
    let errs = violations(&v, &wire);
    assert!(errs.is_empty(), "{wire}: {errs:?}");
    let mut checked = 0;
    for k in [
        "pipelineRebuilds",
        "sinkSwaps",
        "droppedFrames",
        "egressBytes",
        "measuredLatencyMs",
    ] {
        assert_eq!(wire[k].as_u64(), Some(max), "{k}: {wire}");
        checked += 1;
    }
    assert_eq!(
        wire["inputKicks"].as_u64(),
        Some(u64::from(u32::MAX)),
        "a u32 fits as it is"
    );
    eprintln!("integers checked at the maximum: {checked}");
    assert_eq!(checked, 5);
}

// ---- the final beat's two keys ----

#[test]
fn non_final_beats_have_no_exit_keys() {
    let v = schema_def("heartbeat");
    let mut checked = 0;
    for s in all_states().into_iter().filter(|s| *s != BeatState::Stopped) {
        // With no exit, and with one the domain never pairs with this state: the contract's if/then
        // refuses exit keys off a `stopped` beat (a 400), so the state decides, not the field.
        let stray = ExitInfo {
            reason: ExitReason::Fatal,
            detail: Some("x".into()),
        };
        for exit in [None, Some(stray)] {
            let wire = heartbeat_wire(
                &HealthReport {
                    state: s,
                    exit,
                    ..report()
                },
                &full_measured(),
            );
            assert!(wire.get("exitReason").is_none(), "{s:?}: {wire}");
            assert!(wire.get("exitDetail").is_none(), "{s:?}: {wire}");
            assert!(violations(&v, &wire).is_empty(), "{wire}");
            checked += 1;
        }
    }
    eprintln!("non-final beats checked: {checked}");
    assert_eq!(checked, 6);
}

/// Owed 27 (option a): an empty detail is absent, never "" (and so is a `None`).
#[test]
fn an_empty_detail_is_absent() {
    let v = schema_def("heartbeat");
    for detail in [Some(""), None] {
        let wire = heartbeat_wire(&stopped(ExitReason::Fatal, detail), &Measured::default());
        assert_eq!(wire["exitReason"], "fatal", "{detail:?}: {wire}");
        assert!(wire.get("exitDetail").is_none(), "{detail:?}: {wire}");
        assert!(violations(&v, &wire).is_empty(), "{wire}");
    }
    // And the domain's own beat: Event::Fatal { detail: Some("") } from a real supervisor.
    let s = booted(0, u64::MAX / 2);
    let d = s.decide(
        Event::Fatal {
            detail: Some(ExitDetail::new("")),
        },
        10,
    );
    let beats = beats_of(&d.effects);
    assert_eq!(beats.len(), 1, "the fatal exit's one beat");
    let wire = heartbeat_wire(beats[0], &Measured::default());
    assert_eq!(wire["exitReason"], "fatal");
    assert!(wire.get("exitDetail").is_none(), "{wire}");
}

/// A key in a URL near the front is redacted (without `redact` it would sit inside the first 100
/// characters), and a 500-character text, half emoji, is cut at 100 Unicode scalar values: exactly the
/// first 100 of the input. Both come from a real supervisor's fatal beat.
#[test]
fn exit_detail_is_redacted_and_capped() {
    let v = schema_def("heartbeat");
    let filler: String = (0..250).map(|i| if i % 2 == 0 { "😀x" } else { "é🎥" }).collect();
    assert_eq!(filler.chars().count(), 500, "test setup: 500 chars, half emoji");
    let key = "PLANTED-INPUT-B-KEY-1f2e";
    let token = "PLANTED-PAGE-TOKEN-9a8b";
    let pass = "PLANTED-SRT-PASS-3c4d";
    let cases = [
        (
            format!("push failed: rtmps://live.cloudflare.com:443/live2/{key} {filler}"),
            None,
        ),
        (
            format!("page https://seazn.club/overlay/fixtures/f?sid=s&st={token}&delay=3100 {filler}"),
            None,
        ),
        (format!("srtsrc: passphrase={pass} {filler}"), None),
        (
            format!("{filler} rtmps://live.cloudflare.com:443/live/{key}"),
            Some(100),
        ),
    ];
    let mut checked = 0;
    for (detail, exact) in cases {
        let s = booted(0, u64::MAX / 2);
        let d = s.decide(
            Event::Fatal {
                detail: Some(ExitDetail::new(detail.clone())),
            },
            10,
        );
        let beats = beats_of(&d.effects);
        assert_eq!(beats.len(), 1);
        assert_eq!(
            beats[0]
                .exit
                .as_ref()
                .unwrap()
                .detail
                .as_ref()
                .map(ExitDetail::as_str),
            Some(detail.as_str()),
            "domain: verbatim"
        );
        let wire = heartbeat_wire(beats[0], &Measured::default());
        let got = wire["exitDetail"].as_str().expect("a non-empty detail is sent");
        for secret in [key, token, pass] {
            assert!(!got.contains(secret), "{secret} leaked: {got}");
        }
        assert!(got.chars().count() <= 100, "{} chars", got.chars().count());
        assert!(got.encode_utf16().count() <= 200, "web's zod bound");
        if let Some(n) = exact {
            assert_eq!(
                got.chars().count(),
                n,
                "cut at scalars, not bytes or UTF-16 units"
            );
            assert_eq!(
                got,
                detail.chars().take(n).collect::<String>(),
                "the first 100 scalars"
            );
        }
        assert!(violations(&v, &wire).is_empty(), "{wire}");
        checked += 1;
    }
    eprintln!("exit details checked: {checked}");
    assert_eq!(checked, 4);
}

// ---- the session ----

#[test]
fn composed_fixtures_decode_and_passthrough_is_refused() {
    let mut checked = 0;
    for (name, profile) in [
        ("session-composed-720p.json", OutputProfile::P720),
        ("session-composed-1080p.json", OutputProfile::P1080),
    ] {
        let f = fixture(name);
        let cfg = parse_session(&f).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let want = &f["output"];
        assert_eq!(u64::from(cfg.output.width), want["width"].as_u64().unwrap());
        assert_eq!(cfg.output, profile);
        assert_eq!(cfg.overlay_url, f["overlayUrl"].as_str().unwrap());
        assert_eq!(cfg.session_id, f["sessionId"].as_str().unwrap());
        assert_eq!(cfg.fixture_id.as_deref(), f["fixtureId"].as_str());
        assert_eq!(cfg.input.url, f["input"]["url"].as_str().unwrap());
        assert_eq!(cfg.input.stream_id, f["input"]["streamId"].as_str().unwrap());
        assert_eq!(cfg.input.passphrase, f["input"]["passphrase"].as_str().unwrap());
        assert_eq!(cfg.target.url, f["target"]["url"].as_str().unwrap());
        assert_eq!(cfg.target.stream_key, f["target"]["streamKey"].as_str().unwrap());
        assert_eq!(
            cfg.heartbeat_interval_ms,
            f["heartbeatIntervalMs"].as_u64().unwrap()
        );
        assert_eq!(cfg.overlay_delay_ms, f["overlayDelayMs"].as_u64().unwrap());
        checked += 1;
    }
    assert_eq!(
        parse_session(&fixture("session-passthrough.json")),
        Err(FetchError::Refused(Refusal::WrongMode))
    );
    eprintln!("composed fixtures decoded: {checked}");
    assert_eq!(checked, 2);
}

/// The fixture names are the contract's verdicts (fixtures README, "Names"): a plain `session-` vector
/// decodes, and every `refused-` and `invalid-` one is refused, as is the passthrough.
#[test]
fn every_session_fixture_gets_the_verdict_its_name_declares() {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in std::fs::read_dir(fixtures_dir()).unwrap() {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("session-") else {
            continue;
        };
        let got = parse_session(&fixture(&name));
        let verdict =
            if rest.starts_with("refused-") || rest.starts_with("invalid-") || rest == "passthrough.json" {
                assert_eq!(got, Err(FetchError::Refused(Refusal::WrongMode)), "{name}");
                "refused"
            } else {
                assert!(got.is_ok(), "{name}: {got:?}");
                "decoded"
            };
        *counts.entry(verdict).or_default() += 1;
    }
    eprintln!("session fixtures checked: {counts:?}");
    assert!(
        counts.get("decoded").copied().unwrap_or(0) >= 2,
        "anti-vacuity: {counts:?}"
    );
    assert!(
        counts.get("refused").copied().unwrap_or(0) >= 4,
        "anti-vacuity: {counts:?}"
    );
}

/// The reply vectors' verdicts (Owed 108 fix (b)): the contract admits every plain `reply-` vector
/// and refuses every `reply-invalid-` one, and the parser reads each one's `desiredState`, with the
/// `phoneLiveEdge` the vector carries when the contract admits it, and none when it refuses it (a bad
/// counter never costs the beat).
#[test]
fn every_reply_fixture_gets_the_verdict_its_name_declares() {
    let v = schema_def("heartbeatReply");
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in std::fs::read_dir(fixtures_dir()).unwrap() {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("reply-") else {
            continue;
        };
        let data = fixture(&name);
        let got = parse_reply(&data).unwrap_or_else(|| panic!("{name}: not read"));
        assert_eq!(
            Some(match got.desired {
                Desired::Live => "live",
                Desired::Ending => "ending",
            }),
            data["desiredState"].as_str(),
            "{name}"
        );
        let verdict = if rest.starts_with("invalid-") {
            assert!(
                !violations(&v, &data).is_empty(),
                "{name}: the contract admits it"
            );
            assert_eq!(got.phone_live_edge, None, "{name}");
            "refused"
        } else {
            assert_eq!(violations(&v, &data), Vec::<String>::new(), "{name}");
            assert_eq!(got.phone_live_edge, data["phoneLiveEdge"].as_u64(), "{name}");
            if got.phone_live_edge.is_some() {
                *counts.entry("with a counter").or_default() += 1;
            }
            "admitted"
        };
        *counts.entry(verdict).or_default() += 1;
    }
    eprintln!("reply fixtures checked: {counts:?}");
    for (verdict, floor) in [("admitted", 5), ("refused", 2), ("with a counter", 2)] {
        assert!(
            counts.get(verdict).copied().unwrap_or(0) >= floor,
            "anti-vacuity: {verdict}: {counts:?}"
        );
    }
}

/// The counter's bounds are the contract's (`heartbeatReply.phoneLiveEdge`'s integer branch): its
/// `minimum` and `maximum` read as themselves, and one past either reads as no counter, as does a
/// number past the maximum written with a fraction of zero.
#[test]
fn the_phone_live_counter_is_read_within_the_contracts_bounds() {
    let branches = contract()["$defs"]["heartbeatReply"]["properties"]["phoneLiveEdge"]["anyOf"].clone();
    let int = branches
        .as_array()
        .and_then(|a| a.iter().find(|b| b["type"] == "integer"))
        .expect("an integer branch")
        .clone();
    let (min, max) = (int["minimum"].as_u64().unwrap(), int["maximum"].as_u64().unwrap());
    let v = schema_def("heartbeatReply");
    let mut checked = 0;
    for (edge, want) in [
        (json!(min), Some(min)),
        (json!(max), Some(max)),
        (json!(max + 1), None),
        (json!(max as f64 * 2.0), None),
        (json!(min as i64 - 1), None),
    ] {
        let data = json!({ "desiredState": "live", "phoneLiveEdge": edge });
        assert_eq!(violations(&v, &data).is_empty(), want.is_some(), "{data}");
        let got = parse_reply(&data).expect("the reply is read");
        assert_eq!(got.phone_live_edge, want, "{data}");
        checked += 1;
    }
    eprintln!("bounds checked: {checked}");
    assert_eq!(checked, 5);
}

#[test]
fn overlay_delay_must_agree_with_the_overlay_url() {
    let mut odd = fixture("session-composed-720p.json");
    let delay = odd["overlayDelayMs"].as_u64().unwrap();
    odd["overlayDelayMs"] = json!(delay + 1);
    assert_eq!(
        parse_session(&odd),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "two disagreeing delays: refuse, never pick one"
    );
    let mut bare = fixture("session-composed-720p.json");
    let url = bare["overlayUrl"].as_str().unwrap().to_owned();
    let stripped = url
        .split('&')
        .filter(|kv| !kv.starts_with("delay=") && !kv.contains("?delay="))
        .collect::<Vec<_>>()
        .join("&");
    assert!(
        !stripped.contains("delay="),
        "test setup: delay= must be gone from {stripped}"
    );
    bare["overlayUrl"] = json!(stripped);
    assert_eq!(
        parse_session(&bare),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "an absent delay= is refused, never read as 0"
    );
    // The contract's prose: `delay=` appears exactly once.
    let mut twice = fixture("session-composed-720p.json");
    twice["overlayUrl"] = json!(format!("{url}&delay={delay}"));
    assert_eq!(
        parse_session(&twice),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "delay= twice"
    );
    let mut no_ms = fixture("session-composed-720p.json");
    no_ms.as_object_mut().unwrap().remove("overlayDelayMs");
    assert_eq!(
        parse_session(&no_ms),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "no overlayDelayMs"
    );
    // A delay of 0 is a real value (the contract: "0 is a real value, never unset").
    let mut zero = fixture("session-composed-720p.json");
    zero["overlayDelayMs"] = json!(0);
    zero["overlayUrl"] = json!(url.replace(&format!("delay={delay}"), "delay=0"));
    assert_eq!(parse_session(&zero).map(|c| c.overlay_delay_ms), Ok(0));
}

#[test]
fn output_must_be_a_declared_profile() {
    assert_eq!(
        parse_session(&fixture("session-composed-720p.json"))
            .unwrap()
            .output,
        OutputProfile::P720
    );
    assert_eq!(
        parse_session(&fixture("session-composed-1080p.json"))
            .unwrap()
            .output,
        OutputProfile::P1080
    );
    let mut checked = 0;
    for (field, value) in [
        ("width", 1000),
        ("height", 1000),
        ("fps", 60),
        ("videoKbps", 4001),
    ] {
        let mut odd = fixture("session-composed-720p.json");
        odd["output"][field] = json!(value);
        assert_eq!(
            parse_session(&odd),
            Err(FetchError::Refused(Refusal::WrongMode)),
            "{field} = {value}: an undeclared profile is refused, never improvised"
        );
        checked += 1;
    }
    eprintln!("odd profiles checked: {checked}");
    assert_eq!(checked, 4);
}

/// The four composed-only keys (contract `session` description): any one absent is refused, as is a
/// 0 heartbeat interval (`exclusiveMinimum: 0`), an input that is not SRT, and a session that is not
/// an object at all.
#[test]
fn a_composed_session_missing_a_composed_key_is_refused() {
    let mut checked = 0;
    for key in [
        "input",
        "output",
        "overlayUrl",
        "heartbeatIntervalMs",
        "target",
        "sessionId",
        "mode",
    ] {
        let mut odd = fixture("session-composed-720p.json");
        odd.as_object_mut().unwrap().remove(key);
        assert_eq!(
            parse_session(&odd),
            Err(FetchError::Refused(Refusal::WrongMode)),
            "{key} absent"
        );
        checked += 1;
    }
    let mut zero = fixture("session-composed-720p.json");
    zero["heartbeatIntervalMs"] = json!(0);
    assert_eq!(
        parse_session(&zero),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "a 0 interval"
    );
    let mut rtmp = fixture("session-composed-720p.json");
    rtmp["input"]["protocol"] = json!("rtmp");
    assert_eq!(
        parse_session(&rtmp),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "not srt"
    );
    // The contract's pattern `^srt://[^\s/?#@]+:\d+$`: a path, a query, userinfo, a space, no port.
    for url in [
        "srt://live.cloudflare.com:778/x",
        "srt://live.cloudflare.com:778?streamid=x",
        "srt://user@live.cloudflare.com:778",
        "srt://live.cloudflare.com/x:778",
        "srt://live cloudflare.com:778",
        "srt://live.cloudflare.com#x:778",
        "srt://live.cloudflare.com",
        "srt://:778",
        "srt://live.cloudflare.com:",
        "rtmps://live.cloudflare.com:778",
    ] {
        let mut odd = fixture("session-composed-720p.json");
        odd["input"]["url"] = json!(url);
        assert_eq!(
            parse_session(&odd),
            Err(FetchError::Refused(Refusal::WrongMode)),
            "{url}"
        );
        checked += 1;
    }
    // A composed body whose mode says otherwise: every composed key is there, and still refused.
    let mut passthrough = fixture("session-composed-720p.json");
    passthrough["mode"] = json!("passthrough");
    assert_eq!(
        parse_session(&passthrough),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "mode passthrough with every composed key"
    );
    for odd in [json!(null), json!([]), json!("composed"), json!({})] {
        assert_eq!(
            parse_session(&odd),
            Err(FetchError::Refused(Refusal::WrongMode)),
            "{odd}"
        );
        checked += 1;
    }
    eprintln!("refused shapes checked: {}", checked + 3);
    assert_eq!(checked, 21);
}

/// F-006: `fixtureId` is nullable (the session outlives its fixture), and a null decodes as `None`.
#[test]
fn a_null_fixture_id_decodes_as_none() {
    let mut f = fixture("session-composed-720p.json");
    f["fixtureId"] = Value::Null;
    assert_eq!(parse_session(&f).map(|c| c.fixture_id), Ok(None));
    let mut n = fixture("session-composed-720p.json");
    n["fixtureId"] = json!(7);
    assert_eq!(
        parse_session(&n),
        Err(FetchError::Refused(Refusal::WrongMode)),
        "neither a string nor null"
    );
}

/// AGENTS.md §7: a decoded session's `Debug` carries none of its four credentials.
#[test]
fn a_decoded_session_never_prints_its_credentials() {
    let mut checked = 0;
    for name in ["session-composed-720p.json", "session-composed-1080p.json"] {
        let f = fixture(name);
        let dbg = format!("{:?}", parse_session(&f).unwrap());
        for secret in [
            f["target"]["streamKey"].as_str().unwrap(),
            f["input"]["streamId"].as_str().unwrap(),
            f["input"]["passphrase"].as_str().unwrap(),
            f["pageToken"].as_str().unwrap(),
        ] {
            assert!(!dbg.contains(secret), "{name}: {secret} in {dbg}");
            checked += 1;
        }
        assert!(
            dbg.contains(f["sessionId"].as_str().unwrap()),
            "the session id is not a secret: {dbg}"
        );
    }
    eprintln!("credentials checked: {checked}");
    assert_eq!(checked, 8);
}

// ---- every beat a real supervisor emits ----

/// One supervisor and a fake world around it, on C08's 33 ms tick: it answers every effect the way the
/// adapters will (a rebuilt input connects, a new sink sends bytes, a restarted page gets ready and
/// beats, a drain finishes) and records every beat.
struct World {
    s: Supervisor,
    now: u64,
    camera: bool,
    pending: Vec<Event>,
    sink_gen: u64,
    sink_total: u64,
    page_gen: u64,
    last_progress: u64,
    last_alive: u64,
    progress_sent: usize,
    swaps: Vec<u64>,
    beats: Vec<HealthReport>,
    exit: Option<i32>,
}

const TICK: u64 = 33;
/// One second of 720p video at its declared bitrate, in bytes: the growth of the sink's total per
/// progress report (Owed 13: a growing `out-bytes-total`).
fn bytes_per_second() -> u64 {
    u64::from(OutputProfile::P720.video_kbps) * 1_000 / 8
}

impl World {
    fn new() -> Self {
        Self {
            s: Supervisor::new(Thresholds::default()),
            now: 0,
            camera: true,
            pending: Vec::new(),
            sink_gen: 0,
            sink_total: 0,
            page_gen: 0,
            last_progress: 0,
            last_alive: 0,
            progress_sent: 0,
            swaps: Vec::new(),
            beats: Vec::new(),
            exit: None,
        }
    }

    fn decide(&mut self, ev: Event) {
        if self.exit.is_some() {
            return;
        }
        let d = self.s.decide(ev, self.now);
        for e in d.effects {
            match e {
                Effect::RebuildInput { gen } => self.pending.push(Event::InputConnected { gen }),
                Effect::SwapSink { gen, .. } => {
                    self.swaps.push(gen);
                    self.sink_gen = gen;
                    self.sink_total = 0;
                }
                Effect::RestartGraphics { gen }
                | Effect::ReloadGraphics { gen, .. }
                | Effect::ReloadGraphicsNoFrames { gen, .. } => {
                    self.page_gen = gen;
                    // Ready, its latest frame written at the ready and reported (C11, Owed 99).
                    self.pending.push(Event::CefReady { gen });
                    self.pending.push(Event::CefPainted { gen });
                }
                Effect::DrainOutput => self.pending.push(Event::OutputDrained),
                Effect::SendHeartbeat(h) => self.beats.push(h),
                Effect::Exit(c) => self.exit = Some(c),
                // The input connects at once here, so no generation is given up (C10b).
                Effect::AbandonInput { .. }
                | Effect::FetchSession
                | Effect::StartProgramme
                | Effect::HideOverlay { .. }
                | Effect::StepDown(_) => {}
            }
        }
        self.s = d.next;
    }

    fn boot(&mut self, heartbeat_interval_ms: u64, deadline_at: u64) {
        self.decide(Event::Tick);
        self.decide(Event::BootstrapOk {
            heartbeat_interval_ms,
            deadline_at,
        });
    }

    /// Advance to `until` on the tick, feeding frames while the camera is on, a growing byte total
    /// and a page beat every second, and every queued reply.
    fn run_until(&mut self, until: u64) {
        while self.now < until && self.exit.is_none() {
            self.now += TICK;
            for ev in std::mem::take(&mut self.pending) {
                self.decide(ev);
            }
            if self.camera {
                self.decide(Event::InputFrame {
                    gen: self.s.input_generation(),
                });
            }
            if self.sink_gen > 0 && self.now - self.last_progress >= 1_000 {
                self.sink_total += bytes_per_second();
                self.last_progress = self.now;
                self.progress_sent += 1;
                self.decide(Event::OutputProgress {
                    gen: self.sink_gen,
                    total_bytes: self.sink_total,
                    encoded: Encoded::default(),
                });
            }
            if self.page_gen > 0 && self.now - self.last_alive >= 1_000 {
                self.last_alive = self.now;
                self.decide(Event::CefAlive { gen: self.page_gen });
            }
            self.decide(Event::Tick);
        }
    }
}

fn booted(now: u64, deadline_at: u64) -> Supervisor {
    let s = Supervisor::new(Thresholds::default()).decide(Event::Tick, 0).next;
    s.decide(
        Event::BootstrapOk {
            heartbeat_interval_ms: Thresholds::default().heartbeat_interval_ms,
            deadline_at,
        },
        now,
    )
    .next
}

fn beats_of(fx: &[Effect]) -> Vec<&HealthReport> {
    fx.iter()
        .filter_map(|e| {
            if let Effect::SendHeartbeat(h) = e {
                Some(h)
            } else {
                None
            }
        })
        .collect()
}

/// The fatal text the sweep's fatal sessions carry: a key in a URL and 500 characters (Owed 27).
fn fatal_detail() -> String {
    let filler: String = (0..250).map(|_| "😀é").collect();
    format!("programme start failed: rtmps://live.cloudflare.com:443/live/PLANTED-SWEEP-KEY {filler}")
}

/// Every session the sweep runs: each ends a different way, so all six exit reasons, every phase word
/// and both phase channels appear on real beats.
fn sessions() -> Vec<(&'static str, World)> {
    let far = u64::MAX / 2;
    let mut out = Vec::new();

    // The brief's session: boot, live on a growing byte total, then web's `ending` reply: the final
    // hold (`final`, with the page on the loopback channel), the drain and its beat.
    let mut w = World::new();
    w.boot(5_000, far);
    w.run_until(13_200);
    w.decide(Event::PhaseChannel(PhaseChannel::Loopback));
    w.decide(Event::HeartbeatReply(Desired::Ending));
    w.run_until(13_200 + Thresholds::default().final_hold_ms + 2_000);
    out.push(("ended", w));

    // Camera loss: frozen, then reconnecting, then live again; the page reports the web channel; the
    // deadline then holds and drains. A short beat interval puts every phase on a beat.
    let mut w = World::new();
    w.boot(200, 30_000);
    w.run_until(3_000);
    w.decide(Event::PhaseChannel(PhaseChannel::Web));
    w.camera = false;
    w.run_until(8_000);
    w.camera = true;
    w.run_until(30_000 + Thresholds::default().final_hold_ms + 2_000);
    out.push(("deadline", w));

    // SIGINT holds, then drains; SIGTERM drains at once.
    for (name, sig) in [("sigint", SignalKind::Int), ("sigterm", SignalKind::Term)] {
        let mut w = World::new();
        w.boot(1_000, far);
        w.run_until(4_000);
        w.decide(Event::Signal(sig));
        w.run_until(4_000 + Thresholds::default().final_hold_ms + 2_000);
        out.push((name, w));
    }

    // Refusals at bootstrap: 410 exits session_gone, the others refused.
    for (name, r) in [
        ("gone", Refusal::Gone),
        ("unauthorized", Refusal::Unauthorized),
        ("not-found", Refusal::NotFound),
        ("wrong-mode", Refusal::WrongMode),
    ] {
        let mut w = World::new();
        w.decide(Event::Tick);
        w.decide(Event::BootstrapRefused(r));
        out.push((name, w));
    }

    // A fatal error while live, and one during a drain (a second Stopped beat).
    let mut w = World::new();
    w.boot(1_000, far);
    w.run_until(2_000);
    w.decide(Event::Fatal {
        detail: Some(ExitDetail::new(fatal_detail())),
    });
    out.push(("fatal", w));
    let mut w = World::new();
    w.boot(1_000, far);
    w.run_until(2_000);
    w.decide(Event::Signal(SignalKind::Term));
    w.pending.clear(); // the drain does not finish: the fatal error lands inside it
    w.decide(Event::Fatal {
        detail: Some(ExitDetail::new(fatal_detail())),
    });
    out.push(("fatal-in-drain", w));

    // C11b: CEF's health asks for an in-place restart while live.
    let mut w = World::new();
    w.boot(1_000, far);
    w.run_until(2_000);
    w.decide(Event::CefHealth(CefHealth {
        unrecoverable: Some(1),
        ..CefHealth::default()
    }));
    out.push(("restart", w));
    out
}

/// How each sweep session must end: its final beat's `exitReason` and the process exit code. Every row
/// comes from the contract's own `exitReason` description: ended, deadline, signalled and
/// session_gone exit 0; refused exits 78; fatal exits 70; restart exits 75 (Owed 66, vendored at
/// dec7b95d9; before it the `restart` row went as `fatal` with 75, against the description).
const SESSION_ENDS: [(&str, &str, i32); 11] = [
    ("ended", "ended", 0),
    ("deadline", "deadline", 0),
    ("sigint", "signalled", 0),
    ("sigterm", "signalled", 0),
    ("gone", "session_gone", 0),
    ("unauthorized", "refused", 78),
    ("not-found", "refused", 78),
    ("wrong-mode", "refused", 78),
    ("fatal", "fatal", 70),
    ("fatal-in-drain", "fatal", 70),
    ("restart", "restart", 75),
];

#[test]
fn every_heartbeat_we_can_emit_validates_against_the_contract() {
    let v = schema_def("heartbeat");
    let mut beats = 0;
    let mut ended: Vec<&str> = Vec::new();
    let mut words: BTreeMap<String, usize> = BTreeMap::new();
    let mut exit_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut progress = 0;
    for (name, w) in sessions() {
        assert!(
            w.exit.is_some(),
            "{name}: the session must reach Exit (beats {})",
            w.beats.len()
        );
        // Each session ends the way the contract says its path does, on its last beat.
        let (_, reason, code) = SESSION_ENDS
            .iter()
            .find(|(n, _, _)| *n == name)
            .unwrap_or_else(|| panic!("{name}: no declared end"));
        let last = w.beats.last().unwrap_or_else(|| panic!("{name}: no beat"));
        assert_eq!(
            last.state,
            BeatState::Stopped,
            "{name}: the last beat is the final one"
        );
        assert_eq!(
            heartbeat_wire(last, &Measured::default())["exitReason"],
            *reason,
            "{name}"
        );
        assert_eq!(w.exit, Some(*code), "{name}: exit code");
        ended.push(name);
        // Owed 13: a growing total never stall-swaps the output; the first build (gen 1) is the only one.
        assert!(
            w.swaps.len() <= 1 && w.swaps.iter().all(|g| *g == 1),
            "{name}: SwapSink {:?}",
            w.swaps
        );
        progress += w.progress_sent;
        for h in &w.beats {
            for m in [Measured::default(), full_measured()] {
                let wire = heartbeat_wire(h, &m);
                let errs = violations(&v, &wire);
                assert!(errs.is_empty(), "{name}: beat {wire} violates contract: {errs:?}");
                beats += 1;
            }
            let wire = heartbeat_wire(h, &Measured::default());
            *words
                .entry(format!("phase:{}", wire["programmePhase"].as_str().unwrap()))
                .or_default() += 1;
            if let Some(c) = wire.get("phaseChannel") {
                *words
                    .entry(format!("channel:{}", c.as_str().unwrap()))
                    .or_default() += 1;
            }
            if h.state == BeatState::Stopped {
                *exit_reasons
                    .entry(wire["exitReason"].as_str().unwrap().to_owned())
                    .or_default() += 1;
                if h.exit.as_ref().and_then(|e| e.detail.as_ref()).is_some() {
                    let d = wire["exitDetail"]
                        .as_str()
                        .expect("the domain's fatal text is sent");
                    assert!(
                        !d.contains("PLANTED-SWEEP-KEY") && d.chars().count() <= 100,
                        "{d}"
                    );
                    *words.entry("exitDetail".into()).or_default() += 1;
                }
            } else {
                assert!(
                    wire.get("exitReason").is_none() && wire.get("exitDetail").is_none(),
                    "{wire}"
                );
                *words.entry("non-final".into()).or_default() += 1;
            }
        }
    }
    eprintln!("sessions ended as declared: {}", ended.len());
    assert_eq!(
        ended,
        SESSION_ENDS.map(|(n, _, _)| n),
        "every declared session runs, in order"
    );
    eprintln!("heartbeats validated: {beats}; progress events sent: {progress}");
    eprintln!("beat words: {words:?}");
    eprintln!("exit beats by reason: {exit_reasons:?}");
    assert!(beats > 0, "anti-vacuity");
    assert!(progress > 0, "anti-vacuity: no OutputProgress was sent (Owed 13)");
    for w in all_phases() {
        let key = format!("phase:{}", programme_phase_wire(w));
        assert!(
            words.get(&key).copied().unwrap_or(0) > 0,
            "no beat carried {key}: {words:?}"
        );
    }
    for key in ["channel:loopback", "channel:web", "exitDetail", "non-final"] {
        assert!(
            words.get(key).copied().unwrap_or(0) > 0,
            "no beat carried {key}: {words:?}"
        );
    }
    let final_and_loopback = sessions()
        .into_iter()
        .flat_map(|(_, w)| w.beats)
        .filter(|h| {
            h.programme_phase == ProgrammePhase::Final && h.phase_channel == Some(PhaseChannel::Loopback)
        })
        .count();
    eprintln!("beats carrying final on the loopback channel: {final_and_loopback}");
    assert!(
        final_and_loopback > 0,
        "Owed 4: no beat carried final with a phase channel"
    );
    let declared = declared_words("exitReason");
    for r in &declared {
        assert!(
            exit_reasons.get(r).copied().unwrap_or(0) > 0,
            "no exit beat carried {r}: {exit_reasons:?}"
        );
    }
    assert_eq!(declared.len(), 7, "the six exit reasons and C11b's restart");
}

/// Owed 66: the vendored `heartbeat-final-restart.json` is the final beat of an in-place restart, and
/// the beat the restart path sends (the sweep's `restart` session, a dead zygote of generation 1) is
/// that fixture in its exit keys: `stopped`, `restart` and the domain's own detail.
#[test]
fn the_restart_beat_is_the_vendored_restart_fixture() {
    let v = schema_def("heartbeat");
    let want = fixture("heartbeat-final-restart.json");
    assert!(violations(&v, &want).is_empty(), "the fixture validates");
    let (_, w) = sessions()
        .into_iter()
        .find(|(n, _)| *n == "restart")
        .expect("the sweep's restart session");
    let last = w.beats.last().expect("a final beat");
    let wire = heartbeat_wire(last, &Measured::default());
    let mut checked = 0;
    for key in ["state", "exitReason", "exitDetail"] {
        assert_eq!(wire[key], want[key], "{key}: {wire} against {want}");
        checked += 1;
    }
    assert_eq!(w.exit, Some(75), "EX_TEMPFAIL");
    assert!(violations(&v, &wire).is_empty(), "{wire}");
    eprintln!("restart fixture keys checked: {checked}");
    assert_eq!(checked, 3);
}

// ---- R4 task SC1: the sponsor airings (provisional until web's S8 is vendored) ----
//
// Web's S8 adds `airings` and `airingsDropped` to the heartbeat; until it merges and is vendored here
// (`scripts/vendor-contracts.sh`), the vendored `heartbeat` refuses both keys (`additionalProperties:
// false`). So these tests read a PROVISIONAL schema, built below from S8's declared shape
// (`RelayAiring`, P6) on top of the vendored def, and a PROVISIONAL fixture kept beside the tests
// (`tests/fixtures/heartbeat-airings.json`): `contracts/` holds only pinned files, and a hand-edited
// pin would red `vendored_files_match_their_pins`. `airings_are_not_in_the_vendored_contract_yet` reds
// on the re-vendor that brings the keys: point these tests at the vendored def and fixture then, and
// delete the provisional pair.

fn provisional_fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/heartbeat-airings.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("the provisional fixture")).expect("json")
}

/// The vendored `heartbeat` def with S8's two keys (P6): `airings` an array of at most 64 tagged
/// items (strict key sets; ids at most 80, a break kind at most 40 or null), `airingsDropped` an
/// integer ≥ 0. A key the vendored def already declares is refused here, so this cannot shadow it.
fn provisional_heartbeat() -> jsonschema::Validator {
    let doc = contract();
    let mut defs = doc["$defs"].clone();
    let props = defs["heartbeat"]["properties"]
        .as_object_mut()
        .expect("properties");
    assert!(props.get("airings").is_none() && props.get("airingsDropped").is_none());
    let start = json!({
        "type": "object", "additionalProperties": false,
        "required": ["kind", "momentId", "cardId", "style", "source", "breakKind", "at"],
        "properties": {
            "kind": { "const": "start" },
            "momentId": { "type": "string", "maxLength": 80 },
            "cardId": { "type": "string", "maxLength": 80 },
            "style": { "enum": ["lower_third", "banner"] },
            "source": { "enum": ["rotation", "scene", "operator"] },
            "breakKind": { "anyOf": [{ "type": "string", "maxLength": 40 }, { "type": "null" }] },
            "at": { "type": "string" }
        }
    });
    let end = json!({
        "type": "object", "additionalProperties": false,
        "required": ["kind", "momentId", "at", "cutShort"],
        "properties": {
            "kind": { "const": "end" },
            "momentId": { "type": "string", "maxLength": 80 },
            "at": { "type": "string" },
            "cutShort": { "type": "boolean" }
        }
    });
    props.insert(
        "airings".to_owned(),
        json!({ "anyOf": [{ "type": "array", "maxItems": 64, "items": { "oneOf": [start, end] } }, { "type": "null" }] }),
    );
    props.insert(
        "airingsDropped".to_owned(),
        json!({ "anyOf": [{ "type": "integer", "minimum": 0, "maximum": 9_007_199_254_740_991u64 }, { "type": "null" }] }),
    );
    let s = json!({ "$schema": doc["$schema"], "$defs": defs, "$ref": "#/$defs/heartbeat" });
    jsonschema::validator_for(&s).expect("schema compiles")
}

/// The page's line for one wire item: the item without `kind`, after its signal (web's `sponsorLine`).
fn page_line(item: &Value) -> String {
    let mut body = item.as_object().expect("an item").clone();
    let kind = body.remove("kind").expect("a kind");
    format!("seazn:sponsor:{} {}", kind.as_str().unwrap(), Value::Object(body))
}

fn a_start(n: usize) -> AiringLine {
    AiringLine::start(
        format!("{n}:sponsor.set_break"),
        "card".to_owned(),
        AiringStyle::LowerThird,
        AiringSource::Rotation,
        Some("set_break".to_owned()),
        "2026-10-31T12:00:00.000Z".to_owned(),
    )
    .unwrap()
}

/// The tripwire: the vendored contract does not declare the two keys yet. When a re-vendor of web's
/// S8 brings them this reds: move the airings tests onto the vendored def and fixture.
#[test]
fn airings_are_not_in_the_vendored_contract_yet() {
    let hb = &contract()["$defs"]["heartbeat"];
    assert_eq!(
        hb["additionalProperties"], false,
        "the contract refuses an unknown key"
    );
    let declared = hb["properties"].as_object().unwrap();
    for k in ["airings", "airingsDropped"] {
        assert!(
            declared.get(k).is_none(),
            "the vendored contract declares {k} now: point the SC1 airings tests at it and its fixture, \
             and delete the provisional schema and tests/fixtures/heartbeat-airings.json"
        );
    }
    // So the vendored def refuses a beat that carries them: why the keys are off by default.
    let wire = provisional_fixture();
    assert!(!violations(&schema_def("heartbeat"), &wire).is_empty(), "{wire}");
    assert!(violations(&provisional_heartbeat(), &wire).is_empty(), "{wire}");
}

/// The provisional fixture's airings, written by the page as lines, read by the reader, held by the
/// buffer (two lines dropped past the cap, and a 2xx, before them) and written on a beat: the beat is
/// the fixture, every key of it, and validates.
#[test]
fn the_vendored_heartbeat_airings_fixture_round_trips() {
    let want = provisional_fixture();
    let items = want["airings"].as_array().expect("airings");
    assert_eq!(items.len(), 3, "a rotation start, its end, a scene start");
    let mut buffer = AiringBuffer::default();
    for n in 0..AIRING_BUFFER_CAP + 2 {
        buffer.push(a_start(n));
    }
    let filler = buffer.take_for_beat();
    buffer.ack_2xx(&filler);
    for item in items {
        let line = page_line(item);
        let read = parse_sponsor_line(&line).unwrap_or_else(|| panic!("not a sponsor line: {line}"));
        buffer.push(read.unwrap_or_else(|e| panic!("{line}: refused {e:?}")));
    }
    let report = HealthReport {
        state: BeatState::Playing,
        programme_phase: ProgrammePhase::Live,
        input: LinkStatus::Connected,
        output: LinkStatus::Connected,
        graphics: GraphicsStatus::Ready,
        ..report()
    };
    let wire = heartbeat_wire_with(&report, &Measured::default(), &buffer.take_for_beat());
    assert_eq!(wire, want, "the round trip is the fixture");
    let errs = violations(&provisional_heartbeat(), &wire);
    assert!(errs.is_empty(), "{errs:?}");
    eprintln!("airing items round-tripped: {}", items.len());
}

/// No airings, no keys: a beat with an empty batch is the beat without one, key for key (what every
/// beat is while the keys are off).
#[test]
fn a_beat_with_no_airings_carries_neither_key() {
    let mut checked = 0;
    for (r, m) in [
        (report(), Measured::default()),
        (report(), full_measured()),
        (stopped(ExitReason::Ended, Some("drained")), full_measured()),
    ] {
        let with = heartbeat_wire_with(&r, &m, &AiringBatch::default());
        assert!(
            with.get("airings").is_none() && with.get("airingsDropped").is_none(),
            "{with}"
        );
        assert!(
            violations(&schema_def("heartbeat"), &with).is_empty(),
            "the vendored def takes it: {with}"
        );
        checked += 1;
    }
    eprintln!("beats without airings checked: {checked}");
    assert_eq!(checked, 3);
}

/// Fix round 1, m2: with no line to carry but a dropped total above 0 (lines a refused beat carried),
/// the beat carries `airingsDropped` alone, and S8's shape takes it; the vendored def does not yet
/// (why the keys are off until web declares them).
#[test]
fn a_beat_with_only_a_dropped_total_carries_airings_dropped_alone() {
    let mut buffer = AiringBuffer::default();
    buffer.push(a_start(1));
    buffer.push(a_start(2));
    let refused = buffer.take_for_beat();
    assert_eq!(buffer.drop_refused(&refused), 2);
    let batch = buffer.take_for_beat();
    assert!(batch.is_empty() && batch.dropped() == 2);
    let mut checked = 0;
    for r in [report(), stopped(ExitReason::Ended, Some("drained"))] {
        let wire = heartbeat_wire_with(&r, &Measured::default(), &batch);
        assert!(wire.get("airings").is_none(), "no line, no airings: {wire}");
        assert_eq!(wire["airingsDropped"], 2, "{wire}");
        let errs = violations(&provisional_heartbeat(), &wire);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(!violations(&schema_def("heartbeat"), &wire).is_empty(), "{wire}");
        checked += 1;
    }
    eprintln!("dropped-only beats checked: {checked}");
    assert_eq!(checked, 2);
}

/// The largest beat the buffer can give: 64 lines at web's bounds (ids of 80 UTF-16 units, a break
/// kind of 40), every style and source, both kinds, and the largest dropped count, on a periodic and a
/// final beat. Each validates against S8's shape.
#[test]
fn every_airings_beat_we_can_emit_validates() {
    let id = |n: usize| format!("{n:02}{}", "m".repeat(78));
    let styles = [AiringStyle::LowerThird, AiringStyle::Banner];
    let sources = [
        AiringSource::Rotation,
        AiringSource::Scene,
        AiringSource::Operator,
    ];
    let mut buffer = AiringBuffer::default();
    for n in 0..AIRING_BUFFER_CAP {
        let line = if n % 4 == 3 {
            AiringLine::end(id(n), "2026-10-31T12:00:00Z".to_owned(), n % 8 == 3)
        } else {
            AiringLine::start(
                id(n),
                "c".repeat(80),
                styles[n % 2],
                sources[n % 3],
                (n % 5 != 0).then(|| "b".repeat(40)),
                "2026-10-31T12:00:00.123456Z".to_owned(),
            )
        };
        buffer.push(line.expect("at web's bounds"));
    }
    let batch = buffer.take_for_beat();
    assert_eq!(batch.lines().len(), 64);
    let v = provisional_heartbeat();
    let mut checked = 0;
    for r in [report(), stopped(ExitReason::Fatal, Some("boom"))] {
        let wire = heartbeat_wire_with(&r, &full_measured(), &batch);
        assert_eq!(wire["airings"].as_array().map(Vec::len), Some(64));
        assert_eq!(
            wire["airingsDropped"], 0,
            "a measured zero rides with the airings"
        );
        let errs = violations(&v, &wire);
        assert!(errs.is_empty(), "{errs:?}");
        checked += 1;
    }
    // A dropped total past the cap rides as counted (a u32: always inside the contract's maximum).
    let mut full = AiringBuffer::default();
    for n in 0..AIRING_BUFFER_CAP + 3 {
        full.push(a_start(n));
    }
    let wire = heartbeat_wire_with(&report(), &Measured::default(), &full.take_for_beat());
    assert_eq!(wire["airingsDropped"], 3);
    assert!(violations(&v, &wire).is_empty());
    checked += 1;
    let wire = heartbeat_wire_with(&report(), &Measured::default(), &batch);
    let kinds = batch_kinds(&wire);
    assert!(
        kinds.contains(&"start") && kinds.contains(&"end"),
        "both kinds: {kinds:?}"
    );
    eprintln!("airings beats validated: {checked}");
    assert_eq!(checked, 3);
}

fn batch_kinds(wire: &Value) -> Vec<&str> {
    wire["airings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect()
}

/// A named edit of a valid wire.
type Edit = (&'static str, fn(&mut Value));

/// The provisional schema is S8's shape, not a pass-all: each item S8 refuses, it refuses (so the
/// "validates" tests above can fail).
#[test]
fn the_provisional_schema_refuses_what_s8_refuses() {
    let v = provisional_heartbeat();
    let good = provisional_fixture();
    assert!(violations(&v, &good).is_empty());
    let edits: Vec<Edit> = vec![
        ("an unknown key", |w| w["airings"][0]["extra"] = json!(1)),
        ("breakKind absent", |w| {
            w["airings"][0].as_object_mut().unwrap().remove("breakKind");
        }),
        ("no kind", |w| {
            w["airings"][1].as_object_mut().unwrap().remove("kind");
        }),
        ("an unknown kind", |w| w["airings"][1]["kind"] = json!("middle")),
        ("an end with a start's key", |w| {
            w["airings"][1]["cardId"] = json!("c")
        }),
        ("a style S8 lacks", |w| w["airings"][0]["style"] = json!("big")),
        ("a source S8 lacks", |w| {
            w["airings"][2]["source"] = json!("manual")
        }),
        ("a moment id of 81", |w| {
            w["airings"][0]["momentId"] = json!("m".repeat(81))
        }),
        ("a break kind of 41", |w| {
            w["airings"][0]["breakKind"] = json!("b".repeat(41))
        }),
        ("cutShort a string", |w| {
            w["airings"][1]["cutShort"] = json!("false")
        }),
        ("65 items", |w| {
            let one = w["airings"][0].clone();
            w["airings"] = Value::Array(vec![one; 65]);
        }),
        ("a negative dropped", |w| w["airingsDropped"] = json!(-1)),
        ("a fractional dropped", |w| w["airingsDropped"] = json!(1.5)),
    ];
    let mut checked = 0;
    for (what, edit) in &edits {
        let mut w = good.clone();
        edit(&mut w);
        assert!(
            !violations(&v, &w).is_empty(),
            "{what}: the provisional schema took {w}"
        );
        checked += 1;
    }
    eprintln!("provisional refusals checked: {checked}");
    assert_eq!(checked, edits.len());
    assert_eq!(checked, 13);
}
