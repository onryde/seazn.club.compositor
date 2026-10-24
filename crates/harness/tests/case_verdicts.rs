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
/// How long before docker's `start` event a restarted container's init started (sims only: the
/// daemon reports a start it has already made; unmeasured, so case 14 never reads it).
const INIT_BEFORE_START_EVENT_MS: i64 = 30;
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
    /// When a teardown was counted leaked (the `cef health` lines' `leaked_teardowns`, case 16).
    leaks: Vec<i64>,
    /// docker events of the compositor's container: (at, action, exit code).
    events: Vec<(i64, &'static str, Option<i64>)>,
    marks: Vec<(String, i64, Value)>,
    control_state: Value,
    restarts: u64,
    timeline_start: i64,
    delay_ms: i64,
    score_offsets: Vec<(u16, i64)>,
    cores: Value,
    /// Changes report.json after the checker wrote it (a report from an older checker).
    report_patch: Option<fn(&mut Value)>,
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
            leaks: vec![],
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
            report_patch: None,
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

    /// A sink swap as the compositor logs it: the driver's reason line, then the decorator's swap
    /// line (Owed 87, PR #57).
    fn swap(&mut self, at: i64, gen: u64, reason: &str) {
        self.line(at, format!("sink swap reason={reason} gen={gen}"));
        self.line(at, format!("sink swap gen={gen} target=rtmp"));
    }

    /// Generation `from`'s reason line named generation `to` instead.
    fn relabel_reason(&mut self, from: u64, to: u64) {
        let line = format!(" gen={from}");
        let at = self
            .log
            .iter()
            .position(|(_, m)| m.starts_with("sink swap reason=") && m.ends_with(&line))
            .unwrap_or_else(|| panic!("no reason line for gen {from}"));
        self.log[at].1 = self.log[at].1.replace(&line, &format!(" gen={to}"));
    }

    /// Generation `gen`'s reason line read `reason` instead, or was never logged (`None`).
    fn reason_line(&mut self, gen: u64, reason: Option<&str>) {
        let line = format!(" gen={gen}");
        let at = self
            .log
            .iter()
            .position(|(_, m)| m.starts_with("sink swap reason=") && m.ends_with(&line))
            .unwrap_or_else(|| panic!("no reason line for gen {gen}"));
        match reason {
            Some(r) => self.log[at].1 = format!("sink swap reason={r} gen={gen}"),
            None => {
                self.log.remove(at);
            }
        }
    }

    /// A compositor process's boot lines from `start`, and its alive lines until the next die.
    fn boot(&mut self, start: i64) {
        self.line(start + 200, "compositor started drivers=gst,cef");
        self.line(start + 1_500, "cef ready gen=1");
        self.swap(start + 2_400, 1, "first");
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
                let leaked = self.leaks.iter().filter(|&&d| d >= s && d <= at).count();
                lines.push((
                    at,
                    format!("cef health zygote_gone=- leaked_teardowns={leaked} gpu_process_deaths={deaths}"),
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
        let mut report = serde_json::to_value(&report).unwrap();
        if let Some(patch) = self.report_patch {
            patch(&mut report);
        }
        put("report.json", &report);
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
    // Every expression the verdict ran, its printed measures too, compiles: CI's jq (1.7.1) refused
    // one that jq 1.8 takes, and its error printed after a PASS (C14 fix round 2). A runtime error
    // (a measure over an event a red world lacks) is the red's own evidence, so it is allowed.
    assert!(
        !format!("{stdout}{stderr}").contains("compile error"),
        "{tag}: a jq expression does not compile"
    );
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

/// 12 pauses, one every 10 s from 5 s, each `pause_ms` long (the case's own rule: halfway between
/// slate_after_ms and input_stall_after_ms, from thresholds.json).
fn camera_flap_paused(pause_ms: i64) -> World {
    let mut w = World::new(125);
    for i in 0..12 {
        let p = w.at(5.0 + 10.0 * f64::from(i));
        let r = p + pause_ms;
        w.mark("pause", p, json!({"pause_ms": pause_ms}));
        w.mark("resume", r, Value::Null);
        w.camera_gaps.push((p + 30, r + 30));
    }
    w
}

fn camera_flap() -> World {
    let t = World::new(1);
    camera_flap_paused((t.ms("slate_after_ms") + t.ms("input_stall_after_ms")) / 2)
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
    // The first build's pause: input_stall_after_ms itself, the boundary the review's I5 moved off.
    let w = camera_flap_paused(World::new(1).ms("input_stall_after_ms"));
    assert_fails(
        &judge(&w, "camera-flap", "", "flap-boundary"),
        "flaps_seen",
        "flap-boundary",
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

/// `netem.sh stats out`'s line, with no backlog.
fn netem(present: bool, sent_bytes: u64, sent_pkts: u64, dropped: u64) -> Value {
    netem_queued(present, sent_bytes, sent_pkts, dropped, 0, 0)
}

/// `netem.sh stats out`'s line, with `backlog_bytes` in `backlog_pkts` queued.
fn netem_queued(
    present: bool,
    sent_bytes: u64,
    sent_pkts: u64,
    dropped: u64,
    backlog_bytes: u64,
    backlog_pkts: u64,
) -> Value {
    json!({"leg": "out", "present": present, "sent_bytes": sent_bytes, "sent_pkts": sent_pkts,
        "dropped": dropped, "backlog_bytes": backlog_bytes, "backlog_pkts": backlog_pkts})
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

/// The output's reconnect cycle under the fault (cases 4, 5 and 12): a sink built every
/// OUTPUT_CONNECT_TIMEOUT_MS + OUTPUT_BACKOFF_CAP_MS (CI: gen=3, 4, 5 seven seconds apart), each
/// failing its connect at the timeout with its own error line, numbered from `first_gen` and all
/// after `after`; the last is placed so the clear mark lands in `phase`: "pending" half a connect
/// timeout after it (no error of its own yet), "backoff" just after its error. The clear mark's
/// value names `driven`, the phase the drive meant (`clear_out_in_phase`).
fn with_cycle(w: &mut World, after: i64, first_gen: u64, phase: &str, driven: &str) {
    let connect = w.ms("output_connect_timeout_ms");
    let every = connect + w.ms("output_backoff_cap_ms");
    let clear = w.marks.iter().find(|m| m.0 == "clear").expect("a clear mark").1;
    let mut at = match phase {
        "pending" => clear - connect / 2,
        "backoff" => clear - connect - 200,
        _ => panic!("phase {phase}"),
    };
    let mut swaps = vec![];
    while at > after + 1_000 {
        swaps.push(at);
        at -= every;
    }
    swaps.reverse();
    let last = swaps.len().saturating_sub(1);
    for (i, at) in swaps.into_iter().enumerate() {
        let gen = first_gen + i as u64;
        // The first after the Connected sink failed follows its error; each next one a connect
        // that timed out under the fault.
        let reason = if gen == 2 { "error" } else { "connect_timeout" };
        w.swap(at, gen, reason);
        if i < last || phase == "backoff" {
            w.line(
                at + connect + 7,
                format!("output gen={gen} error: Failed to connect: Socket I/O timed out (gstrtmp2sink.c)"),
            );
        }
    }
    for m in &mut w.marks {
        if m.0 == "clear" {
            m.2 = json!({"phase": driven, "swap_ms": null, "swap_gen": null, "error_ms": null});
        }
    }
}

/// [`dest_cut`] with the reconnect cycle from the cut, the clear landing in `phase` as driven.
fn dest_disc(back_ms: i64, stats: Value, phase: &str) -> World {
    let mut w = dest_cut(back_ms, stats);
    let cut = w.at(20.0);
    with_cycle(&mut w, cut, 2, phase, phase);
    w
}

/// The variant a phase runs as (`dest_phase`, run-case.sh).
fn variant_of(phase: &str) -> &'static str {
    if phase == "backoff" {
        "backoff"
    } else {
        ""
    }
}

#[test]
fn dest_disconnect_recovers_and_reds_when_nothing_was_dropped_or_the_return_is_late() {
    // The re-review's I-5: each phase is its own leg, and the clear must land in it. With no
    // variant the last sink built is still connecting at the clear; with `backoff` it has failed.
    let connect = dest_cut(0, Value::Null).ms("output_connect_timeout_ms");
    let mut checked = 0;
    for phase in ["pending", "backoff"] {
        let tag = format!("disc-good-{phase}");
        let w = dest_disc(2_500, netem(true, 900_000, 700, 4_000), phase);
        let v = judge(&w, "dest-disconnect", variant_of(phase), &tag);
        assert_pass(&v, &tag);
        let m = &v.json["measures"]["clear_phase_driven"];
        assert_eq!(
            (m["phase"].as_str(), m["driven"].as_str()),
            (Some(phase), Some(phase)),
            "{tag}: {m}"
        );
        assert_eq!(m["connect_pending"], phase == "pending", "{tag}: {m}");
        assert!(m["swaps_in_fault"].as_u64().is_some_and(|n| n >= 3), "{tag}: {m}");
        checked += 1;
    }
    // A clear that missed its phase, or a drive that never found the cycle, reds the leg.
    for (tag, variant, landed, driven) in [
        ("disc-pending-landed-backoff", "", "backoff", "pending"),
        ("disc-backoff-landed-pending", "backoff", "pending", "backoff"),
        ("disc-drive-missed", "", "pending", "none"),
        ("disc-drive-said-backoff", "", "pending", "backoff"),
    ] {
        let mut w = dest_cut(2_500, netem(true, 900_000, 700, 4_000));
        let cut = w.at(20.0);
        with_cycle(&mut w, cut, 2, landed, driven);
        assert_fails(
            &judge(&w, "dest-disconnect", variant, tag),
            "clear_phase_driven",
            tag,
        );
        checked += 1;
    }
    // No sink built in the fault: the boot's swap, 50 s back, is no phase.
    let mut w = dest_cut(2_500, netem(true, 900_000, 700, 4_000));
    for m in &mut w.marks {
        if m.0 == "clear" {
            m.2 = json!({"phase": "pending"});
        }
    }
    let v = judge(&w, "dest-disconnect", "", "disc-no-cycle");
    assert_fails(&v, "clear_phase_driven", "disc-no-cycle");
    assert_eq!(v.json["measures"]["clear_phase_driven"]["swaps_in_fault"], 0);
    checked += 1;
    // The pending edge: the last sink built just under a connect timeout before the clear is
    // pending; one built a connect timeout before, with no error line, is neither phase.
    for (tag, ago, pass) in [
        ("disc-pending-edge", connect - 1, true),
        ("disc-neither", connect, false),
    ] {
        let mut w = dest_cut(2_500, netem(true, 900_000, 700, 4_000));
        let cut = w.at(20.0);
        let clear = w.marks.iter().find(|m| m.0 == "clear").unwrap().1;
        w.swap(cut + 6_000, 2, "error");
        w.line(
            cut + 6_000 + connect + 7,
            "output gen=2 error: Failed to connect: Socket I/O timed out (gstrtmp2sink.c)",
        );
        w.swap(clear - ago, 3, "connect_timeout");
        for m in &mut w.marks {
            if m.0 == "clear" {
                m.2 = json!({"phase": "pending"});
            }
        }
        let v = judge(&w, "dest-disconnect", "", tag);
        if pass {
            assert_pass(&v, tag);
        } else {
            assert_fails(&v, "clear_phase_driven", tag);
        }
        assert_eq!(
            v.json["measures"]["clear_phase_driven"]["swap_to_clear_ms"], ago,
            "{tag}"
        );
        checked += 1;
    }
    // A connect refused at once: the last sink built errored a second after its build, under a
    // connect timeout before the clear. That is the back-off, never pending.
    let mut w = dest_cut(2_500, netem(true, 900_000, 700, 4_000));
    let cut = w.at(20.0);
    let clear = w.marks.iter().find(|m| m.0 == "clear").unwrap().1;
    w.swap(cut + 6_000, 2, "error");
    w.line(
        cut + 6_000 + connect + 7,
        "output gen=2 error: Failed to connect: Socket I/O timed out (gstrtmp2sink.c)",
    );
    w.swap(clear - 2_000, 3, "connect_timeout");
    w.line(
        clear - 1_000,
        "output gen=3 error: Failed to connect: Connection refused (gstrtmp2sink.c)",
    );
    for m in &mut w.marks {
        if m.0 == "clear" {
            m.2 = json!({"phase": "backoff"});
        }
    }
    let v = judge(&w, "dest-disconnect", "backoff", "disc-backoff-refused");
    assert_pass(&v, "disc-backoff-refused");
    assert_eq!(
        v.json["measures"]["clear_phase_driven"]["phase"], "backoff",
        "disc-backoff-refused"
    );
    checked += 1;
    // The Connected sink errored at the cut and nothing was built after it: the boot's sink and its
    // error are no phase, whatever the drive said.
    let mut w = dest_cut(2_500, netem(true, 900_000, 700, 4_000));
    let cut = w.at(20.0);
    w.line(
        cut + 5_000,
        "output gen=1 error: Connection error: Socket I/O timed out (gstrtmp2sink.c)",
    );
    for m in &mut w.marks {
        if m.0 == "clear" {
            m.2 = json!({"phase": "backoff"});
        }
    }
    let v = judge(&w, "dest-disconnect", "backoff", "disc-no-rebuild");
    assert_fails(&v, "clear_phase_driven", "disc-no-rebuild");
    assert_eq!(v.json["measures"]["clear_phase_driven"]["swaps_in_fault"], 0);
    checked += 1;
    // Driven as the back-off, but the last sink built, a connect timeout and more before the clear,
    // logged no error: it never failed, so this is no back-off.
    let mut w = dest_cut(2_500, netem(true, 900_000, 700, 4_000));
    let cut = w.at(20.0);
    let clear = w.marks.iter().find(|m| m.0 == "clear").unwrap().1;
    w.swap(cut + 6_000, 2, "error");
    w.line(
        cut + 6_000 + connect + 7,
        "output gen=2 error: Failed to connect: Socket I/O timed out (gstrtmp2sink.c)",
    );
    w.swap(clear - connect - 200, 3, "connect_timeout");
    for m in &mut w.marks {
        if m.0 == "clear" {
            m.2 = json!({"phase": "backoff"});
        }
    }
    let v = judge(&w, "dest-disconnect", "backoff", "disc-backoff-no-error");
    assert_fails(&v, "clear_phase_driven", "disc-backoff-no-error");
    checked += 1;
    eprintln!("dest-disconnect phase sims checked: {checked}");
    assert_eq!(checked, 12);
    let w = dest_disc(2_500, netem(true, 900_000, 700, 0), "pending");
    assert_fails(
        &judge(&w, "dest-disconnect", "", "disc-nodrop"),
        "fault_applied",
        "disc-nodrop",
    );
    let w = dest_disc(7_000, netem(true, 900_000, 700, 4_000), "pending");
    assert_fails(
        &judge(&w, "dest-disconnect", "", "disc-late"),
        "output_reconnect",
        "disc-late",
    );
}

/// A stall from 20 s. The egress total grows once more after the cut (the kernel's send buffer
/// drains for under a second), then by `trickle` at each beat until the clear. With `stall_x` 1, the
/// stall is seen `output_stall_after_ms` after that drain, on the next output poll, and the next beat
/// carries the swap; 2 is a detector twice as slow (mutant 9, simulated); 0 swaps nothing.
fn dest_stall(trickle: i64, stall_x: i64, stats: Value) -> World {
    dest_stall_err(trickle, stall_x, stats, None)
}

/// [`dest_stall`], with the bus error `(gen, ms before the swap)` logged by sink `gen`: CI's case 5
/// and mutant 9's case 12, where rtmp2sink's own socket timeout failed sink 1 and the back-off
/// (`output_backoff_base_ms`) came before the swap (C14 fix round 3, item 2).
fn dest_stall_err(trickle: i64, stall_x: i64, stats: Value, err: Option<(u64, i64)>) -> World {
    dest_stall_in(trickle, stall_x, stats, err, "pending")
}

/// [`dest_stall_err`], the clear landing in `phase` of the reconnect cycle that follows the swap.
fn dest_stall_in(trickle: i64, stall_x: i64, stats: Value, err: Option<(u64, i64)>, phase: &str) -> World {
    let mut w = dest_cut(2_500, stats);
    let cut = w.at(20.0);
    let clear = w.at(50.0) + 50;
    let drained = cut + 800;
    let swaps = stall_x > 0;
    let swap = drained + stall_x * w.ms("output_stall_after_ms") + w.ms("output_progress_poll_ms");
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
        // The driver names the path: the back-off's build after sink 1's own error, else the rule.
        let reason = match err {
            Some((1, before)) if before > 0 => "error",
            _ => "stall",
        };
        w.swap(swap, 2, reason);
        with_cycle(&mut w, swap, 3, phase, phase);
    }
    if let Some((gen, before)) = err {
        w.line(
            swap - before,
            format!("output gen={gen} error: Connection error: Socket I/O timed out (gstrtmp2sink.c)"),
        );
    }
    w
}

/// Case 5 in CI (run 40045826841): at 1 kbit nothing was sent in 30 s, and the queue held the leg.
fn stalled() -> Value {
    netem_queued(true, 0, 0, 0, 130_862, 45)
}

#[test]
fn dest_stall_swaps_the_sink_and_reds_without_a_swap_or_a_slow_one() {
    let w = dest_stall(0, 1, stalled());
    assert_pass(&judge(&w, "dest-stall", "", "stall-good"), "stall-good");
    let w = dest_stall(0, 0, stalled());
    assert_fails(
        &judge(&w, "dest-stall", "", "stall-noswap"),
        "stall_swap",
        "stall-noswap",
    );
    // Mutant 9: output_stall_after_ms doubled. The first build's bound (a beat interval wide) let it
    // pass (the PR #52 review's I4).
    let w = dest_stall(0, 2, stalled());
    assert_fails(
        &judge(&w, "dest-stall", "", "stall-slow"),
        "stall_swap",
        "stall-slow",
    );
    // CI's case 5 (runs 40056474269, 40106770881): sink 1's own socket timeout, then the back-off,
    // then the swap, its reason line `error`. The coordinator's decision on Owed 87 (2026-10-24):
    // that recovery inside the spec's output budget is a pass here.
    let probe = dest_stall(0, 1, stalled());
    let base = probe.ms("output_backoff_base_ms");
    let budget = probe.ms("budget_output_reconnect_ms");
    let w = dest_stall_err(0, 1, stalled(), Some((1, base + 20)));
    let v = judge(&w, "dest-stall", "", "stall-socket");
    assert_pass(&v, "stall-socket");
    let m = &v.json["measures"]["stall_swap"];
    assert_eq!(
        (
            m["reason"].as_str(),
            m["cause"].as_str(),
            m["reasons_paired"].as_bool()
        ),
        (Some("error"), Some("output_error"), Some(true)),
        "stall-socket: {m}"
    );
    assert_eq!(m["error_to_swap_ms"], base + 20, "stall-socket: {m}");
    // The backoff leg: the same swap, the clear after the last sink's connect failed.
    let w = dest_stall_in(0, 1, stalled(), Some((1, base + 20)), "backoff");
    let v = judge(&w, "dest-stall", "backoff", "stall-socket-backoff");
    assert_pass(&v, "stall-socket-backoff");
    assert_eq!(v.json["measures"]["clear_phase_driven"]["phase"], "backoff");
    let v = judge(&w, "dest-stall", "", "stall-backoff-as-pending");
    assert_fails(&v, "clear_phase_driven", "stall-backoff-as-pending");
    // The error path's reds: a swap before the back-off could end (no product can beat it), one
    // past the spec's 5 s after the error, and a socket timeout twice as slow as the rule's budget.
    let mut checked = 0;
    for (tag, stall_x, before) in [
        ("stall-err-early", 1, base - 1),
        ("stall-err-late", 1, budget + 1),
        ("stall-err-slow", 2, base + 20),
    ] {
        let w = dest_stall_err(0, stall_x, stalled(), Some((1, before)));
        assert_fails(&judge(&w, "dest-stall", "", tag), "stall_swap", tag);
        checked += 1;
    }
    // The edges: the back-off itself and the budget itself pass.
    for (tag, before) in [("stall-err-floor", base), ("stall-err-budget", budget)] {
        let w = dest_stall_err(0, 1, stalled(), Some((1, before)));
        assert_pass(&judge(&w, "dest-stall", "", tag), tag);
        checked += 1;
    }
    // The reason line is read, never assumed: it must name the path the error line shows, name a
    // path the case accepts, and be there at all for every swap.
    for (tag, err, reason) in [
        ("stall-says-error", None, Some("error")),
        ("stall-says-stall", Some((1, base + 20)), Some("stall")),
        (
            "stall-says-timeout",
            Some((1, base + 20)),
            Some("connect_timeout"),
        ),
        ("stall-no-reason", None, None),
    ] {
        let mut w = dest_stall_err(0, 1, stalled(), err);
        w.reason_line(2, reason);
        assert_fails(&judge(&w, "dest-stall", "", tag), "stall_swap", tag);
        checked += 1;
    }
    // The reason lines must number the swap lines: one missing for the boot's swap, one more than
    // the swaps (a reason logged and its swap never), and as many but one naming the wrong swap.
    for (tag, change) in [
        ("stall-unpaired", 0),
        ("stall-extra-reason", 1),
        ("stall-wrong-gen", 2),
    ] {
        let mut w = dest_stall(0, 1, stalled());
        match change {
            0 => w.reason_line(1, None),
            1 => {
                let at = w.at(70.0);
                w.line(at, "sink swap reason=stall gen=3");
            }
            _ => w.relabel_reason(1, 2),
        }
        let v = judge(&w, "dest-stall", "", tag);
        assert_fails(&v, "stall_swap", tag);
        assert_eq!(v.json["measures"]["stall_swap"]["reasons_paired"], false, "{tag}");
        checked += 1;
    }
    // A report from before the reason was read reads reason "unknown", a red.
    let mut w = dest_stall(0, 1, stalled());
    w.report_patch = Some(|r| {
        r["log"]
            .as_object_mut()
            .unwrap()
            .remove("sink_swap_reasons")
            .expect("the field to drop");
    });
    let v = judge(&w, "dest-stall", "", "stall-noreasons");
    assert_fails(&v, "stall_swap", "stall-noreasons");
    assert_eq!(v.json["measures"]["stall_swap"]["reason"], "unknown");
    checked += 1;
    eprintln!("dest-stall reason and error-path sims checked: {checked}");
    assert_eq!(checked, 13);
    // The re-review's m-3: a report from before fix round 3 has no `output_errors`, which reads as
    // cause "unknown" (a red), never as "no error" and so the stall rule's.
    let mut w = dest_stall(0, 1, stalled());
    w.report_patch = Some(|r| {
        r["log"]
            .as_object_mut()
            .unwrap()
            .remove("output_errors")
            .expect("the field to drop");
    });
    let v = judge(&w, "dest-stall", "", "stall-noerrfield");
    assert_fails(&v, "stall_swap", "stall-noerrfield");
    assert_eq!(
        v.json["measures"]["stall_swap"]["cause"], "unknown",
        "stall-noerrfield"
    );
    // An error of the NEW sink (its first connect failing under the stall) is no cause of the
    // swap that built it, and neither is the outgoing sink's error logged after the swap.
    for (tag, err) in [("stall-newerr", (2, -2_000)), ("stall-lateerr", (1, -1))] {
        let w = dest_stall_err(0, 1, stalled(), Some(err));
        assert_pass(&judge(&w, "dest-stall", "", tag), tag);
    }
    // The rate limit held nothing: no backlog, or no netem qdisc at all.
    for (tag, stats) in [
        ("stall-nobacklog", netem(true, 0, 0, 0)),
        ("stall-noqdisc", netem(false, 0, 0, 0)),
    ] {
        let w = dest_stall(0, 1, stats);
        assert_fails(&judge(&w, "dest-stall", "", tag), "fault_applied", tag);
    }
}

#[test]
fn dest_trickle_swaps_under_a_trickle_and_reds_when_nothing_trickled() {
    // Case 12's qdisc in CI: 16 packets through at 8 kbit, one more queued.
    let trickling = || netem_queued(true, 19_271, 16, 0, 65_536, 1);
    let w = dest_stall(5_000, 1, trickling());
    assert_pass(&judge(&w, "dest-trickle", "", "trickle-good"), "trickle-good");
    // CI's case 12 (run 40106770881): sink 1's socket timeout first, the reason line `error`, and
    // the back-off's swap. A pass by the coordinator's decision on Owed 87; mutant 9 (the threshold
    // at 20 s), which swapped the same way, is judged on case 17 now.
    let base = w.ms("output_backoff_base_ms");
    let w = dest_stall_err(5_000, 1, trickling(), Some((1, base + 20)));
    assert_pass(&judge(&w, "dest-trickle", "", "trickle-socket"), "trickle-socket");
    let w = dest_stall_in(5_000, 1, trickling(), Some((1, base + 20)), "backoff");
    assert_pass(
        &judge(&w, "dest-trickle", "backoff", "trickle-socket-backoff"),
        "trickle-socket-backoff",
    );
    let w = dest_stall_err(5_000, 1, trickling(), Some((1, base - 1)));
    assert_fails(
        &judge(&w, "dest-trickle", "", "trickle-err-early"),
        "stall_swap",
        "trickle-err-early",
    );
    // Mutant 5: the netem line commented out: nothing trickles (a plain stall), and the qdisc is a
    // pass-through that never existed.
    let w = dest_stall(0, 1, netem(false, 0, 0, 0));
    assert_fails(
        &judge(&w, "dest-trickle", "", "trickle-none"),
        "trickled",
        "trickle-none",
    );
    // Case 5's stall under case 12's name: the qdisc sent nothing.
    let w = dest_stall(0, 1, stalled());
    assert_fails(
        &judge(&w, "dest-trickle", "", "trickle-stalled"),
        "trickled",
        "trickle-stalled",
    );
    // One packet through: not a trickle the bytes can be said to have made.
    let w = dest_stall(0, 1, netem_queued(true, 1_448, 1, 0, 130_862, 45));
    assert_fails(
        &judge(&w, "dest-trickle", "", "trickle-onepkt"),
        "trickled",
        "trickle-onepkt",
    );
    // A trickle with no swap at all.
    let w = dest_stall(5_000, 0, trickling());
    assert_fails(
        &judge(&w, "dest-trickle", "", "trickle-noswap"),
        "trickled",
        "trickle-noswap",
    );
}

/// Case 17: [`dest_stall_in`]'s stall under a trickle, with the drive's send-buffer marks: the cap
/// compose set (`wanted`), what the namespace read back (`read`), each RTMP socket's buffer (`tb`)
/// before the cut, and each one's Send-Q 3 s into the fault (`queue`). With the queue under the
/// cap this is the world the case demands, never seen on CI; CI's world (3 of 3 runs, PR #65) is
/// `rule-ci`: the queue at twice the cap and the sink's own error first.
fn dest_stall_rule(
    stall_x: i64,
    err: Option<(u64, i64)>,
    read: Value,
    tb: Value,
    queue: Value,
    stats: Value,
) -> World {
    // The total stays flat between beats: a message that cannot leave counts nothing.
    let mut w = dest_stall_in(0, stall_x, stats, err, "pending");
    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(repo().join("contracts/fixtures/relay-job.v1/session-composed-720p.json"))
            .unwrap(),
    )
    .unwrap();
    let frame_bits =
        session["output"]["videoKbps"].as_i64().unwrap() * 1000 / session["output"]["fps"].as_i64().unwrap();
    let rate = frame_bits * 1000 / (2 * w.ms("output_stall_after_ms"));
    let at = w.at(0.5);
    w.mark(
        "send_buffer",
        at,
        json!({"wanted": [4096, 8192, 8192], "read": read,
            "before_cut": {"sockets": tb.as_array().map_or(0, Vec::len), "tb": tb, "send_q": [0]}}),
    );
    w.mark(
        "rate",
        at,
        json!({"frame_bits": frame_bits, "bit_per_s": rate, "drain_x": 2}),
    );
    let at = w.at(23.0);
    let sockets = queue.as_array().map_or(0, Vec::len);
    w.mark(
        "sockets_in_fault",
        at,
        json!({"sockets": sockets, "tb": vec![8192; sockets], "send_q": queue}),
    );
    w
}

#[test]
fn dest_stall_rule_swaps_by_the_rule_and_reds_on_the_sockets_error_a_slow_rule_or_no_cap() {
    let capped = || json!([4096, 8192, 8192]);
    let trickling = || netem_queued(true, 4_344, 6, 0, 8_688, 6);
    let held = || json!([6_400]);
    let w = dest_stall_rule(1, None, capped(), json!([8192]), held(), trickling());
    let v = judge(&w, "dest-stall-rule", "", "rule-good");
    assert_pass(&v, "rule-good");
    let m = &v.json["measures"]["stall_swap"];
    assert_eq!(
        (m["reason"].as_str(), m["cause"].as_str()),
        (Some("stall"), Some("stall")),
        "rule-good: {m}"
    );
    assert_eq!(
        (m["stall_path_passes"].as_bool(), m["error_path_passes"].as_bool()),
        (Some(true), Some(false)),
        "rule-good: {m}"
    );
    assert_eq!(m["queue_past_cap"], false, "rule-good: the queue held: {m}");
    let base = w.ms("output_backoff_base_ms");
    let mut checked = 1;
    // CI's world (runs 40121216406, 40121220366 and m9's baseline): the cap set as asked, the queue
    // at 16 810 B past it, and the sink's own socket timeout 515 ms before the swap. Both the queue
    // and the swap red, and the error path held in full (Owed 87's evidence).
    let w = dest_stall_rule(
        1,
        Some((1, base + 15)),
        capped(),
        json!([8192]),
        json!([16_810]),
        trickling(),
    );
    let v = judge(&w, "dest-stall-rule", "", "rule-ci");
    assert_fails(&v, "stall_swap", "rule-ci");
    assert_fails(&v, "buffer_capped", "rule-ci");
    let m = &v.json["measures"]["stall_swap"];
    assert_eq!(
        (
            m["reason"].as_str(),
            m["error_path_passes"].as_bool(),
            m["stall_path_passes"].as_bool(),
            m["queue_past_cap"].as_bool()
        ),
        (Some("error"), Some(true), Some(false), Some(true)),
        "rule-ci: {m}"
    );
    checked += 1;
    // The socket timed out first with the queue held (the cases 5 and 12 path): a pass there, a red
    // here. Mutant 9 (output_stall_after_ms at 20 s, four thresholds): the swap far past the
    // budget. And no swap at all in the fault.
    for (tag, stall_x, err) in [
        ("rule-socket", 1, Some((1, base + 20))),
        ("rule-slow", 4, None),
        ("rule-noswap", 0, None),
    ] {
        let w = dest_stall_rule(stall_x, err, capped(), json!([8192]), held(), trickling());
        assert_fails(&judge(&w, "dest-stall-rule", "", tag), "stall_swap", tag);
        checked += 1;
    }
    // A rule that fires early (a logic bug, not a value: 2 s after the last growth, within the
    // bound of it), 3 s after the cut: the bytes flowed at full rate up to the cut, so the rule
    // cannot have seen 5 s of stall (the PR #65 review's m-3).
    let mut w = dest_stall_rule(0, None, capped(), json!([8192]), held(), trickling());
    let cut = w.at(20.0);
    w.swap(cut + 3_000, 2, "stall");
    w.beats_in(cut + 3_000, cut + 60_000, |b| b["sinkSwaps"] = json!(1));
    let v = judge(&w, "dest-stall-rule", "", "rule-early");
    assert_fails(&v, "stall_swap", "rule-early");
    let m = &v.json["measures"]["stall_swap"];
    assert!(
        m["detect_ms"]
            .as_i64()
            .is_some_and(|d| d <= m["bound_ms"].as_i64().unwrap())
            && m["reasons_paired"] == true
            && m["swap_beat_ms"].is_i64(),
        "rule-early must red on the floor alone: {m}"
    );
    checked += 1;
    // The cap never applied: the namespace reads the kernel's default, a socket's buffer is past
    // the cap, no RTMP socket was seen at all, or ss could not read a buffer.
    // Each with the queue past the cap, which is not Owed 87's evidence when the cap was not set
    // as asked.
    for (tag, read, tb) in [
        ("rule-default", json!([4096, 16384, 4194304]), json!([8192])),
        ("rule-big-socket", capped(), json!([87_040])),
        ("rule-no-socket", capped(), json!([])),
        ("rule-unread-socket", capped(), json!([null])),
    ] {
        let w = dest_stall_rule(1, None, read, tb, json!([16_810]), trickling());
        let v = judge(&w, "dest-stall-rule", "", tag);
        assert_fails(&v, "buffer_capped", tag);
        assert_eq!(
            v.json["measures"]["stall_swap"]["queue_past_cap"], false,
            "{tag}: the cap not set as asked"
        );
        checked += 1;
    }
    // The cap set as asked, and the queue not held by it (the PR #65 review's I-1): past it, not
    // read, or no socket seen in the fault.
    // Only a queue read past the cap is Owed 87's evidence.
    for (tag, queue, past) in [
        ("rule-queue-over", json!([8_193]), true),
        ("rule-queue-unread", json!([null]), false),
        ("rule-queue-none", json!([]), false),
    ] {
        let w = dest_stall_rule(1, None, capped(), json!([8192]), queue, trickling());
        let v = judge(&w, "dest-stall-rule", "", tag);
        assert_fails(&v, "buffer_capped", tag);
        assert_eq!(
            v.json["measures"]["stall_swap"]["queue_past_cap"],
            json!(past),
            "{tag}"
        );
        checked += 1;
    }
    // The trickle's measured rate, netem's bytes over the 30 s fault: one frame per stall threshold
    // is 6 frames' bytes here. Unlimited (netem's rate 0 sends everything) and 5 % over red; 5 %
    // under passes.
    let frame_bytes = w.marks.iter().find(|m| m.0 == "rate").unwrap().2["frame_bits"]
        .as_i64()
        .unwrap()
        / 8;
    let frames = (w.at(50.0) - w.at(20.0)) / w.ms("output_stall_after_ms");
    let per_threshold = |x: f64| ((frame_bytes * frames) as f64 * x) as u64;
    for (tag, sent, red) in [
        ("rule-unlimited", 15_000_000, true),
        ("rule-fast", per_threshold(1.05), true),
        ("rule-edge", per_threshold(0.95), false),
    ] {
        let w = dest_stall_rule(
            1,
            None,
            capped(),
            json!([8192]),
            held(),
            netem_queued(true, sent, 900, 0, 8_688, 6),
        );
        let v = judge(&w, "dest-stall-rule", "", tag);
        if red {
            assert_fails(&v, "trickled", tag);
        } else {
            assert_pass(&v, tag);
        }
        checked += 1;
    }
    // Nothing trickled: a plain stall (case 5's), not this case.
    let w = dest_stall_rule(1, None, capped(), json!([8192]), held(), stalled());
    assert_fails(
        &judge(&w, "dest-stall-rule", "", "rule-stalled"),
        "trickled",
        "rule-stalled",
    );
    checked += 1;
    eprintln!("dest-stall-rule sims checked: {checked}");
    assert_eq!(checked, 17);
}

/// What the starvation did to the compositor (case 6).
#[derive(Clone, Copy, PartialEq)]
enum Felt {
    /// Nothing: droppedFrames 0, rung full and encoderSpeed 1.0 throughout (CI's first build, a
    /// sibling container's burn).
    No,
    /// The encoder fell behind realtime, and the ladder never moved: droppedFrames 0, rung full
    /// (Owed 86, as CI read it: encoderSpeed 0.90 to 1.07, every beat healthy).
    Unread,
    /// The overruns the ladder's step needs, and no step (the ladder never moves).
    Drops,
    /// A step to overlay_half_rate with no overrun counted (droppedFrames never rose).
    Uncounted,
    /// Overruns and the step to overlay_half_rate.
    Step,
    /// As `Step`, and the step absorbed it at once: every beat at encoderSpeed 1.0, the clock whole.
    Absorbed,
}

/// The quota at three quarters of the compositor's 1.4-CPU demand from 20 s for 40 s, put back with
/// `done` (the drive's `stress_done`).
fn cpu_starved(done: bool, rung_back: bool, felt: Felt) -> World {
    let mut w = World::new(80);
    let (a, b) = (w.at(20.0), w.at(60.0));
    w.mark(
        "stress",
        a,
        json!({"demand_cpus": 1.4, "quota_cpus": 1.05, "cpus": 4.0, "share": 0.75, "seconds": 40}),
    );
    if done {
        w.mark("stress_done", b, json!({"cpus": 4.0}));
    }
    let step = w.ms("ladder_overruns_to_step");
    let end = w.end;
    // droppedFrames is the session's overrun count (cumulative, the ladder's `overruns`).
    let dropped = match felt {
        Felt::No | Felt::Unread | Felt::Uncounted => 0,
        Felt::Drops => step,
        Felt::Step | Felt::Absorbed => 40,
    };
    w.beats_in(a + 1_000, end + 1, |x| x["droppedFrames"] = json!(dropped));
    match felt {
        Felt::No | Felt::Absorbed => {}
        Felt::Unread => w.beats_in(a + 6_000, a + 6_001 + w.ms("heartbeat_interval_ms"), |x| {
            x["encoderSpeed"] = json!(0.9)
        }),
        _ => w.beats_in(a, b, |x| x["encoderSpeed"] = json!(0.8)),
    }
    if matches!(felt, Felt::Step | Felt::Uncounted | Felt::Absorbed) {
        w.beats_in(a + 6_000, end + 1, |x| x["rung"] = json!("overlay_half_rate"));
        if rung_back {
            w.beats_in(b + 5_000, end + 1, |x| x["rung"] = json!("full"));
        }
    }
    w
}

#[test]
fn cpu_starved_steps_the_ladder_and_holds_its_frame_rate_and_reds_when_nothing_moved() {
    let v = judge(
        &cpu_starved(true, false, Felt::Step),
        "cpu-starved",
        "",
        "cpu-good",
    );
    assert_pass(&v, "cpu-good");
    assert_eq!(
        v.json["measures"]["starvation_applied"]["applied"], true,
        "cpu-good"
    );
    // A ladder that steps inside its 1 s window keeps every beat at realtime and the clock whole:
    // the overruns it counted are the starvation's witness, and the case passes.
    let v = judge(
        &cpu_starved(true, false, Felt::Absorbed),
        "cpu-starved",
        "",
        "cpu-absorbed",
    );
    assert_pass(&v, "cpu-absorbed");
    assert_eq!(
        v.json["measures"]["starvation_applied"]["min_encoder_speed"], 1.0,
        "cpu-absorbed"
    );
    let w = cpu_starved(false, false, Felt::Step);
    assert_fails(
        &judge(&w, "cpu-starved", "", "cpu-notdone"),
        "stress_ran",
        "cpu-notdone",
    );
    // The quota put back after 10 s, set no lower than the demand, or put back to another limit.
    let mut checked = 0;
    for (tag, edit) in [
        (
            "cpu-short",
            (|w: &mut World| {
                let at = w.at(30.0);
                w.marks.iter_mut().find(|m| m.0 == "stress_done").unwrap().1 = at;
            }) as fn(&mut World),
        ),
        ("cpu-noquota", |w: &mut World| {
            w.marks.iter_mut().find(|m| m.0 == "stress").unwrap().2["quota_cpus"] = json!(1.4);
        }),
        ("cpu-norestore", |w: &mut World| {
            w.marks.iter_mut().find(|m| m.0 == "stress_done").unwrap().2["cpus"] = json!(1.05);
        }),
    ] {
        let mut w = cpu_starved(true, false, Felt::Step);
        edit(&mut w);
        assert_fails(&judge(&w, "cpu-starved", "", tag), "stress_ran", tag);
        checked += 1;
    }
    assert_eq!(checked, 3);
    // The drive could not read the demand: no quota, the refusal shown.
    let mut w = cpu_starved(false, false, Felt::Step);
    w.marks.retain(|m| m.0 != "stress");
    let at = w.at(20.0);
    w.mark(
        "stress_refused",
        at,
        json!({"reason": "the cgroup CPU time did not read twice"}),
    );
    let v = judge(&w, "cpu-starved", "", "cpu-refused");
    assert_fails(&v, "stress_ran", "cpu-refused");
    assert_fails(&v, "starvation_applied", "cpu-refused");
    assert!(
        v.json["measures"]["stress_ran"]["refused"]["reason"].is_string(),
        "cpu-refused"
    );
    // A window with no start (an out/ that lost the `stress` mark): no window, so nothing in it was
    // starved, whatever the beats before its end read.
    let mut w = cpu_starved(true, false, Felt::Step);
    w.marks.retain(|m| m.0 != "stress");
    let v = judge(&w, "cpu-starved", "", "cpu-nostart");
    assert_fails(&v, "starvation_applied", "cpu-nostart");
    assert_eq!(
        v.json["measures"]["starvation_applied"]["applied"], false,
        "cpu-nostart"
    );
    let w = cpu_starved(true, true, Felt::Step);
    assert_fails(
        &judge(&w, "cpu-starved", "", "cpu-climbs"),
        "rung_holds",
        "cpu-climbs",
    );
    // CI's first build: the stress never reached the compositor (the PR #52 review's I3). The
    // re-review's I-4: that is the drive's own red (`starvation_applied`), and `ladder_moved` is
    // never judged a pass on a window nothing starved.
    let v = judge(
        &cpu_starved(true, false, Felt::No),
        "cpu-starved",
        "",
        "cpu-unfelt",
    );
    assert_fails(&v, "starvation_applied", "cpu-unfelt");
    assert_fails(&v, "ladder_moved", "cpu-unfelt");
    assert_eq!(v.json["measures"]["ladder_moved"]["starved"], false, "cpu-unfelt");
    // The window's frame rate alone says starved (2 s of the 40 lost, encoderSpeed 1.0 throughout).
    let mut w = cpu_starved(true, false, Felt::No);
    let a = w.at(30.0);
    w.outages.push((a, a + 2_000));
    let v = judge(&w, "cpu-starved", "", "cpu-fps-only");
    assert!(
        !v.json["failed"]
            .as_array()
            .unwrap()
            .contains(&json!("starvation_applied")),
        "cpu-fps-only: {:?}",
        v.json["measures"]["starvation_applied"]
    );
    assert_fails(&v, "window_fps", "cpu-fps-only");
    // Owed 86 as CI read it: starved (one beat's encoderSpeed 0.90), the ladder never moved. The
    // measures carry what Owed 86's signature reads (chaos.json): starved, no rise, no step.
    let v = judge(
        &cpu_starved(true, false, Felt::Unread),
        "cpu-starved",
        "",
        "cpu-owed86",
    );
    assert_fails(&v, "ladder_moved", "cpu-owed86");
    let m = &v.json["measures"];
    assert_eq!(m["starvation_applied"]["applied"], true, "cpu-owed86: {m}");
    assert_eq!(m["ladder_moved"]["dropped_rise"], 0, "cpu-owed86: {m}");
    assert_eq!(m["ladder_moved"]["half_rate_beats"], 0, "cpu-owed86: {m}");
    // Each half of `ladder_moved` alone: overruns enough for a step and no step, then a step with no
    // overrun counted.
    for (tag, felt) in [("cpu-nostep", Felt::Drops), ("cpu-uncounted", Felt::Uncounted)] {
        let w = cpu_starved(true, false, felt);
        assert_fails(&judge(&w, "cpu-starved", "", tag), "ladder_moved", tag);
    }
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

/// `in-container.sh kill-cef` at 20 s: its targets (`kill`, printed before the kill) and, when the
/// exec lived to print it, its status (`kill_rc`).
fn cef_killed_rc(kill: Value, rc: Option<i64>, code: i64) -> World {
    cef_killed_with(kill, rc, code, None)
}

/// As `cef_killed_rc`; with `detail`, the process sends C11b's final beat (`exitReason` "restart")
/// before it dies.
fn cef_killed_with(kill: Value, rc: Option<i64>, code: i64, detail: Option<&str>) -> World {
    let mut w = World::new(70);
    let k = w.at(20.0);
    let mut v = kill;
    v["at_ms"] = json!(k);
    w.mark("kill", k, v);
    if let Some(rc) = rc {
        w.mark("kill_rc", k + 5, json!({"rc_at_ms": k + 5, "rc": rc}));
    }
    w.restart(k + 1_200, code, detail);
    w
}

fn cef_killed(kill: Value, code: i64) -> World {
    cef_killed_rc(kill, Some(0), code)
}

#[test]
fn cef_killed_restarts_the_process_and_reds_when_the_kill_missed_or_the_death_is_no_restart() {
    let good = || json!({"killed": [201, 202, 203], "types": ["zygote", "zygote", "gpu-process"]});
    let v = judge(&cef_killed(good(), 133), "cef-killed", "", "cef-good");
    assert_pass(&v, "cef-good");
    assert_eq!(
        graphics_failed(&v),
        json!(0),
        "cef-good: Owed 85's evidence is absent"
    );
    // The container died under the exec before its status line: the targets alone.
    assert_pass(
        &judge(
            &cef_killed_rc(good(), None, 133),
            "cef-killed",
            "",
            "cef-good-norc",
        ),
        "cef-good-norc",
    );
    // Mutant 8 (CI's first build, the PR #52 review's C1): a NUL-only split finds no CEF child (its
    // failed status aside: the empty target list alone reds).
    let w = cef_killed_rc(json!({"killed": [], "types": []}), None, 133);
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-missed"),
        "kill_applied",
        "cef-missed",
    );
    let w = cef_killed_rc(good(), Some(1), 133);
    assert_fails(&judge(&w, "cef-killed", "", "cef-rc"), "kill_applied", "cef-rc");
    // C14 fix round 2 (CI, 1 run of 6): C11b's zygote verdict came before Chromium's abort, and the
    // process restarted in place, as the owner's ruling "B" asks for a dead zygote (Owed 56).
    let w = cef_killed_with(
        good(),
        Some(0),
        75,
        Some("restart in place: a CEF zygote is gone (gen 1)"),
    );
    assert_pass(&judge(&w, "cef-killed", "", "cef-inplace"), "cef-inplace");
    // A 75 with no restart beat is not C11b's restart.
    let w = cef_killed(good(), 75);
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-75-nobeat"),
        "died_to_restart",
        "cef-75-nobeat",
    );
    let w = cef_killed(good(), 0);
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-exit0"),
        "died_to_restart",
        "cef-exit0",
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
    // Mutant 3 (the review's m3): the new process logs ready, but its first beat is not ready.
    let mut w = cef_killed(good(), 133);
    let start = w.starts()[1];
    let first = w
        .beats
        .iter()
        .map(|b| b.0)
        .filter(|&at| at > start + 1_500)
        .min()
        .unwrap();
    w.beats_in(first, first + 1, |x| x["graphics"] = json!("starting"));
    assert_fails(
        &judge(&w, "cef-killed", "", "cef-notready"),
        "cef_ready_after_restart",
        "cef-notready",
    );
    // Fix round 3, item 3: CI's runs 1 and 3 (40056474269): the restart, and no overlay after it.
    let mut w = cef_killed(good(), 133);
    let k = w.at(20.0);
    overlay_lost_after_die(&mut w, k, None);
    let v = judge(&w, "cef-killed", "", "cef-nooverlay");
    assert_fails(&v, "overlay_back", "cef-nooverlay");
    assert_eq!(
        graphics_failed(&v),
        json!(1),
        "cef-nooverlay: Owed 85's evidence is present"
    );
    // The re-review's m-2: the budget runs from the output's last packet (spec §6's "output back"),
    // which is before the die. An overlay back 100 ms inside the budget from the die is outside it
    // from that packet.
    let mut w = cef_killed(good(), 133);
    overlay_lost_after_die(&mut w, k, Some(-100));
    let v = judge(&w, "cef-killed", "", "cef-overlay-fromdie");
    assert_fails(&v, "overlay_back", "cef-overlay-fromdie");
    assert_eq!(
        graphics_failed(&v),
        json!(0),
        "cef-overlay-fromdie: a late overlay is not Owed 85's"
    );
    // And 150 ms inside the budget from that packet passes: `World::restart`'s outage begins
    // 200 ms before the die, so that is 350 ms inside the die's.
    let mut w = cef_killed(good(), 133);
    overlay_lost_after_die(&mut w, k, Some(-350));
    assert_pass(
        &judge(&w, "cef-killed", "", "cef-overlay-inbudget"),
        "cef-overlay-inbudget",
    );
}

/// The kill's status line reads a failed `kill`.
fn fail_kill_rc(w: &mut World) {
    let m = w
        .marks
        .iter_mut()
        .find(|m| m.0 == "kill_rc")
        .expect("a kill_rc mark");
    m.2["rc"] = json!(1);
}

fn process_killed(pid: Value, restart_delay: i64) -> World {
    let mut w = World::new(70);
    let k = w.at(20.0);
    w.mark("netem_before", w.at(18.0), netem(true, 9_000_000, 8_000, 0));
    w.mark("kill", k, json!({"at_ms": k, "pid": pid}));
    w.mark("kill_rc", k + 5, json!({"rc_at_ms": k + 5, "rc": 0}));
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
    let mut w = process_killed(json!(31), 0);
    fail_kill_rc(&mut w);
    assert_fails(
        &judge(&w, "process-killed", "", "proc-rc"),
        "kill_applied",
        "proc-rc",
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
    if !detected {
        let mut w = cef_hung_base();
        let (hang, end) = (w.page_hang.unwrap(), w.end);
        w.overlay_hold.push((hang, end + 1));
        return w;
    }
    // CI run 40045826841: the log 10 012 ms after the last alive, the strip 6 ms after the log.
    let hang_after = cef_hung_base().ms("cef_hang_after_ms");
    cef_hung_at(hang_after + 12, 6)
}

/// The hung page, before any hide: the hang at 20.7 s, and the page's last alive before it.
fn cef_hung_base() -> World {
    let mut w = World::new(60);
    let armed = w.at(20.0);
    w.mark("armed", armed, json!({"page_hang_armed": true}));
    let hang = armed + 700;
    w.page_hang = Some(hang);
    w.control_state["page_hang_served_ms"] = json!(hang - 50);
    w.line(hang, "gen=1: page console: hang starts");
    w
}

/// The page's last `cef alive` line before the hang (`log_text` writes one every cef_alive_every_ms
/// from the boot's ready).
fn last_alive_before(w: &World, hang: i64) -> i64 {
    let alive = w.ms("cef_alive_every_ms");
    let mut last = w.starts()[0] + 1_500 + alive;
    while last + alive <= hang {
        last += alive;
    }
    last
}

/// The hide logged `log_ms` after the last alive, and the strip's gap from the first programme frame
/// at or after `strip_ms` past that line (CI: +6 ms; MediaMTX's segment names jitter ±17 ms).
fn cef_hung_at(log_ms: i64, strip_ms: i64) -> World {
    let mut w = cef_hung_base();
    let hang = w.page_hang.unwrap();
    let alive = w.ms("cef_alive_every_ms");
    let hide = last_alive_before(&w, hang) + log_ms;
    let gap = hide + strip_ms;
    let ready = hide.max(gap) + 2_500;
    w.overlay_hold.push((hang, gap));
    w.overlay_none.push((gap, ready + 300));
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
    // Mutant 4's gap (the review's m4): no beat in the span, so `all()` would pass on nothing.
    let mut w = cef_hung(true);
    let hang = w.page_hang.unwrap();
    w.beats.retain(|(at, b)| *at < hang || b["state"] != "playing");
    assert_fails(
        &judge(&w, "cef-hung", "", "hung-nobeats"),
        "beats_never_failed",
        "hung-nobeats",
    );
    // Fix round 4 (the re-review's I-1): the rule is judged on the log, the strip against the log.
    // PR run 40065135366: the log 10 031 ms after the last alive (the first tick at or after the
    // bound) and the strip 6 ms after it. The brief's strip bound (10 033.3) redded it.
    let hang_after = w.ms("cef_hang_after_ms");
    let period = PERIOD_MS;
    let v = judge(&cef_hung_at(hang_after + 31, 6), "cef-hung", "", "hung-ci-pr");
    assert_pass(&v, "hung-ci-pr");
    let shown = &v.json["measures"]["detect_ms"];
    assert_eq!(
        shown["log_from_last_alive_ms"],
        hang_after + 31,
        "hung-ci-pr: {shown}"
    );
    // Each side of the log's window, by more than its tick (a beat late is 2 000 ms), and the strip
    // off the logged hide by more than spec §6's frame-to-encoder budget, either way.
    let budget = w.ms("budget_frame_to_encoder_ms");
    for (tag, log_ms, strip_ms) in [
        ("hung-late", hang_after + (3.0 * period).ceil() as i64, 6),
        ("hung-early", hang_after - (2.0 * period).ceil() as i64, 6),
        ("hung-strip-late", hang_after + 12, budget + 40),
        ("hung-strip-early", hang_after + 12, -(budget + 40)),
    ] {
        assert_fails(
            &judge(&cef_hung_at(log_ms, strip_ms), "cef-hung", "", tag),
            "detect_ms",
            tag,
        );
    }
}

/// Owed 85's restart (C14 fix round 3, item 3): the process comes back, CEF never does, so the
/// overlay strip reads nothing from the first die after `after` on (`late_ms` None), or reads again
/// only `late_ms` after BUDGET_PROCESS_RESTART_OUTPUT_MS from that die. Never back, the restarted
/// graphics gives up as CI's Owed 85 runs logged it (fix round 4, I-3): `overlay hidden
/// reason=failed` once CEF_FAIL_COUNT builds failed, a back-off base and its double after the
/// start's first build.
fn overlay_lost_after_die(w: &mut World, after: i64, late_ms: Option<i64>) {
    let die = w
        .events
        .iter()
        .filter(|e| e.1 == "die" && e.0 > after)
        .map(|e| e.0)
        .min()
        .expect("a die after the fault");
    let to = match late_ms {
        Some(l) => die + w.ms("budget_process_restart_output_ms") + l,
        None => {
            let start = w
                .starts()
                .into_iter()
                .find(|&s| s >= die)
                .expect("a start after the die");
            let base = w.ms("cef_backoff_base_ms");
            let gen = w.ms("cef_fail_count");
            w.line(
                start + 100 + 3 * base,
                format!("overlay hidden reason=failed gen={gen}"),
            );
            w.end + 60_000
        }
    };
    w.overlay_none.push((die, to));
}

/// The `graphics_failed_after_start` measure a verdict recorded: Owed 85's evidence (its chaos.json
/// signature left with its tags when PR #54 fixed it).
fn graphics_failed(v: &Verdict) -> Value {
    v.json["measures"]["graphics_failed_after_start"].clone()
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
            "want": want, "children": [52], "child_types": [child]}),
    );
    w.mark("kill_rc", k + 5, json!({"rc_at_ms": k + 5, "rc": 0}));
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
            // The restarted container as CI's case 14 listed it (fix round 3, on one clock): its
            // init a little before the daemon's `start` report, the compositor, and CEF's
            // children 130 to 450 ms after the init.
            let at = w.at(60.0);
            let init = start - INIT_BEFORE_START_EVENT_MS;
            for (pid, ppid, off, ty) in [
                (1, 0, 0, Value::Null),
                (17, 9, 100, Value::Null),
                (60, 17, 130, json!("zygote")),
                (61, 60, 450, json!("gpu-process")),
            ] {
                w.mark(
                    "tree_after",
                    at,
                    json!({"at_ms": at, "pid": pid, "ppid": ppid, "start_ms": init + off,
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
    let mut w = zygote("gpu-process", true, "zygote");
    fail_kill_rc(&mut w);
    assert_fails(
        &judge(&w, "cef-gpu-zygote-killed", "", "zyg-rc"),
        "killed_the_zygote",
        "zyg-rc",
    );
    // Fix round 3, item 3: the overlay must come back after the restart. Owed 85's CEF that never
    // starts again; an overlay back 500 ms past the budget; and a restart with no `start` event,
    // where every frame of the run would count as "after" it.
    let mut w = zygote("gpu-process", true, "zygote");
    let k = w.at(20.0);
    overlay_lost_after_die(&mut w, k, None);
    let v = judge(&w, "cef-gpu-zygote-killed", "", "zyg-nooverlay");
    assert_fails(&v, "overlay_back", "zyg-nooverlay");
    assert_eq!(
        graphics_failed(&v),
        json!(1),
        "zyg-nooverlay: Owed 85's evidence is present"
    );
    let mut w = zygote("gpu-process", true, "zygote");
    overlay_lost_after_die(&mut w, k, Some(500));
    assert_fails(
        &judge(&w, "cef-gpu-zygote-killed", "", "zyg-overlay-late"),
        "overlay_back",
        "zyg-overlay-late",
    );
    let mut w = zygote("gpu-process", true, "zygote");
    w.events.retain(|e| !(e.1 == "start" && e.0 > k));
    assert_fails(
        &judge(&w, "cef-gpu-zygote-killed", "", "zyg-overlay-nostart"),
        "overlay_back",
        "zyg-overlay-nostart",
    );
}

#[test]
fn cef_renderer_zygote_killed_restarts_in_place_and_reds_when_an_old_process_survives() {
    let w = zygote("renderer", true, "zygote");
    let v = judge(&w, "cef-renderer-zygote-killed", "", "rzyg-good");
    assert_pass(&v, "rzyg-good");
    // The printed measure: the processes counted, and the oldest one's start after the restart's.
    // CI's jq (1.7.1) refused this object's bare `a - b` value (fix round 2; jq 1.8 takes it).
    let shown = &v.json["measures"]["old_tree_gone"];
    assert_eq!(shown["cef_processes"], 2, "rzyg-good: {shown}");
    assert_eq!(shown["oldest_after_init_ms"], 130, "rzyg-good: {shown}");
    assert_eq!(
        shown["init_minus_start_event_ms"], -INIT_BEFORE_START_EVENT_MS,
        "rzyg-good: {shown}"
    );
    // Fix round 3: the anchor is the sampled init, never the daemon's `start` report. A report that
    // lags the init past CEF's first child (130 ms in) is still a clean restart.
    let mut w = zygote("renderer", true, "zygote");
    let first_start = w.starts().into_iter().find(|&s| s > w.at(20.0)).unwrap();
    for e in &mut w.events {
        if e.0 == first_start && e.1 == "start" {
            e.0 += 300;
        }
    }
    assert_pass(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-lagged"),
        "rzyg-lagged",
    );
    // Each conjunct, alone: no start after the kill; a sample taken before the restart; no init in
    // the sample; an init older than the kill (the old container); no CEF process at all (Owed 85's
    // CEF that never came back lists none).
    let tree = |w: &mut World, f: &dyn Fn(&mut Value) -> bool| {
        let mut kept = vec![];
        for (name, at, mut v) in std::mem::take(&mut w.marks) {
            if name != "tree_after" || f(&mut v) {
                kept.push((name, at, v));
            }
        }
        w.marks = kept;
    };
    let mut w = zygote("renderer", true, "zygote");
    let k = w.at(20.0);
    w.events.retain(|e| !(e.1 == "start" && e.0 > k));
    assert_fails(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-nostart"),
        "old_tree_gone",
        "rzyg-nostart",
    );
    let mut w = zygote("renderer", true, "zygote");
    let early = w.starts().into_iter().find(|&s| s > k).unwrap() - 500;
    tree(&mut w, &|v| {
        v["at_ms"] = json!(early);
        true
    });
    assert_fails(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-early"),
        "old_tree_gone",
        "rzyg-early",
    );
    let mut w = zygote("renderer", true, "zygote");
    tree(&mut w, &|v| v["pid"] != 1);
    assert_fails(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-noinit"),
        "old_tree_gone",
        "rzyg-noinit",
    );
    let mut w = zygote("renderer", true, "zygote");
    tree(&mut w, &|v| {
        if v["pid"] == 1 {
            v["start_ms"] = json!(k - 1);
        }
        true
    });
    assert_fails(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-oldinit"),
        "old_tree_gone",
        "rzyg-oldinit",
    );
    let mut w = zygote("renderer", true, "zygote");
    tree(&mut w, &|v| v["type"].is_null());
    assert_fails(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-notyped"),
        "old_tree_gone",
        "rzyg-notyped",
    );
    // Fix round 2: with no kill mark, `starts_after(null)` took the run's first start, so every
    // process looked younger than "the restart" and the check passed on nothing.
    let mut w = zygote("renderer", true, "zygote");
    w.marks.retain(|(name, _, _)| name != "kill" && name != "kill_rc");
    assert_fails(
        &judge(&w, "cef-renderer-zygote-killed", "", "rzyg-nokill"),
        "old_tree_gone",
        "rzyg-nokill",
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
    } else {
        // The loop lived to its end (no restart took its container): its done line.
        let done = kill_at.last().copied().unwrap_or(k0) + 10;
        w.mark(
            "kill_done",
            done,
            json!({"done_at_ms": done, "done": kills, "wanted": kills, "compositor_alive": true,
                "timed_out": false}),
        );
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
    // Fix round 3, item 3: CI's threshold runs 2 and 3 (40056474269): the restart, then no overlay.
    let mut w = gpu_deaths(n, 2_000, 800, true, 600);
    let k = w.at(20.0);
    overlay_lost_after_die(&mut w, k, None);
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "threshold", "gpu-nooverlay"),
        "overlay_back",
        "gpu-nooverlay",
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
    // Fix round 3, item 3: no restart, and the overlay gone for good after the last kill. And the
    // re-review's m-1: a page that stops painting and keeps beating holds its last frame on air, a
    // stale score that round 3's `overlay_back` passed (CI's `back_ms` 20 and 22).
    let last_kill = |w: &World| {
        w.marks
            .iter()
            .filter(|m| m.0 == "kill")
            .map(|m| m.1)
            .max()
            .unwrap()
    };
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let last = last_kill(&w);
    w.overlay_none.push((last, w.end + 60_000));
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-nooverlay"),
        "scheduled_changes_shown",
        "fewer-nooverlay",
    );
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let last = last_kill(&w);
    w.overlay_hold.push((last, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-held");
    assert_fails(&v, "scheduled_changes_shown", "fewer-held");
    let shown = &v.json["measures"]["scheduled_changes_shown"];
    assert!(
        shown["due_after_last_kill"].as_u64().is_some_and(|n| n >= 1)
            && shown["missing"].as_array().is_some_and(|m| !m.is_empty()),
        "fewer-held: the changes due after the last kill are missing: {shown}"
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
    // C14 fix round 2, CI's world (runs 40051995233 and 40051902711, Owed 70 (a) measured): Chromium
    // relaunched the GPU process in about 80 ms, so the loop's kills of 5 distinct pids took about
    // 330 ms, and the 1 s child poll counted one death. No restart: deaths_logged reds, and its
    // measure shows the kills against the most the log counted.
    let mut w = World::new(90);
    let k0 = w.at(20.0);
    for i in 0..i64::from(abort - 1) {
        let k = k0 + i * 80;
        w.mark("kill", k, json!({"at_ms": k, "pid": 300 + i, "n": i + 1}));
    }
    w.gpu_deaths.push(k0 + 700);
    w.line(
        k0 + 700,
        "cef health zygote_gone=- leaked_teardowns=0 gpu_process_deaths=1",
    );
    let done = k0 + 400;
    w.mark(
        "kill_done",
        done,
        json!({"done_at_ms": done, "done": abort - 1, "wanted": abort - 1,
            "compositor_alive": true, "timed_out": false}),
    );
    let v = judge(&w, "cef-gpu-deaths", "fast", "fast-ci");
    assert_fails(&v, "deaths_logged", "fast-ci");
    let shown = &v.json["measures"]["deaths_logged"];
    assert_eq!(shown["kills"], abort - 1, "fast-ci: {shown}");
    assert_eq!(shown["counted_max"], 1, "fast-ci: {shown}");
    assert_eq!(
        shown["needs"],
        World::new(1).ms("cef_gpu_deaths_to_restart"),
        "fast-ci: {shown}"
    );
}

/// The review's I8: a loop that reaches its deadline (no new GPU process to kill) fails, in every
/// variant, rather than running to the relay's own deadline.
#[test]
fn cef_gpu_deaths_reds_when_the_kill_loop_reaches_its_deadline() {
    let n = World::new(1).ms("cef_gpu_deaths_to_restart") as u32;
    let mut checked = 0;
    for variant in ["threshold", "one-fewer", "fast"] {
        let mut w = gpu_deaths(1, 2_000, 800, false, 0);
        w.marks.retain(|m| m.0 != "kill_done");
        let deadline = w.end - w.ms("budget_process_restart_output_ms");
        w.mark(
            "kill_done",
            deadline,
            json!({"done_at_ms": deadline, "done": 1, "wanted": n, "compositor_alive": true,
                "timed_out": true}),
        );
        let tag = format!("gpu-deadline-{variant}");
        assert_fails(
            &judge(&w, "cef-gpu-deaths", variant, &tag),
            "kill_loop_in_time",
            &tag,
        );
        checked += 1;
    }
    assert_eq!(checked, 3);
}

/// The review's I6: NOT RUNNABLE is never a pass. run-case.sh exits 3 (non-success), chaos.yml's leg
/// fails on it, and run-chaos.sh's table holds wave gate 3 blocked (chaos_table.rs). No case is NOT
/// RUNNABLE since case 16 was wired (Owed 82's hook), so the mechanism is probed on a copy of the
/// harness with a case of its own, and every real case file is checked to declare no reason.
#[test]
fn a_not_runnable_case_is_reported_never_faked_and_never_a_pass() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("not-runnable-probe");
    let _ = std::fs::remove_dir_all(&root);
    let h = root.join("harness");
    std::fs::create_dir_all(h.join("cases")).unwrap();
    for f in ["run-case.sh", "thresholds.json", "cases/lib.jq"] {
        std::fs::copy(repo().join("harness").join(f), h.join(f)).unwrap();
    }
    // run-case.sh reads the page token at load, to scan for it.
    let fixture = "contracts/fixtures/relay-job.v1/session-composed-720p.json";
    std::fs::create_dir_all(root.join(fixture).parent().unwrap()).unwrap();
    std::fs::copy(repo().join(fixture), root.join(fixture)).unwrap();
    let reason = "a probe case that cannot run (case_verdicts.rs)";
    std::fs::write(
        h.join("cases/probe-not-runnable.sh"),
        format!(
            "# shellcheck shell=bash\nCASE_NOT_RUNNABLE=\"{reason}\"\ncase_drive() {{ :; }}\nCASE_CHECKS=1\n\
             case_verdict() {{ require never '.x'; }}\n"
        ),
    )
    .unwrap();
    let out_root = root.join("out");
    let out = Command::new(bash())
        .arg(h.join("run-case.sh"))
        .arg("probe-not-runnable")
        .current_dir(&root)
        .env("CASE_REJUDGE", "1")
        .env("CASE_OUT_ROOT", &out_root)
        .env_remove("CASE_RUN")
        .output()
        .expect("run-case.sh runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    eprintln!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(out_root.join("probe-not-runnable/verdict.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["status"], "not_runnable", "{v}");
    assert_eq!(out.status.code(), Some(3), "NOT RUNNABLE exits non-success");
    assert_eq!(v["checks_run"], 0, "{v}");
    assert_eq!(v["not_runnable_reason"], reason, "{v}");
    assert!(stdout.lines().any(|l| l.starts_with("NOT RUNNABLE ")));
    // Every real case runs: none declares a reason (a case file that sets one is reported, never
    // run, so this would hide it from every verdict here).
    let mut cases = 0;
    for e in std::fs::read_dir(repo().join("harness/cases")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "sh") {
            let text = std::fs::read_to_string(&p).unwrap();
            assert!(
                !text.contains("CASE_NOT_RUNNABLE="),
                "{} declares itself NOT RUNNABLE",
                p.display()
            );
            cases += 1;
        }
    }
    eprintln!("case files with no NOT RUNNABLE reason: {cases}");
    assert!(cases >= 17, "the sweep read too few case files ({cases})");
}

/// Owed 82's arming line, before a process's `compositor started` (boot()'s, at +200).
fn arm(w: &mut World, start: i64, n: u64) {
    w.line(
        start + 150,
        format!("WARN test hook (Owed 82): the next {n} CEF teardowns never return"),
    );
}

const LEAK_ORIGIN: &str = "http://fake-control:7000";

/// Case 16's world (the C14 brief's amendment "Owed 82's handoff"): a compositor armed to hold
/// `hold` teardowns, and from 20 s each current build's renderer killed once that build's `cef
/// ready` is in, `hold` kills. Each kill is a CEF crash, and its page's reload logs a stale `cef
/// ready gen=<old>`. Before the cef_fail_count-th crash the next build loads after the back-off
/// (the base, doubling, at most the cap) and the old one is retired; the cef_fail_count-th fails
/// graphics, and its stop is the retirement. Each retired build's teardown is held, the reaper kills
/// its reloaded renderer REAPER_KILL_AFTER_MS after the retirement (whose page reloads again: one
/// more stale ready), and it is counted leaked TEARDOWN_LEAK_AFTER_MS after the kill. At
/// cef_leaks_to_restart leaked the compositor restarts in place, and the restarted process arms
/// `hold` again. The renderers' pids and client ids are the simulation's: the current build's id
/// the highest, as the product's attribution says.
fn leaks(hold: u64) -> World {
    let n_restart = World::new(1).ms("cef_leaks_to_restart") as u64;
    let restarts = hold >= n_restart;
    let mut w = World::new(if restarts { 80 } else { 60 });
    let reaper = w.ms("reaper_kill_after_ms");
    let leak = w.ms("teardown_leak_after_ms");
    let base = w.ms("cef_backoff_base_ms");
    let cap = w.ms("cef_backoff_cap_ms");
    let fail = w.ms("cef_fail_count") as usize;
    let boot = w.live - 9_000;
    arm(&mut w, boot, hold);
    w.line(boot + 1_000, format!("gen=1: loading {LEAK_ORIGIN}"));
    let (mut load, mut ready) = (boot + 1_000, boot + 1_500);
    let mut deciding = None;
    let mut failed_from = None;
    for i in 0..hold as usize {
        let gen = i as u64 + 1;
        let k = w.at(20.0).max(ready + 300);
        w.mark("ready", ready, json!({"gen": gen, "loading_ms": load}));
        let renderers: Vec<Value> = (1..=gen)
            .map(|j| json!({"pid": 400 + j, "client_id": 6 + j}))
            .collect();
        w.mark(
            "kill",
            k,
            json!({"at_ms": k, "pid": 400 + gen, "client_id": 6 + gen, "renderers": renderers}),
        );
        w.mark("kill_rc", k + 5, json!({"rc_at_ms": k + 5, "rc": 0}));
        w.line(k + 200, format!("cef crashed gen={gen}"));
        w.line(k + 1_500, format!("cef ready gen={gen}"));
        let retire = if i + 1 < fail {
            let next = gen + 1;
            let r = k + 200 + (base << i).min(cap);
            w.line(r, format!("graphics restart gen={next}"));
            load = r + 10;
            ready = load + 2_000;
            w.line(load, format!("gen={next}: loading {LEAK_ORIGIN}"));
            w.line(ready, format!("cef ready gen={next}"));
            w.overlay_none.push((k + 200, ready + 100));
            r
        } else {
            w.line(k + 250, format!("overlay hidden reason=failed gen={gen}"));
            w.line(k + 255, "graphics stop");
            failed_from = Some(k + 200);
            k + 255
        };
        let left = hold - 1 - i as u64;
        w.line(
            retire + 12,
            format!(
                "WARN test hook (Owed 82) gen={gen}: this CEF teardown never returns ({left} more to hold)"
            ),
        );
        let pid = 600 + gen;
        let kill = retire + reaper;
        w.line(
            kill,
            format!(
                "WARN gen={gen}: teardown has not returned after {reaper} ms; SIGKILL sent to renderer pids [{pid}] (of [{pid}])"
            ),
        );
        w.line(kill + 800, format!("cef ready gen={gen}"));
        let at = kill + leak;
        w.leaks.push(at);
        let n = w.leaks.len();
        w.line(
            at,
            format!(
                "WARN gen={gen}: teardown still has not returned {leak} ms after the kill: its thread, pipeline and renderer stay until the process restarts ({n} leaked; Owed 55)"
            ),
        );
        w.line(
            at + 5,
            format!("cef health zygote_gone=- leaked_teardowns={n} gpu_process_deaths=0"),
        );
        if n as u64 == n_restart {
            deciding = Some(at + 5);
        }
    }
    w.mark(
        "drive_done",
        w.at(20.0) + 30_000,
        json!({"kills": hold, "stopped": "none"}),
    );
    if restarts {
        let d = deciding.expect("the leak count reached the restart");
        let detail = format!("restart in place: {n_restart} CEF teardowns leaked");
        let start = w.restart(d + 600, 75, Some(&detail));
        if let Some(f) = failed_from {
            w.overlay_none.push((f, d + 600));
        }
        arm(&mut w, start, hold);
        w.line(start + 1_000, format!("gen=1: loading {LEAK_ORIGIN}"));
    } else {
        // The CEF children's CPU, twice (cef_cpu: printed, never judged).
        for (s, r, g) in [(40.0, 9_000, 1_000), (55.0, 12_000, 1_600)] {
            let at = w.at(s);
            w.mark(
                "cef_cpu",
                at,
                json!({"sample_ms": at, "children": [
                    {"pid": 30, "type": "gpu-process", "client_id": null, "cpu_ms": g},
                    {"pid": 403, "type": "renderer", "client_id": 9, "cpu_ms": r}]}),
            );
        }
    }
    w
}

/// The world's log line that starts with `prefix`, changed by `f` (None: removed).
fn edit_line(w: &mut World, prefix: &str, f: impl Fn(&str) -> Option<String>) {
    let i = w
        .log
        .iter()
        .position(|(_, m)| m.starts_with(prefix))
        .unwrap_or_else(|| panic!("no log line starts with {prefix:?}"));
    match f(&w.log[i].1) {
        Some(m) => w.log[i].1 = m,
        None => {
            w.log.remove(i);
        }
    }
}

fn kill_marks(w: &mut World) -> impl Iterator<Item = &mut (String, i64, Value)> {
    w.marks.iter_mut().filter(|m| m.0 == "kill")
}

#[test]
fn cef_leaks_two_carries_its_leaks_on_air_and_reds_without_the_hold_or_its_witnesses() {
    let n = World::new(1).ms("cef_leaks_to_restart") as u64;
    let hold = n - 1;
    let v = judge(&leaks(hold), "cef-leaks", "two", "leaks2-good");
    assert_pass(&v, "leaks2-good");
    let r = &v.json["measures"]["retirements"];
    let gens: Vec<u64> = (1..=hold).collect();
    assert_eq!(
        (&r["held"], &r["kills"], &r["want"]),
        (&json!(hold), &json!(hold), &json!(hold)),
        "leaks2-good: {r}"
    );
    assert_eq!(
        (&r["held_gens"], &r["killed_gens"]),
        (&json!(gens), &json!(gens)),
        "{r}"
    );
    // The CPU measure, from the sim's two samples 15 s apart: (12 000 - 9 000) / 15 and
    // (1 600 - 1 000) / 15 ms per s.
    let cpu = &v.json["measures"]["cef_cpu"];
    assert_eq!(cpu["samples"], 2, "{cpu}");
    assert_eq!(cpu["interval_ms"], 15_000, "{cpu}");
    assert_eq!(
        cpu["ms_per_s"],
        json!([{"type": "gpu-process", "client_id": null, "ms_per_s": 40},
            {"type": "renderer", "client_id": 9, "ms_per_s": 200}]),
        "{cpu}"
    );
    // A retirement no kill made (a load timeout: count retirements, not kills): the second build's
    // teardown is held with no kill of it, and the leg passes.
    let one_kill = |w: &mut World| {
        let last = w
            .marks
            .iter()
            .filter(|m| m.0 == "kill")
            .map(|m| m.1)
            .max()
            .unwrap();
        w.marks
            .retain(|m| !((m.0 == "kill" || m.0 == "kill_rc" || m.0 == "ready") && m.1 >= last - 300));
    };
    let mut w = leaks(hold);
    one_kill(&mut w);
    let v = judge(&w, "cef-leaks", "two", "leaks2-load-timeout");
    assert_pass(&v, "leaks2-load-timeout");
    let r = &v.json["measures"]["retirements"];
    assert_eq!((&r["held"], &r["kills"]), (&json!(hold), &json!(hold - 1)), "{r}");
    let mut checked = 0;
    let mut red = |w: World, checks: &[&str], tag: &str| {
        let v = judge(&w, "cef-leaks", "two", tag);
        for c in checks {
            assert_fails(&v, c, tag);
        }
        checked += 1;
    };
    // The plain relay image: no arming, so nothing is held, reaped or leaked, and the leg would
    // read "0 leaked" (mutant a's world): every retirement witness reds, none vacuously passes.
    let mut w = leaks(hold);
    w.log.retain(|(_, m)| !m.starts_with("WARN "));
    w.leaks.clear();
    w.log
        .retain(|(_, m)| !m.starts_with("cef health ") || m.contains("leaked_teardowns=0"));
    red(
        w,
        &[
            "armed",
            "held_n",
            "reaper_killed",
            "leaks_counted",
            "leaked_logged",
        ],
        "leaks2-plain-image",
    );
    // No process boot read at all (an empty or unstamped log): no arming is no pass.
    let mut w = leaks(hold);
    w.log
        .retain(|(_, m)| !m.starts_with("compositor started") && !m.contains("(Owed 82): the next"));
    red(w, &["armed"], "leaks2-no-boot");
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN test hook (Owed 82): the next", |m| {
        Some(m.replace(&format!("next {hold} "), &format!("next {} ", hold + 1)))
    });
    red(w, &["armed"], "leaks2-armed-other-n");
    let mut w = leaks(hold);
    let i = w
        .log
        .iter()
        .position(|(_, m)| m.contains("(Owed 82): the next"))
        .unwrap();
    w.log[i].0 += 100;
    red(w, &["armed"], "leaks2-armed-after-boot");
    // The kill hit a held build's renderer, not the current one; failed; came before the current
    // build's ready; or never ran.
    let mut w = leaks(hold);
    if let Some(k) = kill_marks(&mut w).last() {
        k.2["client_id"] = json!(7);
        k.2["pid"] = json!(401);
    }
    red(w, &["killed_one_current"], "leaks2-kill-old");
    let mut w = leaks(hold);
    if let Some(rc) = w.marks.iter_mut().find(|m| m.0 == "kill_rc") {
        rc.2["rc"] = json!(1);
    }
    red(w, &["killed_one_current"], "leaks2-kill-rc");
    let mut w = leaks(hold);
    let i = w.log.iter().position(|(_, m)| m == "cef ready gen=2").unwrap();
    w.log[i].0 += 60_000;
    red(w, &["killed_one_current"], "leaks2-kill-before-ready");
    let mut w = leaks(hold);
    w.marks.retain(|m| m.0 != "kill" && m.0 != "kill_rc");
    red(w, &["killed_one_current"], "leaks2-no-kill");
    // A kill past N (the drive ran on), and a kill whose status never printed.
    let mut w = leaks(hold);
    let next_ready = format!("cef ready gen={}", hold + 1);
    let ready = w.log.iter().find(|(_, m)| *m == next_ready).map(|l| l.0).unwrap();
    let k = ready + 300;
    let renderers: Vec<Value> = (1..=hold + 1)
        .map(|j| json!({"pid": 400 + j, "client_id": 6 + j}))
        .collect();
    w.mark(
        "kill",
        k,
        json!({"at_ms": k, "pid": 401 + hold, "client_id": 7 + hold, "renderers": renderers}),
    );
    w.mark("kill_rc", k + 5, json!({"rc_at_ms": k + 5, "rc": 0}));
    red(w, &["killed_one_current"], "leaks2-kill-past-n");
    let mut w = leaks(hold);
    let last = w
        .marks
        .iter()
        .filter(|m| m.0 == "kill_rc")
        .map(|m| m.1)
        .max()
        .unwrap();
    w.marks.retain(|m| !(m.0 == "kill_rc" && m.1 == last));
    red(w, &["killed_one_current"], "leaks2-kill-no-rc");
    // One retirement short; the count not counting down; one held past the budget.
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN test hook (Owed 82) gen=2", |_| None);
    red(w, &["held_n"], "leaks2-held-short");
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN test hook (Owed 82) gen=2", |m| {
        Some(m.replace("(0 more", "(1 more"))
    });
    red(w, &["held_n"], "leaks2-held-lefts");
    let mut w = leaks(hold);
    let at = w.at(45.0);
    w.line(
        at,
        "WARN test hook (Owed 82) gen=3: this CEF teardown never returns (0 more to hold)",
    );
    red(w, &["held_n"], "leaks2-held-past-budget");
    // Two held lines of one gen (one kill, the other retirement a load timeout's), and a killed
    // build whose teardown was never held (another gen's was).
    let mut w = leaks(hold);
    one_kill(&mut w);
    edit_line(&mut w, "WARN test hook (Owed 82) gen=2", |m| {
        Some(m.replace("gen=2:", "gen=1:"))
    });
    red(w, &["held_n"], "leaks2-held-same-gen");
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN test hook (Owed 82) gen=2", |m| {
        Some(m.replace("gen=2:", "gen=9:"))
    });
    red(w, &["held_n"], "leaks2-held-other-gen");
    // Mutant 8's signature (the hold after set_state(NULL): no renderer to kill), no reaper line,
    // and a reaper on another wait than the adapters' constant.
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN gen=2: teardown has not returned", |m| {
        Some(m.replace("pids [602] (of [602])", "pids [] (of [])"))
    });
    red(w, &["reaper_killed"], "leaks2-reaper-none");
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN gen=1: teardown has not returned", |_| None);
    red(w, &["reaper_killed", "leaks_counted"], "leaks2-reaper-missing");
    let reaper = w_ms("reaper_kill_after_ms");
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN gen=1: teardown has not returned", |m| {
        Some(m.replace(&format!("after {reaper} ms"), &format!("after {} ms", reaper - 1)))
    });
    red(w, &["reaper_killed"], "leaks2-reaper-ms");
    // A leak never counted, or counted after the session's end (no room left).
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN gen=2: teardown still has not returned", |_| None);
    red(w, &["leaks_counted"], "leaks2-leak-missing");
    let mut w = leaks(hold);
    let end = w.end;
    let i = w
        .log
        .iter()
        .position(|(_, m)| m.starts_with("WARN gen=2: teardown still"))
        .unwrap();
    w.log[i].0 = end + 500;
    red(w, &["leaks_counted"], "leaks2-leak-late");
    // The count not 1..N, a leak of another gen, and a leak logged before its reaper's kill.
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN gen=2: teardown still has not returned", |m| {
        Some(m.replace("(2 leaked;", "(1 leaked;"))
    });
    red(w, &["leaks_counted"], "leaks2-leak-count");
    let mut w = leaks(hold);
    edit_line(&mut w, "WARN gen=2: teardown still has not returned", |m| {
        Some(m.replace("WARN gen=2:", "WARN gen=9:"))
    });
    red(w, &["leaks_counted"], "leaks2-leak-other-gen");
    // The leak counted for a build that was reaped but never held (a teardown not the hook's).
    let mut w = leaks(hold);
    let leak_at = w
        .log
        .iter()
        .find(|(_, m)| m.starts_with("WARN gen=2: teardown still"))
        .unwrap()
        .0;
    let reaper = w.ms("reaper_kill_after_ms");
    w.line(
        leak_at - 100,
        format!(
            "WARN gen=3: teardown has not returned after {reaper} ms; SIGKILL sent to renderer pids [603] (of [603])"
        ),
    );
    edit_line(&mut w, "WARN gen=2: teardown still has not returned", |m| {
        Some(m.replace("WARN gen=2:", "WARN gen=3:"))
    });
    red(w, &["leaks_counted"], "leaks2-leak-unheld-gen");
    let mut w = leaks(hold);
    let leak_at = w
        .log
        .iter()
        .find(|(_, m)| m.starts_with("WARN gen=2: teardown still"))
        .unwrap()
        .0;
    let i = w
        .log
        .iter()
        .position(|(_, m)| m.starts_with("WARN gen=2: teardown has not"))
        .unwrap();
    w.log[i].0 = leak_at + 100;
    red(w, &["leaks_counted"], "leaks2-leak-before-reaper");
    // Owner ruling 4A: two leaked is carried, never restarted (mutant c's ruling, simulated).
    let mut w = leaks(hold);
    let at = w.at(45.0);
    w.restart(
        at,
        75,
        Some(&format!("restart in place: {hold} CEF teardowns leaked")),
    );
    red(w, &["restart_as_ruled"], "leaks2-restarted");
    // The count logged short of the hold at the end.
    let mut w = leaks(hold);
    w.leaks.pop();
    w.log
        .retain(|(_, m)| !m.contains(&format!("leaked_teardowns={hold} ")));
    red(w, &["leaked_logged"], "leaks2-logged-short");
    // The last health line names a zygote gone, or a GPU death, beside the leaks.
    for (tag, line) in [
        (
            "leaks2-health-zygote",
            format!("cef health zygote_gone=1 leaked_teardowns={hold} gpu_process_deaths=0"),
        ),
        (
            "leaks2-health-gpu",
            format!("cef health zygote_gone=- leaked_teardowns={hold} gpu_process_deaths=1"),
        ),
    ] {
        let mut w = leaks(hold);
        let at = w.end - 50;
        w.line(at, line);
        red(w, &["leaked_logged"], tag);
    }
    // The overlay off air after the last kill, graphics given up, or the output lost.
    let mut w = leaks(hold);
    let last = w
        .marks
        .iter()
        .filter(|m| m.0 == "kill")
        .map(|m| m.1)
        .max()
        .unwrap();
    w.overlay_none.push((last, w.end + 60_000));
    red(w, &["overlay_on_air"], "leaks2-overlay-off");
    // The newest build ready, then its page holding one frame (a stale score on air).
    let mut w = leaks(hold);
    let ready = w.log.iter().find(|(_, m)| *m == next_ready).map(|l| l.0).unwrap();
    w.overlay_hold.push((ready + 200, w.end + 60_000));
    red(w, &["overlay_on_air"], "leaks2-overlay-held");
    let mut w = leaks(hold);
    let at = w.at(50.0);
    w.line(at, format!("overlay hidden reason=failed gen={}", hold + 1));
    red(w, &["overlay_on_air"], "leaks2-graphics-failed");
    let mut w = leaks(hold);
    let at = w.at(48.0);
    w.outages.push((at, at + 2_000));
    red(w, &["on_air_to_end"], "leaks2-outage");
    eprintln!("two's reds checked: {checked}");
    assert_eq!(checked, 32);
}

/// thresholds.json's value for `key`.
fn w_ms(key: &str) -> i64 {
    World::new(1).ms(key)
}

#[test]
fn cef_leaks_three_restarts_in_place_at_the_third_leak_and_reds_without_it() {
    let n = w_ms("cef_leaks_to_restart") as u64;
    let v = judge(&leaks(n), "cef-leaks", "three", "leaks3-good");
    assert_pass(&v, "leaks3-good");
    assert_eq!(v.json["measures"]["retirements"]["held"], json!(n), "{}", v.json);
    // A shown value past run-case.sh's 300-character cut says so, with its length; verdict.json
    // keeps it whole (the PR #65 confirm pass's n-3). Three kills' list is past it.
    let whole = v.json["measures"]["killed_one_current"].to_string();
    assert_eq!(
        v.json["measures"]["killed_one_current"]["kills"]
            .as_array()
            .map(Vec::len),
        Some(n as usize)
    );
    let line = v
        .lines
        .iter()
        .find(|l| l.starts_with("PASS killed_one_current: "))
        .unwrap();
    let marker = format!(
        " [cut at 300 of {} characters; verdict.json's measures has it whole]",
        whole.len()
    );
    assert!(
        whole.len() > 300 && line.ends_with(&marker),
        "{line}\nwant the marker {marker:?}"
    );
    let short = v.lines.iter().find(|l| l.starts_with("PASS held_n: ")).unwrap();
    assert!(
        !short.contains("[cut at 300"),
        "a value under the cut is printed whole: {short}"
    );
    // The restarted process is armed again and its session-end stop is held, reaped and leaked in
    // the drain: never this case's (the first process's window).
    let mut w = leaks(n);
    let (end, reaper, leak) = (
        w.end,
        w.ms("reaper_kill_after_ms"),
        w.ms("teardown_leak_after_ms"),
    );
    w.line(
        end + 300,
        format!(
            "WARN test hook (Owed 82) gen=1: this CEF teardown never returns ({} more to hold)",
            n - 1
        ),
    );
    w.line(
        end + 300 + reaper,
        format!(
            "WARN gen=1: teardown has not returned after {reaper} ms; SIGKILL sent to renderer pids [] (of [])"
        ),
    );
    w.line(
        end + 300 + reaper + leak,
        format!(
            "WARN gen=1: teardown still has not returned {leak} ms after the kill: its thread, pipeline and renderer stay until the process restarts (1 leaked; Owed 55)"
        ),
    );
    assert_pass(
        &judge(&w, "cef-leaks", "three", "leaks3-p2-held"),
        "leaks3-p2-held",
    );
    let mut checked = 0;
    let mut red = |w: World, check: &str, tag: &str| {
        assert_fails(&judge(&w, "cef-leaks", "three", tag), check, tag);
        checked += 1;
    };
    // The restarted process's arming line read in the first process instead (both before its
    // boot): one per boot, each after the boot before it.
    let mut w = leaks(n);
    let i = w
        .log
        .iter()
        .rposition(|(_, m)| m.contains("(Owed 82): the next"))
        .unwrap();
    let first = w
        .log
        .iter()
        .position(|(_, m)| m.contains("(Owed 82): the next"))
        .unwrap();
    w.log[i].0 = w.log[first].0 + 10;
    red(w, "armed", "leaks3-armed-twice-in-p1");
    // The restarted process unarmed (a second process on another image).
    let mut w = leaks(n);
    let i = w
        .log
        .iter()
        .rposition(|(_, m)| m.contains("(Owed 82): the next"))
        .unwrap();
    w.log.remove(i);
    red(w, "armed", "leaks3-p2-unarmed");
    // The domain restarting nothing at the third leak (no die, no restart beat).
    let mut w = leaks(n);
    let die = w
        .events
        .iter()
        .filter(|e| e.1 == "die" && e.2 == Some(75))
        .map(|e| e.0)
        .max()
        .unwrap();
    w.events.retain(|e| !(e.0 >= die && e.0 <= die + 1_000));
    w.events.push((w.end + 3_000, "die", Some(0)));
    w.beats.retain(|b| b.1["exitReason"] != "restart");
    w.restarts = 0;
    red(w, "restart_as_ruled", "leaks3-no-restart");
    // The restart's detail names another count; the deciding line counted a GPU death too.
    let mut w = leaks(n);
    for b in &mut w.beats {
        if b.1["exitReason"] == "restart" {
            b.1["exitDetail"] = json!(format!("restart in place: {} CEF teardowns leaked", n - 1));
        }
    }
    red(w, "final_beat_detail", "leaks3-detail");
    let mut w = leaks(n);
    edit_line(
        &mut w,
        &format!("cef health zygote_gone=- leaked_teardowns={n} "),
        |m| Some(m.replace("gpu_process_deaths=0", "gpu_process_deaths=1")),
    );
    red(w, "leaks_health", "leaks3-deciding-gpu");
    let mut w = leaks(n);
    edit_line(
        &mut w,
        &format!("cef health zygote_gone=- leaked_teardowns={n} "),
        |m| Some(m.replace("zygote_gone=-", "zygote_gone=1")),
    );
    red(w, "leaks_health", "leaks3-deciding-zygote");
    let mut w = leaks(n);
    edit_line(
        &mut w,
        &format!("cef health zygote_gone=- leaked_teardowns={n} "),
        |m| {
            Some(m.replace(
                &format!("leaked_teardowns={n}"),
                &format!("leaked_teardowns={}", n + 1),
            ))
        },
    );
    red(w, "leaks_health", "leaks3-deciding-past-n");
    // The first process's reaper line missing, the restarted one's of the same gen present.
    let mut w = leaks(n);
    edit_line(&mut w, "WARN gen=1: teardown has not returned", |_| None);
    let reaper = w.ms("reaper_kill_after_ms");
    let end = w.end;
    w.line(
        end + 400,
        format!(
            "WARN gen=1: teardown has not returned after {reaper} ms; SIGKILL sent to renderer pids [701] (of [701])"
        ),
    );
    red(w, "reaper_killed", "leaks3-reaper-in-p2");
    // The third retirement never held (the graphics stop's teardown returned).
    let mut w = leaks(n);
    edit_line(&mut w, &format!("WARN test hook (Owed 82) gen={n}"), |_| None);
    red(w, "held_n", "leaks3-third-not-held");
    eprintln!("three's reds checked: {checked}");
    assert_eq!(checked, 9);
}

/// The case files that are not chaos cases, each run by hand from harness.yml: C13's smoke case
/// (the harness's own end-to-end run) and Owed 81's two probe controls (F-029). The same list is in
/// chaos_table.rs.
const NOT_CHAOS: [&str; 3] = ["smoke", "owed81-noaudio", "owed81-align1"];

/// Every case file in harness/cases has a test here: a new case cannot ship unjudged.
#[test]
fn every_case_file_is_judged_here() {
    let me = include_str!("case_verdicts.rs");
    let (mut n, mut not_chaos) = (0, 0);
    for e in std::fs::read_dir(repo().join("harness/cases")).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        let Some(case) = name.strip_suffix(".sh") else {
            continue;
        };
        if NOT_CHAOS.contains(&case) {
            not_chaos += 1;
            continue;
        }
        n += 1;
        assert!(
            me.contains(&format!("\"{case}\"")),
            "harness/cases/{name} has no verdict test in case_verdicts.rs"
        );
    }
    eprintln!("case files checked: {n}; not chaos cases: {not_chaos}");
    assert_eq!(
        n, 17,
        "the brief's 16 cases (amended 2026-10-20) and case 17 (2026-10-24)"
    );
    // Every name excused is a file, so the list cannot hide a renamed or a deleted case.
    assert_eq!(not_chaos, NOT_CHAOS.len(), "a NOT_CHAOS name with no case file");
}

/// Each case excused as NOT_CHAOS other than smoke is one of Owed 81's controls: smoke's run with
/// one camera knob changed, so it must drive exactly what smoke drives, for as long (the PR #59
/// review's m-r2). A control whose drive drifted would no longer compare with a smoke run. Each
/// file is sourced by bash and its `case_drive` printed by `declare -f`, so a comment or a
/// re-indent is no difference and a changed command is.
#[test]
fn every_not_chaos_control_drives_what_smoke_drives() {
    // The drive, and every stack knob a case file may set beside it (run-case.sh's header: the
    // compose overlay, the teardown hook, the compose profiles, the camera's knobs, the checker's
    // recordings, the deadline) and every camera variable it exports, so a control cannot inject a
    // fault through an overlay or a knob with smoke's exact drive (the PR #65 review's m-1).
    let drive = |case: &str| {
        let file = repo().join(format!("harness/cases/{case}.sh"));
        let out = Command::new(bash())
            .args([
                "-c",
                r#"set -eu; CASE_COMPOSE=""; CASE_TEARDOWN=""; source "$1"; declare -f case_drive
                for k in CASE_LIVE_S CASE_COMPOSE CASE_TEARDOWN COMPOSE_PROFILES CASE_DEADLINE_S \
                  CAMERA_SIZE CAMERA_PATTERN CHECKER_RECORDINGS; do echo "$k=${!k-<unset>}"; done
                env | grep -E '^CAMERA_' | sort | sed 's/^/env /' || true"#,
                "drive",
            ])
            .arg(&file)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .output()
            .expect("bash runs");
        assert!(
            out.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let smoke = drive("smoke");
    assert!(
        smoke.contains("case_drive ()")
            && smoke.contains("netem.sh")
            && smoke.contains("CASE_LIVE_S=60\n")
            && smoke.contains("CASE_COMPOSE=\n")
            && smoke.contains("COMPOSE_PROFILES=probe\n")
            && !smoke.contains("\nenv "),
        "smoke's drive did not print: {smoke}"
    );
    // Each control's documented difference from smoke, and nothing else: its one camera variable
    // (the case file's header). Smoke already runs compose's `probe` profile (the input tap, Owed
    // 81), so the profile is no difference.
    let allowed = |case: &str| -> &str {
        match case {
            "owed81-noaudio" => "env CAMERA_AUDIO=off",
            "owed81-align1" => "env CAMERA_TS_ALIGNMENT=1",
            other => panic!("{other}: a NOT_CHAOS control with no documented difference from smoke"),
        }
    };
    let mut checked = 0;
    for case in NOT_CHAOS.iter().filter(|c| **c != "smoke") {
        let mut want: Vec<String> = smoke.lines().map(str::to_owned).collect();
        want.push(allowed(case).to_owned());
        let got: Vec<String> = drive(case).lines().map(str::to_owned).collect();
        assert_eq!(got, want, "{case}'s drive or stack is not smoke's");
        checked += 1;
    }
    eprintln!("not-chaos controls checked against smoke's drive and stack: {checked}");
    assert_eq!(checked, NOT_CHAOS.len() - 1);
}

/// Each `$t.<key>` a verdict filter reads (lib.jq, every case file, run-case.sh) and each
/// `$(threshold <key>)` a drive reads names a key of harness/thresholds.json. A renamed key would
/// read null, and jq orders null below every number, so a bound like `x >= $t.k` would pass on null
/// (the PR #65 review's m-2).
#[test]
fn every_threshold_a_verdict_or_a_drive_reads_exists() {
    let thresholds: Value =
        serde_json::from_str(&std::fs::read_to_string(repo().join("harness/thresholds.json")).unwrap())
            .unwrap();
    let keys = thresholds.as_object().expect("thresholds.json is an object");
    let mut files = vec![
        repo().join("harness/cases/lib.jq"),
        repo().join("harness/run-case.sh"),
    ];
    for e in std::fs::read_dir(repo().join("harness/cases")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "sh") {
            files.push(p);
        }
    }
    let key_after = |text: &str, prefix: &str| -> Vec<String> {
        text.match_indices(prefix)
            .map(|(i, _)| {
                text[i + prefix.len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                    .collect()
            })
            .collect()
    };
    let (mut reads, mut unknown) = (0, vec![]);
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for prefix in ["$t.", "$(threshold "] {
            for k in key_after(&text, prefix) {
                reads += 1;
                if k.is_empty() || !keys.contains_key(&k) {
                    unknown.push(format!("{}: {prefix}{k}", f.display()));
                }
            }
        }
    }
    eprintln!(
        "threshold reads checked: {reads}, in {} files, against {} keys",
        files.len(),
        keys.len()
    );
    assert!(reads >= 100, "the sweep read too few threshold reads ({reads})");
    assert!(
        unknown.is_empty(),
        "threshold keys thresholds.json does not carry: {unknown:?}"
    );
}
