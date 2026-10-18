//! The vendored contract is the arbiter: our wire output must validate against it, and its fixtures
//! must decode into our types. Expected values come from the vendored files, never from our code.
use adapters::control_http::{heartbeat_wire, parse_session, programme_phase_wire};
use app::ports::{FetchError, Measured};
use domain::config::{OutputProfile, Thresholds};
use domain::health::{BeatState, ExitInfo, GraphicsStatus, HealthReport, LinkStatus, PhaseChannel};
use domain::ladder::Rung;
use domain::lifecycle::ExitReason;
use domain::programme::ProgrammePhase;
use domain::supervisor::{Desired, Effect, Event, Refusal, SignalKind, Supervisor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `docs/_INDEX.md` Owed 24: web's own pin of `docs/contracts/relay-job.v1.json`, copied from
/// `seazn.club` `apps/web/src/server/api-v1/__tests__/relay-contract.test.ts` (`SHA256`, at
/// eb6538a79, the #980 merge). A contract bump moves web's constant and this one together; a
/// vendored file that hashes otherwise drifted from the contract web enforces.
const WEB_PINNED_CONTRACT_SHA256: &str = "fc99d44899ee543cfe36c48902573501f815d68b4ec7c2e4bbb50d096608dc50";

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
            detail: detail.map(str::to_owned),
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
            detail: Some(String::new()),
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
                detail: Some(detail.clone()),
            },
            10,
        );
        let beats = beats_of(&d.effects);
        assert_eq!(beats.len(), 1);
        assert_eq!(
            beats[0].exit.as_ref().unwrap().detail.as_deref(),
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
                Effect::SwapSink { gen } => {
                    self.swaps.push(gen);
                    self.sink_gen = gen;
                    self.sink_total = 0;
                }
                Effect::RestartGraphics { gen } => {
                    self.page_gen = gen;
                    self.pending.push(Event::CefReady { gen });
                }
                Effect::DrainOutput => self.pending.push(Event::OutputDrained),
                Effect::SendHeartbeat(h) => self.beats.push(h),
                Effect::Exit(c) => self.exit = Some(c),
                Effect::FetchSession
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
                self.decide(Event::InputFrame);
            }
            if self.sink_gen > 0 && self.now - self.last_progress >= 1_000 {
                self.sink_total += bytes_per_second();
                self.last_progress = self.now;
                self.progress_sent += 1;
                self.decide(Event::OutputProgress {
                    gen: self.sink_gen,
                    total_bytes: self.sink_total,
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
        detail: Some(fatal_detail()),
    });
    out.push(("fatal", w));
    let mut w = World::new();
    w.boot(1_000, far);
    w.run_until(2_000);
    w.decide(Event::Signal(SignalKind::Term));
    w.pending.clear(); // the drain does not finish: the fatal error lands inside it
    w.decide(Event::Fatal {
        detail: Some(fatal_detail()),
    });
    out.push(("fatal-in-drain", w));
    out
}

/// How each sweep session must end: its final beat's `exitReason` and the process exit code. Both come
/// from the contract's own `exitReason` description: ended, deadline, signalled and session_gone exit
/// 0; refused exits 78; fatal exits 70.
const SESSION_ENDS: [(&str, &str, i32); 10] = [
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
    assert_eq!(declared.len(), 6, "the six exit reasons");
}
