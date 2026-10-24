//! The chaos cases' verdicts (C14) over the checker's own report: the seam between
//! `harness/cases/*.sh` (with `harness/cases/lib.jq`) and `checker::analyze`.
//!
//! Each test simulates one case's run as the product is declared to behave. The simulation covers
//! the camera's emits, the programme's frames with the camera counter and the overlay index each
//! shows, the heartbeats, the compositor's log with docker stamps, docker events and the case's marks.
//! The programme's frames go through `checker::analyze`, and the report is written beside run.json,
//! case.json and timeline.json, in the shapes run-case.sh writes. Then `run-case.sh <case>
//! [<variant>]` re-judges that directory (`CASE_REJUDGE=1`), as it would a CI artifact.
//!
//! The good run of each case must pass with every check it declares printed. Each red run must fail
//! with its named check. A red run is either the fault never applied (the anti-vacuity clause) or the
//! product regressed (the brief's mutants, simulated).
//!
//! The product's numbers come from harness/thresholds.json, the file the verdicts read: slate after
//! `slate_after_ms` of no input, the resume hold-off, the beat interval, the alive cadence and the
//! restart thresholds. Only the simulation's own physics is typed here, each value beside the fact it
//! stands for: how long a reconnect takes, or when a kill lands.
//!
//! Needs bash 4 or later, jq and python3. macOS's /bin/bash is 3.2, so set HARNESS_BASH to a bash 5
//! there. The test refuses to run under an older bash rather than skip.
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::audio::AudioFacts;
use harness::checker::{analyze, FrameRec, Inputs, SegmentScan, Settings};
use harness::inputs::{crash_times, Beat, Emit, Timeline};
use harness::logfacts::log_facts;
use serde_json::{json, Value};
use time::macros::{datetime, format_description};

const PERIOD_MS: f64 = 1000.0 / 30.0;
/// The camera's glass-to-glass latency to the recording (the smoke run's p50 was about 700 ms).
const LATENCY_MS: f64 = 700.0;
/// The page's draw to the recorded strip.
const OVERLAY_LAG_MS: f64 = 100.0;
/// Compose's restart of the container after its die (`restart: on-failure`).
const RESTART_DELAY_MS: i64 = 1_000;
/// A restarted compositor's start to its first programme packet at MediaMTX (boot, CEF ready, the
/// output's connect).
const FIRST_PACKET_MS: i64 = 6_000;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn t0() -> i64 {
    (datetime!(2026-10-24 12:00:00 UTC).unix_timestamp_nanos() / 1_000_000) as i64
}

/// A docker `--timestamps` stamp and the compositor's own `ts=`, both for `ms`, from the `time`
/// crate (a clock independent of the checker's).
fn stamps(ms: i64) -> (String, String) {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).unwrap();
    let docker = t
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z"
        ))
        .unwrap();
    let ts = t
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        ))
        .unwrap();
    (docker, ts)
}

fn within(spans: &[(i64, i64)], at: f64) -> Option<(i64, i64)> {
    spans
        .iter()
        .copied()
        .find(|&(a, b)| at >= a as f64 && at < b as f64)
}

/// One simulated run.
struct World {
    t: Value,
    live: i64,
    drive: i64,
    end: i64,
    slate_after_ms: f64,
    resume_after_ms: f64,
    /// The camera emits nothing (fake-camera's /pause).
    camera_gaps: Vec<(i64, i64)>,
    /// The camera emits, but the compositor receives nothing (a kicked reader).
    input_gaps: Vec<(i64, i64)>,
    /// MediaMTX receives no programme frames.
    outages: Vec<(i64, i64)>,
    /// The overlay is not drawn.
    overlay_none: Vec<(i64, i64)>,
    /// The overlay holds the frame it had at the span's start (a hung page).
    overlay_hold: Vec<(i64, i64)>,
    /// The page learns no new score event (the feed is down).
    score_blind: Option<(i64, i64)>,
    /// The page hung: no `cef alive` line of its generation from then on.
    page_hang: Option<i64>,
    beats: Vec<(i64, Value)>,
    log: Vec<(i64, String)>,
    /// When the CEF child poll counted a GPU-process death (the `cef health` lines' count).
    gpu_deaths: Vec<i64>,
    /// docker events of the compositor's container: (at, action, exit code).
    events: Vec<(i64, &'static str, Option<i64>)>,
    marks: Vec<(String, i64, Value)>,
    control_state: Value,
    restarts: u64,
    timeline_start: i64,
    delay_ms: i64,
    score_offsets: Vec<(u16, i64)>,
    cores: Value,
}

impl World {
    fn new(live_s: i64) -> Self {
        let t: Value = serde_json::from_str(
            &std::fs::read_to_string(repo().join("harness/thresholds.json")).expect("thresholds.json"),
        )
        .unwrap();
        let session: Value = serde_json::from_str(
            &std::fs::read_to_string(
                repo().join("contracts/fixtures/relay-job.v1/session-composed-720p.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let score: Value =
            serde_json::from_str(&std::fs::read_to_string(repo().join("harness/pages/score.json")).unwrap())
                .unwrap();
        let live = t0();
        let drive = live + 1_000;
        let end = drive + live_s * 1_000;
        let mut w = Self {
            slate_after_ms: t["slate_after_ms"].as_f64().unwrap(),
            resume_after_ms: t["resume_after_ms"].as_f64().unwrap(),
            t,
            live,
            drive,
            end,
            camera_gaps: vec![],
            input_gaps: vec![],
            outages: vec![],
            overlay_none: vec![],
            overlay_hold: vec![],
            score_blind: None,
            page_hang: None,
            beats: vec![],
            log: vec![],
            gpu_deaths: vec![],
            events: vec![(live - 9_000, "start", None)],
            marks: vec![],
            control_state: json!({"session_fetches": 1, "other_session_requests": 0,
                "unauthorized_requests": 0, "score_refused": 0, "page_hang_armed": false,
                "page_hang_served_ms": null, "score_down_until_ms": 0}),
            restarts: 0,
            timeline_start: drive - 50,
            delay_ms: session["overlayDelayMs"].as_i64().unwrap(),
            score_offsets: score["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    (
                        e["index"].as_u64().unwrap() as u16,
                        e["offset_ms"].as_i64().unwrap(),
                    )
                })
                .collect(),
            cores: json!({"diff_ok": true, "container_files": 0, "host_before": 0, "host_after": 0,
                "core_pattern": "core"}),
        };
        let hb = w.ms("heartbeat_interval_ms");
        let mut at = live;
        let mut k = 0;
        while at <= end {
            w.beats.push((at, Self::body(k)));
            at += hb;
            k += 1;
        }
        let hold = w.ms("final_hold_ms");
        w.beats.push((
            end + hold + 200,
            json!({"state": "stopped", "exitReason": "ended", "programmePhase": "ended"}),
        ));
        w.boot(live - 9_000);
        w.log.push((end + hold + 1_000, "exit code=0".into()));
        w.events.push((end + hold + 1_500, "die", Some(0)));
        w
    }

    fn body(k: i64) -> Value {
        json!({"state": "playing", "programmePhase": "live", "input": "connected",
            "output": "connected", "graphics": "ready", "rung": "full", "sinkSwaps": 0,
            "egressBytes": k * 1_500_000, "droppedFrames": 0, "encoderSpeed": 1.0,
            "pipelineRebuilds": 0, "inputKicks": 0, "inputKickAlert": false})
    }

    fn ms(&self, key: &str) -> i64 {
        self.t[key]
            .as_f64()
            .unwrap_or_else(|| panic!("thresholds.json has no {key}")) as i64
    }

    /// The drive's clock: `s` seconds after it started.
    fn at(&self, s: f64) -> i64 {
        self.drive + (s * 1000.0) as i64
    }

    fn mark(&mut self, name: &str, at: i64, value: Value) {
        self.marks.push((name.into(), at, value));
    }

    fn line(&mut self, at: i64, msg: impl Into<String>) {
        self.log.push((at, msg.into()));
    }

    /// A compositor process's boot lines from `start`, and its alive lines until the next die.
    fn boot(&mut self, start: i64) {
        self.line(start + 200, "compositor started drivers=gst,cef");
        self.line(start + 1_500, "cef ready gen=1");
        self.line(start + 2_500, "programme started");
    }

    /// Every beat in `[a, b)`, changed by `f`.
    fn beats_in(&mut self, a: i64, b: i64, f: impl Fn(&mut Value)) {
        for (at, body) in &mut self.beats {
            if *at >= a && *at < b && body["state"] == "playing" {
                f(body);
            }
        }
    }

    /// The container dies at `die` with `code` and compose starts it again; `detail` is the final
    /// beat's exitDetail for an in-place restart (C11b). Returns the start.
    fn restart(&mut self, die: i64, code: i64, detail: Option<&str>) -> i64 {
        let start = die + RESTART_DELAY_MS;
        self.events.push((die, "die", Some(code)));
        self.events.push((start, "start", None));
        self.beats
            .retain(|(at, _)| *at < die - 2_000 || *at >= start + 3_000);
        if let Some(d) = detail {
            self.beats.push((
                die - 300,
                json!({"state": "stopped", "exitReason": "restart", "exitDetail": d}),
            ));
            self.line(die - 100, "exit code=75");
        }
        self.outages.push((die - 200, start + FIRST_PACKET_MS));
        self.boot(start);
        self.restarts += 1;
        start
    }

    fn starts(&self) -> Vec<i64> {
        let mut s: Vec<i64> = self
            .events
            .iter()
            .filter(|e| e.1 == "start")
            .map(|e| e.0)
            .collect();
        s.sort_unstable();
        s
    }

    fn timeline_events(&self) -> Vec<(u16, i64)> {
        self.score_offsets
            .iter()
            .map(|&(i, o)| (i, self.timeline_start + o))
            .collect()
    }

    fn score_at(&self, w: f64) -> Option<u16> {
        if within(&self.overlay_none, w).is_some() {
            return None;
        }
        let w = within(&self.overlay_hold, w).map_or(w, |(a, _)| a as f64);
        let poll = self.ms("harness_page_poll_ms");
        let horizon = match self.score_blind {
            Some((a, b)) if w >= a as f64 && w < (b + poll) as f64 => a as f64 - 1.0,
            _ => w,
        };
        Some(
            self.timeline_events()
                .iter()
                .filter(|&&(_, at)| at as f64 <= horizon && (at + self.delay_ms) as f64 <= w - OVERLAY_LAG_MS)
                .map(|&(i, _)| i)
                .max()
                .unwrap_or(0),
        )
    }

    /// The camera's emits, the programme's recorded segments.
    fn media(&self) -> (Vec<Emit>, Vec<SegmentScan>) {
        let cam0 = self.live as f64 - 30_000.0;
        let mut emits = Vec::new();
        let mut received: Vec<(f64, u32)> = Vec::new();
        let mut c = 1u32;
        loop {
            let wall = cam0 + f64::from(c) * PERIOD_MS;
            if wall > self.end as f64 {
                break;
            }
            if within(&self.camera_gaps, wall).is_none() {
                emits.push(Emit {
                    counter: c,
                    wall_ms: wall.round() as i64,
                });
                if within(&self.input_gaps, wall).is_none() {
                    received.push((wall, c));
                }
            }
            c += 1;
        }
        let newest = |p: f64| received.iter().rev().find(|(w, _)| *w <= p).copied();
        let mut segments: Vec<SegmentScan> = Vec::new();
        let mut open: Option<SegmentScan> = None;
        let mut first_wall = 0.0;
        // Slated since the newest frame shown before the slate.
        let mut slated: Option<f64> = None;
        let prog0 = self.live as f64 - 2_000.0;
        let mut k = 0u32;
        loop {
            let w = prog0 + f64::from(k) * PERIOD_MS;
            k += 1;
            if w > self.end as f64 - PERIOD_MS {
                break;
            }
            if within(&self.outages, w).is_some() {
                segments.extend(open.take());
                slated = None;
                continue;
            }
            let p = w - LATENCY_MS;
            let counter = match (slated, newest(p)) {
                (Some(last), _) => {
                    let back = received.iter().find(|(rw, _)| *rw > last).copied();
                    match back {
                        Some((bw, _)) if bw + self.resume_after_ms <= p => {
                            slated = None;
                            newest(p).map(|n| n.1)
                        }
                        _ => None,
                    }
                }
                (None, Some((nw, nc))) => {
                    if p - nw > self.slate_after_ms {
                        slated = Some(nw);
                        None
                    } else {
                        Some(nc)
                    }
                }
                (None, None) => None,
            };
            let seg = open.get_or_insert_with(|| {
                first_wall = w;
                SegmentScan {
                    file: format!("seg{}.mp4", segments.len() + 1),
                    start_wall_us: (w * 1000.0).round() as i64,
                    frames: vec![],
                    audio: AudioFacts::default(),
                    decode_error: None,
                }
            });
            seg.frames.push(FrameRec {
                pts_ns: ((w - first_wall) * 1e6).round() as u64,
                counter,
                score: self.score_at(w),
            });
        }
        segments.extend(open.take());
        (emits, segments)
    }

    /// The `cef alive` and periodic `cef health` lines of every process, then the whole log, sorted.
    fn log_text(&self) -> String {
        let mut lines = self.log.clone();
        let alive = self.ms("cef_alive_every_ms");
        let hb = self.ms("heartbeat_interval_ms");
        for s in self.starts() {
            let die = self
                .events
                .iter()
                .filter(|e| e.1 == "die" && e.0 >= s)
                .map(|e| e.0)
                .min()
                .unwrap();
            let mut at = s + 1_500 + alive;
            while at < die - 200 {
                if self.page_hang.is_none_or(|h| at < h) {
                    lines.push((at, "cef alive gen=1".into()));
                }
                at += alive;
            }
            let mut at = s + 1_000;
            while at < die - 200 {
                let deaths = self.gpu_deaths.iter().filter(|&&d| d >= s && d <= at).count();
                lines.push((
                    at,
                    format!("cef health zygote_gone=- leaked_teardowns=0 gpu_process_deaths={deaths}"),
                ));
                at += hb;
            }
        }
        lines.sort_by_key(|l| l.0);
        lines
            .iter()
            .map(|(at, m)| {
                let (d, ts) = stamps(*at);
                format!("{d} ts={ts} level=info msg={m}\n")
            })
            .collect()
    }

    /// Writes report.json, run.json, case.json and timeline.json to `dir`.
    fn write(&self, dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let (emit, segments) = self.media();
        let mut beats = self.beats.clone();
        beats.sort_by_key(|b| b.0);
        let text = self.log_text();
        let timeline = Timeline {
            start_wall_ms: self.timeline_start,
            delay_ms: self.delay_ms,
            events: self.timeline_events(),
        };
        let inputs = Inputs {
            segments,
            emit,
            emit_truncated: false,
            beats: beats
                .iter()
                .map(|(at, body)| Beat {
                    received_ms: *at,
                    body: body.clone(),
                })
                .collect(),
            beats_truncated: false,
            timeline,
            crashes_ms: crash_times(&text),
            end_request_ms: Some(self.end),
            log: log_facts(&text),
        };
        let report = analyze(
            &inputs,
            &Settings::from_thresholds(&domain::config::Thresholds::default()),
        );
        let put = |name: &str, v: &Value| std::fs::write(dir.join(name), v.to_string()).unwrap();
        put("report.json", &serde_json::to_value(&report).unwrap());
        let mut sorted = self.events.clone();
        sorted.sort_by_key(|e| e.0);
        let mut events: Vec<Value> = sorted
            .iter()
            .map(|(at, action, code)| {
                json!({"at_ms": at, "action": action, "service": "compositor", "exit_code": code})
            })
            .collect();
        // Another service's events, which no verdict may read as the compositor's.
        events.push(
            json!({"at_ms": self.live - 9_500, "action": "start", "service": "fake-camera",
            "exit_code": null}),
        );
        events.push(
            json!({"at_ms": self.drive + 5_000, "action": "die", "service": "fake-camera",
            "exit_code": 75}),
        );
        let mut state = self.control_state.clone();
        state["session_fetches"] = json!(1 + self.restarts);
        let hold = self.ms("final_hold_ms");
        put(
            "run.json",
            &json!({"case": "", "variant": "", "run": "", "name": "", "live_ms": self.live,
                "drive_start_ms": self.drive, "end_request_ms": self.end,
                "exit_seen_ms": self.end + hold + 2_000,
                "compositor": {"status": "exited", "exit_code": 0, "restarts": self.restarts},
                "restarts_at_live": 0, "last_beat": beats.last().unwrap().1, "events": events,
                "control_state": state, "cores": self.cores}),
        );
        let mut marks = self.marks.clone();
        marks.sort_by_key(|m| m.1);
        let mut case = serde_json::Map::new();
        for (name, at, value) in marks {
            case.entry(name)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap()
                .push(json!({"at_ms": at, "value": value}));
        }
        put("case.json", &Value::Object(case));
        put(
            "timeline.json",
            &json!({"start_wall_ms": self.timeline_start, "delay_ms": self.delay_ms,
                "events": self.timeline_events().iter().map(|(i, at)| json!({"index": i, "at": at}))
                    .collect::<Vec<_>>()}),
        );
    }
}

/// What run-case.sh judged.
struct Verdict {
    status: String,
    lines: Vec<String>,
    json: Value,
    exit: Option<i32>,
}

fn bash() -> String {
    let bash = std::env::var("HARNESS_BASH").unwrap_or_else(|_| "bash".into());
    let out = Command::new(&bash)
        .args(["-c", "echo ${BASH_VERSINFO[0]}"])
        .output()
        .expect("bash runs");
    let major: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    assert!(
        major >= 4,
        "run-case.sh needs bash 4 or later; {bash} is {major} (set HARNESS_BASH)"
    );
    bash
}

/// Re-judges `w` as `<case> [<variant>]`, in a directory of its own (`tag`).
fn judge(w: &World, case: &str, variant: &str, tag: &str) -> Verdict {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("case-verdicts")
        .join(tag);
    let _ = std::fs::remove_dir_all(&root);
    let name = if variant.is_empty() {
        case.to_owned()
    } else {
        format!("{case}-{variant}")
    };
    let dir = root.join(&name);
    w.write(&dir);
    let mut cmd = Command::new(bash());
    cmd.arg(repo().join("harness/run-case.sh"))
        .arg(case)
        .current_dir(repo())
        .env("CASE_REJUDGE", "1")
        .env("CASE_OUT_ROOT", &root)
        .env_remove("CASE_RUN");
    if !variant.is_empty() {
        cmd.arg(variant);
    }
    let out = cmd.output().expect("run-case.sh runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    eprintln!("--- {tag}: {case} {variant}\n{stdout}{stderr}");
    let json: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("verdict.json"))
            .unwrap_or_else(|_| panic!("{tag}: no verdict.json")),
    )
    .unwrap();
    Verdict {
        status: json["status"].as_str().unwrap_or("").to_owned(),
        lines: stdout.lines().map(str::to_owned).collect(),
        json,
        exit: out.status.code(),
    }
}

/// Lines of each kind the verdict printed.
fn counts(v: &Verdict) -> (usize, usize, usize) {
    let n = |p: &str| v.lines.iter().filter(|l| l.starts_with(p)).count();
    (n("PASS "), n("FAIL "), n("MEASURE ") + n("OWED "))
}

/// The good run: a pass, every declared check run and printed, none failed.
fn assert_pass(v: &Verdict, tag: &str) {
    let (pass, fail, other) = counts(v);
    let declared = v.json["checks_declared"].as_u64().unwrap() as usize;
    eprintln!("{tag}: {pass} PASS, {fail} FAIL, {other} measured, {declared} declared");
    assert_eq!(fail, 0, "{tag}: failures {:?}", v.json["failed"]);
    assert_eq!(v.status, "pass", "{tag}");
    assert_eq!(v.exit, Some(0), "{tag}");
    assert!(declared > 0, "{tag}: a case that declares no checks");
    assert_eq!(v.json["checks_run"].as_u64(), Some(declared as u64), "{tag}");
    assert_eq!(pass + other, declared, "{tag}: every check prints one line");
    assert!(pass >= 1, "{tag}: nothing judged");
    // run-chaos.sh's table shows the case's key: a check or measure the verdict recorded.
    let key = v.json["key"].as_str().unwrap_or("");
    assert!(
        !key.is_empty() && v.json["measures"].get(key).is_some(),
        "{tag}: the key '{key}' is not among the measures {:?}",
        v.json["measures"]
            .as_object()
            .map(|m| m.keys().collect::<Vec<_>>())
    );
}

/// A red run: a fail with `check` named, and the verdict still ran every check it declares.
fn assert_fails(v: &Verdict, check: &str, tag: &str) {
    let failed: Vec<&str> = v.json["failed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    eprintln!("{tag}: failed {failed:?}");
    assert_eq!(v.status, "fail", "{tag}");
    assert_eq!(v.exit, Some(1), "{tag}");
    assert!(
        v.lines.iter().any(|l| l.starts_with(&format!("FAIL {check} ("))),
        "{tag}: no `FAIL {check}` line"
    );
    assert!(
        failed.contains(&check),
        "{tag}: {check} not in verdict.json's failed"
    );
    assert_eq!(v.json["checks_run"], v.json["checks_declared"], "{tag}");
}

// ---- the cases ----

fn camera_stop() -> World {
    let mut w = World::new(50);
    let (p, r) = (w.at(20.0), w.at(30.0));
    w.mark("pause", p, Value::Null);
    w.mark("resume", r, Value::Null);
    w.camera_gaps.push((p + 30, r + 30));
    w
}

#[test]
fn camera_stop_slates_and_returns_and_reds_without_its_pause() {
    assert_pass(
        &judge(&camera_stop(), "camera-stop", "", "stop-good"),
        "stop-good",
    );
    let mut w = camera_stop();
    w.camera_gaps.clear();
    assert_fails(
        &judge(&w, "camera-stop", "", "stop-nopause"),
        "camera_paused",
        "stop-nopause",
    );
    let mut w = camera_stop();
    w.slate_after_ms += 500.0;
    assert_fails(
        &judge(&w, "camera-stop", "", "stop-lateslate"),
        "slate_after_loss",
        "stop-lateslate",
    );
    let mut w = camera_stop();
    let at = w.at(40.0);
    w.line(
        at,
        "cef health zygote_gone=1 leaked_teardowns=0 gpu_process_deaths=0",
    );
    assert_fails(
        &judge(&w, "camera-stop", "", "stop-fpzygote"),
        "fp_no_zygote_verdict",
        "stop-fpzygote",
    );
}

fn camera_flap() -> World {
    let mut w = World::new(125);
    for i in 0..12 {
        let (p, r) = (w.at(5.0 + 10.0 * f64::from(i)), w.at(10.0 + 10.0 * f64::from(i)));
        w.mark("pause", p, Value::Null);
        w.mark("resume", r, Value::Null);
        w.camera_gaps.push((p + 30, r + 30));
    }
    w
}

#[test]
fn camera_flap_holds_off_each_return_and_reds_without_the_hold_off() {
    assert_pass(
        &judge(&camera_flap(), "camera-flap", "", "flap-good"),
        "flap-good",
    );
    // Mutant 1: resume_after_ms 0.
    let mut w = camera_flap();
    w.resume_after_ms = 0.0;
    assert_fails(
        &judge(&w, "camera-flap", "", "flap-nohold"),
        "flap_hysteresis",
        "flap-nohold",
    );
}

/// Kicks at 20 s, 40 s, ... 120 s; each costs the input `reconnect_ms`; the alert from `alert_from`
/// kicks on.
fn reader_kicked(reconnect_ms: i64, alert_from: usize) -> World {
    let mut w = World::new(130);
    let kicks: Vec<i64> = (1..=6).map(|k| w.at(20.0 * f64::from(k))).collect();
    for (n, &k) in kicks.iter().enumerate() {
        w.mark("kick", k, json!(format!("conn-{n}")));
        w.input_gaps.push((k + 100, k + 100 + reconnect_ms));
    }
    let end = w.end;
    w.mark("readers_after", w.at(130.0), json!(["conn-6"]));
    for (n, &k) in kicks.iter().enumerate() {
        let next = kicks.get(n + 1).copied().unwrap_or(end + 1);
        let kicks_so_far = n + 1;
        w.beats_in(k + 100, next + 100, |b| {
            b["inputKicks"] = json!(kicks_so_far);
            b["inputKickAlert"] = json!(kicks_so_far >= alert_from);
        });
    }
    w
}

#[test]
fn reader_kicked_returns_after_each_kick_and_reds_on_a_missed_kick_or_an_early_alert() {
    let alert_from = 4; // kick_alert_count 3: the alert is kicks > 3
    assert_eq!(World::new(1).ms("kick_alert_count") as usize + 1, alert_from);
    assert_pass(
        &judge(
            &reader_kicked(1_400, alert_from),
            "reader-kicked",
            "",
            "kick-good",
        ),
        "kick-good",
    );
    let mut w = reader_kicked(1_400, alert_from);
    let at = w.at(125.0);
    w.mark("kick_missed", at, Value::Null);
    assert_fails(
        &judge(&w, "reader-kicked", "", "kick-missed"),
        "kicks_applied",
        "kick-missed",
    );
    let w = reader_kicked(1_400, 1);
    assert_fails(
        &judge(&w, "reader-kicked", "", "kick-early"),
        "kick_alert_not_early",
        "kick-early",
    );
    let w = reader_kicked(12_000, alert_from);
    assert_fails(
        &judge(&w, "reader-kicked", "", "kick-slow"),
        "return_after_kicks",
        "kick-slow",
    );
}

fn netem(present: bool, sent_bytes: u64, sent_pkts: u64, dropped: u64) -> Value {
    json!({"leg": "out", "present": present, "sent_bytes": sent_bytes, "sent_pkts": sent_pkts,
        "dropped": dropped})
}

/// The output cut at 20 s and cleared at 50 s; MediaMTX has frames again `back_ms` after the clear.
fn dest_cut(back_ms: i64, stats: Value) -> World {
    let mut w = World::new(80);
    let (cut, clear) = (w.at(20.0), w.at(50.0));
    w.mark("cut", cut, Value::Null);
    w.mark("netem_out", clear, stats);
    w.mark("clear", clear + 50, Value::Null);
    w.outages.push((cut + 100, clear + 50 + back_ms));
    w.beats_in(cut + 100, clear + 50 + back_ms, |b| {
        b["output"] = json!("backoff")
    });
    w
}

#[test]
fn dest_disconnect_recovers_and_reds_when_nothing_was_dropped_or_the_return_is_late() {
    let w = dest_cut(2_500, netem(true, 900_000, 700, 4_000));
    assert_pass(&judge(&w, "dest-disconnect", "", "disc-good"), "disc-good");
    let w = dest_cut(2_500, netem(true, 900_000, 700, 0));
    assert_fails(
        &judge(&w, "dest-disconnect", "", "disc-nodrop"),
        "fault_applied",
        "disc-nodrop",
    );
    let w = dest_cut(7_000, netem(true, 900_000, 700, 4_000));
    assert_fails(
        &judge(&w, "dest-disconnect", "", "disc-late"),
        "output_reconnect",
        "disc-late",
    );
}

/// A stall from 20 s. The egress total grows once more after the cut (the kernel's send buffer
/// drains for under a second), then by `trickle` at each beat until the clear. With `swaps`, the
/// stall is seen `output_stall_after_ms` after that drain, on the next output poll, and the next beat
/// carries the swap.
fn dest_stall(trickle: i64, swaps: bool, stats: Value) -> World {
    let mut w = dest_cut(2_500, stats);
    let cut = w.at(20.0);
    let clear = w.at(50.0) + 50;
    let drained = cut + 800;
    let swap = drained + w.ms("output_stall_after_ms") + w.ms("output_progress_poll_ms");
    let mut total = 0;
    let mut drain_seen = false;
    for (at, body) in &mut w.beats {
        if body["state"] != "playing" {
            continue;
        }
        total += if *at < cut || *at >= clear {
            1_500_000
        } else if !drain_seen {
            drain_seen = true;
            40_000
        } else {
            trickle
        };
        body["egressBytes"] = json!(total);
        body["sinkSwaps"] = json!(i64::from(swaps && *at >= swap));
    }
    if swaps {
        w.line(swap, "sink swap gen=1 target=rtmp");
    }
    w
}

#[test]
fn dest_stall_swaps_the_sink_and_reds_without_a_swap() {
    let w = dest_stall(0, true, netem(true, 3_000, 30, 0));
    assert_pass(&judge(&w, "dest-stall", "", "stall-good"), "stall-good");
    let w = dest_stall(0, false, netem(true, 3_000, 30, 0));
    assert_fails(
        &judge(&w, "dest-stall", "", "stall-noswap"),
        "stall_swap",
        "stall-noswap",
    );
}

#[test]
fn dest_trickle_swaps_under_a_trickle_and_reds_when_nothing_trickled() {
    let w = dest_stall(5_000, true, netem(true, 30_000, 300, 0));
    assert_pass(&judge(&w, "dest-trickle", "", "trickle-good"), "trickle-good");
    // Mutant 5: the netem line commented out: nothing trickles (a plain stall), and the qdisc is a
    // pass-through that never existed.
    let w = dest_stall(0, true, netem(false, 0, 0, 0));
    assert_fails(
        &judge(&w, "dest-trickle", "", "trickle-none"),
        "trickled",
        "trickle-none",
    );
}

fn cpu_starved(bogo_ops: u64, rung_back: bool, felt: bool) -> World {
    let mut w = World::new(80);
    let (a, b) = (w.at(20.0), w.at(60.0));
    w.mark("stress_start", a, Value::Null);
    w.mark("stress_end_seen", b + 200, Value::Null);
    w.mark("stress_end", b, json!({"exit_code": 0, "bogo_ops": bogo_ops}));
    if felt {
        w.beats_in(a, b, |x| {
            x["droppedFrames"] = json!(40);
            x["encoderSpeed"] = json!(0.8);
        });
        let half = a + 6_000;
        let end = w.end;
        w.beats_in(half, end + 1, |x| x["rung"] = json!("overlay_half_rate"));
        if rung_back {
            w.beats_in(b + 5_000, end + 1, |x| x["rung"] = json!("full"));
        }
    }
    w
}

#[test]
fn cpu_starved_holds_its_frame_rate_and_reds_without_stress_or_when_the_rung_climbs() {
    assert_pass(
        &judge(&cpu_starved(52_000, false, true), "cpu-starved", "", "cpu-good"),
        "cpu-good",
    );
    let w = cpu_starved(0, false, true);
    assert_fails(
        &judge(&w, "cpu-starved", "", "cpu-noops"),
        "stress_ran",
        "cpu-noops",
    );
    let w = cpu_starved(52_000, true, true);
    assert_fails(
        &judge(&w, "cpu-starved", "", "cpu-climbs"),
        "rung_holds",
        "cpu-climbs",
    );
    let w = cpu_starved(52_000, false, false);
    assert_fails(
        &judge(&w, "cpu-starved", "", "cpu-unfelt"),
        "stress_seen_in_beats",
        "cpu-unfelt",
    );
}

fn score_down(page_says: bool, overlay_drops: bool) -> World {
    let mut w = World::new(100);
    let down = w.at(20.0);
    let until = down - 5 + 60_000;
    w.mark("down", down, json!({"score_down_until_ms": until}));
    w.control_state["score_refused"] = json!(60);
    w.control_state["score_down_until_ms"] = json!(until);
    w.score_blind = Some((down, until));
    let poll = w.ms("harness_page_poll_ms");
    let stale_after = w.ms("budget_score_stale_after_ms");
    // The page's last good poll before the feed went down, its stale line, its first good poll.
    let last_good = down - poll / 2;
    let stale = last_good + stale_after + poll / 4;
    let fresh = until + poll / 2;
    if page_says {
        w.line(stale, "gen=1: score stale=true");
        w.line(fresh, "gen=1: score stale=false");
        let end = w.end;
        w.beats_in(stale + 100, fresh + 100, |b| b["scoreStale"] = json!(true));
        w.beats_in(fresh + 100, end + 1, |b| b["scoreStale"] = json!(false));
    }
    if overlay_drops {
        w.overlay_none.push((down + 30_000, down + 31_000));
    }
    w
}

#[test]
fn score_down_reports_stale_and_holds_the_last_score_and_reds_when_the_page_is_silent() {
    assert_pass(
        &judge(&score_down(true, false), "score-down", "", "score-good"),
        "score-good",
    );
    let w = score_down(false, false);
    assert_fails(
        &judge(&w, "score-down", "", "score-silent"),
        "stale_line",
        "score-silent",
    );
    let w = score_down(true, true);
    assert_fails(
        &judge(&w, "score-down", "", "score-blank"),
        "last_good_held",
        "score-blank",
    );
}

fn cef_killed(kill: Value, code: i64) -> World {
    let mut w = World::new(70);
    let k = w.at(20.0);
    let mut v = kill;
    v["at_ms"] = json!(k);
    w.mark("kill", k, v);
    w.restart(k + 1_200, code, None);
    w
}

#[test]
fn cef_killed_restarts_the_process_and_reds_when_the_kill_missed_or_the_exit_is_in_place() {
    let good = || json!({"killed": [201, 202, 203], "types": ["zygote", "zygote", "gpu-process"], "rc": 0});
    assert_pass(
        &judge(&cef_killed(good(), 133), "cef-killed", "", "cef-good"),
        "cef-good",
    );
    let w = cef_killed(json!({"killed": [], "types": [], "rc": 1}), 133);
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-missed"),
        "kill_applied",
        "cef-missed",
    );
    let w = cef_killed(good(), 75);
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-inplace"),
        "died_not_in_place",
        "cef-inplace",
    );
    let mut w = cef_killed(good(), 133);
    let (a, b) = (w.at(30.0), w.end);
    w.beats_in(a, b, |x| x["graphics"] = json!("failed"));
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-failed"),
        "beats_never_failed",
        "cef-failed",
    );
    let mut w = cef_killed(good(), 133);
    let k = w.at(20.0);
    w.log.retain(|(at, m)| !(m.starts_with("cef ready") && *at > k));
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-noready"),
        "cef_ready_after_restart",
        "cef-noready",
    );
}

fn process_killed(pid: Value, restart_delay: i64) -> World {
    let mut w = World::new(70);
    let k = w.at(20.0);
    w.mark("netem_before", w.at(18.0), netem(true, 9_000_000, 8_000, 0));
    w.mark("kill", k, json!({"at_ms": k, "pid": pid, "rc": 0}));
    w.line(k + 50, "the compositor was killed by SIGKILL (status 137)");
    let start = w.restart(k + 80, 137, None);
    if restart_delay > 0 {
        w.events.retain(|e| e.0 != start);
        w.events.push((start + restart_delay, "start", None));
        let o = w.outages.last_mut().unwrap();
        o.1 += restart_delay;
    }
    w.mark("netem_after", w.at(55.0), json!({"present": false}));
    w
}

#[test]
fn process_killed_restarts_within_budget_and_reds_on_a_missed_kill_or_a_slow_restart() {
    assert_pass(
        &judge(&process_killed(json!(31), 0), "process-killed", "", "proc-good"),
        "proc-good",
    );
    let w = process_killed(Value::Null, 0);
    assert_fails(
        &judge(&w, "process-killed", "", "proc-nopid"),
        "kill_applied",
        "proc-nopid",
    );
    let w = process_killed(json!(31), 9_000);
    assert_fails(
        &judge(&w, "process-killed", "", "proc-slow"),
        "restart_outage_in_budget",
        "proc-slow",
    );
}

fn input_impaired(dropped: u64, retrans_after: u64) -> World {
    let mut w = World::new(100);
    let (a, b) = (w.at(20.0), w.at(80.0));
    let reader = |r: u64| {
        json!([{"id": "c1", "created": "x", "state": "read", "packetsRetrans": r,
        "packetsReceivedLoss": 0}])
    };
    w.mark("readers_before", a, reader(2));
    w.mark("impair", a + 20, Value::Null);
    w.mark(
        "netem_in",
        b,
        json!({"leg": "in", "present": true, "sent_bytes": 40_000_000,
        "sent_pkts": 30_000, "dropped": dropped}),
    );
    w.mark("readers_at_clear", b + 30, reader(retrans_after));
    w.mark("clear", b + 60, Value::Null);
    w
}

#[test]
fn input_impaired_keeps_the_clock_and_reds_when_nothing_was_dropped_or_retransmitted() {
    assert_pass(
        &judge(&input_impaired(600, 900), "input-impaired", "", "imp-good"),
        "imp-good",
    );
    let w = input_impaired(0, 900);
    assert_fails(
        &judge(&w, "input-impaired", "", "imp-nodrop"),
        "fault_applied",
        "imp-nodrop",
    );
    let w = input_impaired(600, 2);
    assert_fails(
        &judge(&w, "input-impaired", "", "imp-noretrans"),
        "retransmitted",
        "imp-noretrans",
    );
}

/// The page hangs at 20 s plus a poll. With `detected`, the compositor hides it
/// `cef_hang_after_ms` after its last alive line and restarts graphics; without, the overlay holds
/// to the end.
fn cef_hung(detected: bool) -> World {
    let mut w = World::new(60);
    let armed = w.at(20.0);
    w.mark("armed", armed, json!({"page_hang_armed": true}));
    let hang = armed + 700;
    w.page_hang = Some(hang);
    w.control_state["page_hang_served_ms"] = json!(hang - 50);
    w.line(hang, "gen=1: page console: hang starts");
    let alive = w.ms("cef_alive_every_ms");
    let start = w.starts()[0];
    let mut last_alive = start + 1_500 + alive;
    while last_alive + alive <= hang {
        last_alive += alive;
    }
    if !detected {
        let end = w.end;
        w.overlay_hold.push((hang, end + 1));
        return w;
    }
    let hide = last_alive + w.ms("cef_hang_after_ms") + 20;
    let ready = hide + 2_500;
    w.overlay_hold.push((hang, hide + 80));
    w.overlay_none.push((hide + 80, ready + 300));
    w.line(hide, "overlay hidden reason=hang gen=1");
    w.line(hide + 50, "graphics restart gen=2");
    w.line(ready, "cef ready gen=2");
    let mut at = ready + alive;
    while at < w.end {
        w.line(at, "cef alive gen=2");
        at += alive;
    }
    w.beats_in(hide, ready, |b| b["graphics"] = json!("restarting"));
    w
}

#[test]
fn cef_hung_hides_and_restarts_graphics_and_reds_when_the_hang_is_never_seen() {
    assert_pass(&judge(&cef_hung(true), "cef-hung", "", "hung-good"), "hung-good");
    // Mutant 4: C05b's hang arm deleted at Ready: the hang is never seen.
    assert_fails(
        &judge(&cef_hung(false), "cef-hung", "", "hung-unseen"),
        "one_hang_hide",
        "hung-unseen",
    );
}

/// A zygote killed at 20 s; with `in_place`, the compositor names it gone within the budget and
/// restarts in place (exit 75).
fn zygote(child: &str, in_place: bool, killed_type: &str) -> World {
    let mut w = World::new(70);
    let k = w.at(20.0);
    let want = if child == "gpu-process" { "gpu" } else { "renderer" };
    w.mark(
        "kill",
        k,
        json!({"at_ms": k, "pid": 41, "ppid": 17, "compositor": 17, "type": killed_type,
            "want": want, "children": [52], "child_types": [child], "rc": 0}),
    );
    if in_place {
        let verdict = k + w.ms("cef_child_poll_ms") - 100;
        w.line(
            verdict,
            "cef health zygote_gone=1 leaked_teardowns=0 gpu_process_deaths=0",
        );
        let start = w.restart(
            verdict + 600,
            75,
            Some("restart in place: a CEF zygote is gone (gen 1)"),
        );
        if child == "renderer" {
            let at = w.at(60.0);
            for (pid, ppid, off, ty) in [
                (17, 9, 100, Value::Null),
                (60, 17, 1_200, json!("zygote")),
                (61, 60, 1_400, json!("gpu-process")),
            ] {
                w.mark(
                    "tree_after",
                    at,
                    json!({"at_ms": at, "pid": pid, "ppid": ppid, "start_ms": start + off,
                        "comm": "compositor", "type": ty}),
                );
            }
        }
    }
    w
}

#[test]
fn cef_gpu_zygote_killed_restarts_in_place_and_reds_without_the_in_place_restart() {
    let w = zygote("gpu-process", true, "zygote");
    assert_pass(&judge(&w, "cef-gpu-zygote-killed", "", "zyg-good"), "zyg-good");
    // Mutant 6: restart_cause None for a zygote: no verdict, no restart.
    let w = zygote("gpu-process", false, "zygote");
    assert_fails(
        &judge(&w, "cef-gpu-zygote-killed", "", "zyg-none"),
        "died_in_place",
        "zyg-none",
    );
    let w = zygote("gpu-process", true, "gpu-process");
    assert_fails(
        &judge(&w, "cef-gpu-zygote-killed", "", "zyg-notzygote"),
        "killed_the_zygote",
        "zyg-notzygote",
    );
}

#[test]
fn cef_renderer_zygote_killed_restarts_in_place_and_reds_when_an_old_process_survives() {
    let w = zygote("renderer", true, "zygote");
    assert_pass(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-good"),
        "rzyg-good",
    );
    let mut w = zygote("renderer", true, "zygote");
    let at = w.at(60.0);
    w.mark(
        "tree_after",
        at,
        json!({"at_ms": at, "pid": 42, "ppid": 17, "start_ms": w.live - 7_000, "comm": "compositor",
            "type": "renderer"}),
    );
    assert_fails(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-survivor"),
        "old_tree_gone",
        "rzyg-survivor",
    );
}

/// GPU-process kills from 20 s, `spacing_ms` apart, `kills` of them; the child poll counts each
/// `count_lag_ms` after it. With `restarts`, the compositor restarts in place after the poll that
/// counted `cef_gpu_deaths_to_restart`, `exit_after_ms` after that poll.
fn gpu_deaths(kills: u32, spacing_ms: i64, count_lag_ms: i64, restarts: bool, exit_after_ms: i64) -> World {
    let mut w = World::new(90);
    let k0 = w.at(20.0);
    let n = w.ms("cef_gpu_deaths_to_restart") as usize;
    let kill_at: Vec<i64> = (0..i64::from(kills)).map(|i| k0 + i * spacing_ms).collect();
    let counted: Vec<i64> = kill_at.iter().map(|k| k + count_lag_ms).collect();
    let deciding = counted.get(n - 1).copied();
    let die = deciding.filter(|_| restarts).map(|d| d + exit_after_ms);
    for (i, &k) in kill_at.iter().enumerate() {
        if die.is_some_and(|d| k >= d) {
            break;
        }
        w.mark("kill", k, json!({"at_ms": k, "pid": 300 + i, "n": i + 1}));
        w.gpu_deaths.push(counted[i]);
        let deaths = w.gpu_deaths.len();
        if die.is_none_or(|d| counted[i] < d) {
            w.line(
                counted[i],
                format!("cef health zygote_gone=- leaked_teardowns=0 gpu_process_deaths={deaths}"),
            );
        }
    }
    if let (Some(d), Some(line)) = (die, deciding) {
        let detail = format!("restart in place: {n} CEF GPU-process deaths");
        w.restart(d, 75, Some(&detail));
        assert!(line < d);
    }
    w
}

#[test]
fn cef_gpu_deaths_threshold_restarts_in_place_and_reds_when_the_threshold_is_six() {
    let n = World::new(1).ms("cef_gpu_deaths_to_restart") as u32;
    let w = gpu_deaths(n, 2_000, 800, true, 600);
    assert_pass(&judge(&w, "cef-gpu-deaths", "threshold", "gpu-good"), "gpu-good");
    // Mutant 7: cef_gpu_deaths_to_restart 6: the third death restarts nothing.
    let w = gpu_deaths(n, 2_000, 800, false, 0);
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "threshold", "gpu-six"),
        "died_in_place",
        "gpu-six",
    );
}

#[test]
fn cef_gpu_deaths_one_fewer_restarts_nothing_and_reds_on_a_restart() {
    let n = World::new(1).ms("cef_gpu_deaths_to_restart") as u32;
    let w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    assert_pass(
        &judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-good"),
        "fewer-good",
    );
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let at = w.at(40.0);
    w.restart(at, 75, Some("restart in place: 2 CEF GPU-process deaths"));
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-restart"),
        "no_restart",
        "fewer-restart",
    );
}

#[test]
fn cef_gpu_deaths_fast_restarts_before_the_abort_and_reds_when_the_loop_reaches_it() {
    let abort = World::new(1).ms("chromium_gpu_deaths_abort") as u32;
    // A new GPU process 700 ms after each kill; the 1 s child poll counts each within a second.
    let w = gpu_deaths(abort - 1, 700, 350, true, 800);
    assert_pass(&judge(&w, "cef-gpu-deaths", "fast", "fast-good"), "fast-good");
    // The restart comes too late: the loop's last kill (one short of the abort) lands before it.
    let w = gpu_deaths(abort - 1, 700, 350, true, 3_000);
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "fast", "fast-late"),
        "last_kill_not_before_die",
        "fast-late",
    );
}

#[test]
fn cef_leaks_is_reported_not_runnable_never_faked() {
    for variant in ["three", "two"] {
        let tag = format!("leaks-{variant}");
        let v = judge(&World::new(60), "cef-leaks", variant, &tag);
        eprintln!("{tag}: {}", v.json);
        assert_eq!(v.status, "not_runnable", "{tag}");
        assert_eq!(v.exit, Some(3), "{tag}");
        assert_eq!(v.json["checks_run"], 0, "{tag}");
        assert!(v.json["not_runnable_reason"]
            .as_str()
            .is_some_and(|r| r.contains("hold_teardowns")));
        assert!(v.lines.iter().any(|l| l.starts_with("NOT RUNNABLE ")), "{tag}");
    }
}

/// Every case file in harness/cases has a test here: a new case cannot ship unjudged.
#[test]
fn every_case_file_is_judged_here() {
    let me = include_str!("case_verdicts.rs");
    let mut n = 0;
    for e in std::fs::read_dir(repo().join("harness/cases")).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        let Some(case) = name.strip_suffix(".sh") else {
            continue;
        };
        // C13's smoke case, the harness's own end-to-end run (harness.yml), is not a chaos case.
        if case == "smoke" {
            continue;
        }
        n += 1;
        assert!(
            me.contains(&format!("\"{case}\"")),
            "harness/cases/{name} has no verdict test in case_verdicts.rs"
        );
    }
    eprintln!("case files checked: {n}");
    assert_eq!(n, 16, "the brief's 16 cases (amended 2026-10-20)");
}
