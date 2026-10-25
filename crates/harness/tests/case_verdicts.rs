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
    /// Lines docker stamped late, as (at, the line's start, ms late): `docker logs --timestamps`
    /// stamps a line when its daemon reads it, so the stamp trails the process's own `ts=` by a
    /// read latency with no fixed bound (the PR #73 confirm pass's b-1).
    docker_late: Vec<(i64, String, i64)>,
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
    /// The programme records this long past the end request (the final hold's end slate); 0 stops
    /// the recording at the end request.
    tail_ms: i64,
    /// Changes run.json before it is written (a run whose end request was never recorded).
    run_patch: Option<fn(&mut Value)>,
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
            docker_late: vec![],
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
            tail_ms: 0,
            run_patch: None,
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

    /// The line at `at` starting `start` reached docker `late` ms after the process wrote it.
    fn docker_late(&mut self, at: i64, start: &str, late: i64) {
        assert!(
            self.log.iter().any(|(a, m)| *a == at && m.starts_with(start)),
            "no line {start:?} at {at}"
        );
        self.docker_late.push((at, start.into(), late));
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
            if w > (self.end + self.tail_ms) as f64 - PERIOD_MS {
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
                let late = self
                    .docker_late
                    .iter()
                    .find(|(a, start, _)| a == at && m.starts_with(start.as_str()))
                    .map_or(0, |l| l.2);
                let (d, _) = stamps(*at + late);
                let (_, ts) = stamps(*at);
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
        let mut run = json!({"case": "", "variant": "", "run": "", "name": "", "live_ms": self.live,
            "drive_start_ms": self.drive, "end_request_ms": self.end,
            "exit_seen_ms": self.end + hold + 2_000,
            "compositor": {"status": "exited", "exit_code": 0, "restarts": self.restarts},
            "restarts_at_live": 0, "last_beat": beats.last().unwrap().1, "events": events,
            "control_state": state, "cores": self.cores});
        if let Some(patch) = self.run_patch {
            patch(&mut run);
        }
        put("run.json", &run);
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

/// CI's swaps (runs 40056474269, 40106770881 and PR #65's): 515-520 ms after the sink's error, the
/// back-off from Connected (`output_backoff_base_ms`) and the driver's next tick.
const TICK_AFTER_BACKOFF_MS: i64 = 20;

/// The stalled sink's error line, as the compositor logs it (`output gen=<g> error:`).
#[derive(Clone, Copy)]
enum ErrLine {
    /// The product (Owed 87): sink 1's own socket timeout, the back-off and a tick before the swap.
    Product,
    /// Sink `gen`'s error, `before` ms before the swap (negative: after it).
    At(u64, i64),
    /// No error line at all.
    Missing,
}

/// A stall from 20 s. The egress total grows once more after the cut (the kernel's send buffer
/// drains for under a second: the sink's last write), then by `trickle` at each beat until the
/// clear. The sink's own socket timeout fails it `timeout_x` sink timeouts after that last write
/// (Owed 87; the owner, 2026-10-24, "A": rtmp2sink's `timeout` is the only detector of a
/// destination stall), and the swap follows the back-off from Connected and a tick later, named
/// `error`; the next beat carries it. `timeout_x` 1 is the product; 2 a detector twice as slow; 4
/// the sink's own wait at 20 s (mutant m9's value; on CI the destination closed the publisher first,
/// about 11 s after the cut: case 17's `rule-m9-ci`); 0 errors and swaps nothing.
fn dest_stall(trickle: i64, timeout_x: i64, stats: Value) -> World {
    dest_stall_err(trickle, timeout_x, stats, ErrLine::Product)
}

/// [`dest_stall`], with the error line `err` in place of the product's.
fn dest_stall_err(trickle: i64, timeout_x: i64, stats: Value, err: ErrLine) -> World {
    dest_stall_in(trickle, timeout_x, stats, err, "pending")
}

/// [`dest_stall_err`], the clear landing in `phase` of the reconnect cycle that follows the swap.
fn dest_stall_in(trickle: i64, timeout_x: i64, stats: Value, err: ErrLine, phase: &str) -> World {
    let mut w = dest_cut(2_500, stats);
    let cut = w.at(20.0);
    let clear = w.at(50.0) + 50;
    let last_write = cut + 800;
    let swaps = timeout_x > 0;
    let gap = w.ms("output_backoff_base_ms") + TICK_AFTER_BACKOFF_MS;
    let swap = last_write + timeout_x * w.ms("output_sink_timeout_ms") + gap;
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
        w.swap(swap, 2, "error");
        with_cycle(&mut w, swap, 3, phase, phase);
    }
    let line = match err {
        ErrLine::Product if swaps => Some((1, gap)),
        ErrLine::At(gen, before) => Some((gen, before)),
        _ => None,
    };
    if let Some((gen, before)) = line {
        w.line(
            swap - before,
            format!("output gen={gen} error: Connection error: Socket I/O timed out (gstrtmp2sink.c)"),
        );
    }
    w
}

/// A stall whose error comes `after_cut` ms after the cut, and its swap the back-off and a tick
/// later: a detector faster than the sink's timeout (case 17's `rule-early`, the PR #65 review's
/// m-3), in `trickle`'s world.
fn dest_stall_error_at(after_cut: i64, trickle: i64, stats: Value) -> World {
    let w = World::new(80);
    let gap = w.ms("output_backoff_base_ms") + TICK_AFTER_BACKOFF_MS;
    dest_stall_error_swap_at(after_cut, gap, trickle, stats)
}

/// [`dest_stall_error_at`], the swap `error_to_swap` ms after the error, as the log's stamps read it.
fn dest_stall_error_swap_at(after_cut: i64, error_to_swap: i64, trickle: i64, stats: Value) -> World {
    let mut w = dest_stall_in(trickle, 0, stats, ErrLine::Missing, "pending");
    let cut = w.at(20.0);
    let error = cut + after_cut;
    let swap = error + error_to_swap;
    w.line(
        error,
        "output gen=1 error: Connection error: Socket I/O timed out (gstrtmp2sink.c)",
    );
    w.swap(swap, 2, "error");
    w.beats_in(swap, cut + 60_000, |b| b["sinkSwaps"] = json!(1));
    with_cycle(&mut w, swap, 3, "pending", "pending");
    w
}

/// The earliest error after the cut the floor accepts: one sink timeout less one frame (the bytes
/// flow at full rate up to the cut), rounded up to the ms the log carries.
fn cut_floor_ms(w: &World) -> i64 {
    let frame = w.t["frame_period_ms"].as_f64().expect("frame_period_ms");
    (w.ms("output_sink_timeout_ms") as f64 - frame).ceil() as i64
}

/// Case 5 in CI (run 40045826841): at 1 kbit nothing was sent in 30 s, and the queue held the leg.
fn stalled() -> Value {
    netem_queued(true, 0, 0, 0, 130_862, 45)
}

#[test]
fn dest_stall_swaps_on_the_sinks_error_and_reds_without_it_or_a_slow_one() {
    let w = dest_stall(0, 1, stalled());
    let (base, timeout) = (w.ms("output_backoff_base_ms"), w.ms("output_sink_timeout_ms"));
    let v = judge(&w, "dest-stall", "", "stall-good");
    assert_pass(&v, "stall-good");
    let m = &v.json["measures"]["stall_swap"];
    assert_eq!(
        (m["reason"].as_str(), m["reasons_paired"].as_bool()),
        (Some("error"), Some(true)),
        "stall-good: {m}"
    );
    assert_eq!(
        m["error_to_swap_ms"],
        base + TICK_AFTER_BACKOFF_MS,
        "stall-good: {m}"
    );
    assert_eq!(m["error_from_cut_ms"], 800 + timeout, "stall-good: {m}");
    let mut checked = 1;
    // No error and no swap; a detector twice as slow as the sink's timeout; mutant m9 (20 s).
    for (tag, x) in [("stall-noswap", 0), ("stall-slow", 2), ("stall-m9", 4)] {
        let w = dest_stall(0, x, stalled());
        assert_fails(&judge(&w, "dest-stall", "", tag), "stall_swap", tag);
        checked += 1;
    }
    // The backoff leg: the same swap, the clear after the last sink's connect failed.
    let w = dest_stall_in(0, 1, stalled(), ErrLine::Product, "backoff");
    let v = judge(&w, "dest-stall", "backoff", "stall-backoff");
    assert_pass(&v, "stall-backoff");
    assert_eq!(v.json["measures"]["clear_phase_driven"]["phase"], "backoff");
    let v = judge(&w, "dest-stall", "", "stall-backoff-as-pending");
    assert_fails(&v, "clear_phase_driven", "stall-backoff-as-pending");
    checked += 2;
    // The back-off is a floor no product can beat, on the process's own clock (b-1): the error's
    // `ts=` is no later than the `now` the domain backs off from, and the swap's no earlier than the
    // tick that builds, so their difference is at least the base, cut to the ms or not. 1 ms under
    // reds, the base passes, and a swap that skipped the back-off (a tick after the error) reds.
    assert!(base - 1 > TICK_AFTER_BACKOFF_MS, "{base}");
    let w = dest_stall_err(0, 1, stalled(), ErrLine::At(1, base - 1));
    assert_fails(
        &judge(&w, "dest-stall", "", "stall-err-early"),
        "stall_swap",
        "stall-err-early",
    );
    let w = dest_stall_err(0, 1, stalled(), ErrLine::At(1, TICK_AFTER_BACKOFF_MS));
    assert_fails(
        &judge(&w, "dest-stall", "", "stall-err-no-backoff"),
        "stall_swap",
        "stall-err-no-backoff",
    );
    let w = dest_stall_err(0, 1, stalled(), ErrLine::At(1, base));
    assert_pass(&judge(&w, "dest-stall", "", "stall-err-base"), "stall-err-base");
    checked += 3;
    // PR #71's CI run 40137725801 (dest-stall-backoff): the lines' own `ts=` 500 ms apart
    // (`.137Z`, `.637Z`), their docker stamps 499 (`.138389528`, `.637769066`: the error line read
    // about 1.4 ms late), the swap 6 197 ms after the cut. The product backed off exactly its base;
    // judged on docker's stamps with the floor at the base it red, NOT OWED. On `ts=` it passes
    // (judged in the default leg: the clear's phase is not this check's).
    let mut w = dest_stall_error_swap_at(6_197 - base, base, 0, stalled());
    let error = w.at(20.0) + 6_197 - base;
    w.docker_late(error, "output gen=1 error:", 1);
    let v = judge(&w, "dest-stall", "", "stall-ci-499");
    assert_pass(&v, "stall-ci-499");
    let m = &v.json["measures"]["stall_swap"];
    let docker = m["swap_ms"]
        .as_i64()
        .zip(m["error_ms"].as_i64())
        .map(|(s, e)| s - e);
    assert_eq!(
        (docker, m["error_to_swap_ms"].as_i64(), m["from_cut_ms"].as_i64()),
        (Some(base - 1), Some(base), Some(6_197)),
        "stall-ci-499: {m}"
    );
    // The mirror: docker's stamps a full base apart (the swap line read 1 ms late), the process's
    // own 1 ms short of it. The interval is the process's, so this reds, and on that alone.
    let mut w = dest_stall_error_swap_at(6_197 - base, base - 1, 0, stalled());
    let swap = w.at(20.0) + 6_197 - 1;
    w.docker_late(swap, "sink swap gen=2 ", 1);
    let v = judge(&w, "dest-stall", "", "stall-ts-499");
    assert_fails(&v, "stall_swap", "stall-ts-499");
    assert_eq!(v.json["failed"], json!(["stall_swap"]), "stall-ts-499");
    let m = &v.json["measures"]["stall_swap"];
    let docker = m["swap_ms"]
        .as_i64()
        .zip(m["error_ms"].as_i64())
        .map(|(s, e)| s - e);
    assert_eq!(
        (docker, m["error_to_swap_ms"].as_i64()),
        (Some(base), Some(base - 1)),
        "stall-ts-499: {m}"
    );
    checked += 2;
    // An error sooner after the cut than the sink's timeout allows is not the stall's (the PR #65
    // review's m-3): the floor itself passes, 1 ms sooner reds on the floor alone.
    let w = dest_stall_error_at(cut_floor_ms(&w), 0, stalled());
    assert_pass(&judge(&w, "dest-stall", "", "stall-cut-floor"), "stall-cut-floor");
    let w = dest_stall_error_at(cut_floor_ms(&w) - 1, 0, stalled());
    let v = judge(&w, "dest-stall", "", "stall-cut-early");
    assert_fails(&v, "stall_swap", "stall-cut-early");
    assert_eq!(v.json["failed"], json!(["stall_swap"]), "stall-cut-early");
    checked += 2;
    // The reason line and the error line are read, never assumed: the retired reason, the connect
    // timeout's, no reason line, no error line, only the new sink's error (its first connect failing
    // under the stall, no cause of the swap that built it), and the outgoing sink's error logged
    // after the swap.
    for (tag, err, reason) in [
        ("stall-says-stall", ErrLine::Product, Some(Some("stall"))),
        (
            "stall-says-timeout",
            ErrLine::Product,
            Some(Some("connect_timeout")),
        ),
        ("stall-no-reason", ErrLine::Product, Some(None)),
        ("stall-no-error", ErrLine::Missing, None),
        ("stall-newerr-only", ErrLine::At(2, -2_000), None),
        ("stall-lateerr", ErrLine::At(1, -1), None),
    ] {
        let mut w = dest_stall_err(0, 1, stalled(), err);
        if let Some(r) = reason {
            w.reason_line(2, r);
        }
        assert_fails(&judge(&w, "dest-stall", "", tag), "stall_swap", tag);
        checked += 1;
    }
    // The new sink's own error beside the outgoing one's changes nothing.
    let mut w = dest_stall(0, 1, stalled());
    let swap = w.at(20.0) + 800 + timeout + base + TICK_AFTER_BACKOFF_MS;
    w.line(
        swap + 2_000,
        "output gen=2 error: Failed to connect: Socket I/O timed out (gstrtmp2sink.c)",
    );
    assert_pass(
        &judge(&w, "dest-stall", "", "stall-newerr-too"),
        "stall-newerr-too",
    );
    checked += 1;
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
                w.line(at, "sink swap reason=error gen=3");
            }
            _ => w.relabel_reason(1, 2),
        }
        let v = judge(&w, "dest-stall", "", tag);
        assert_fails(&v, "stall_swap", tag);
        assert_eq!(v.json["measures"]["stall_swap"]["reasons_paired"], false, "{tag}");
        checked += 1;
    }
    // A report from before the reason was read reads reason "unknown", a red; one from before the
    // errors were read (fix round 3) reads no error, never one: a red too.
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
        v.json["measures"]["stall_swap"]["error_ms"],
        Value::Null,
        "stall-noerrfield"
    );
    checked += 2;
    // The rate limit held nothing: no backlog, or no netem qdisc at all.
    for (tag, stats) in [
        ("stall-nobacklog", netem(true, 0, 0, 0)),
        ("stall-noqdisc", netem(false, 0, 0, 0)),
    ] {
        let w = dest_stall(0, 1, stats);
        assert_fails(&judge(&w, "dest-stall", "", tag), "fault_applied", tag);
        checked += 1;
    }
    eprintln!("dest-stall sims checked: {checked}");
    assert_eq!(checked, 27);
}

#[test]
fn dest_trickle_swaps_under_a_trickle_and_reds_when_nothing_trickled() {
    // Case 12's qdisc in CI: 16 packets through at 8 kbit, one more queued.
    let trickling = || netem_queued(true, 19_271, 16, 0, 65_536, 1);
    let w = dest_stall(5_000, 1, trickling());
    let budget = w.ms("budget_output_reconnect_ms");
    // CI's case 12 (run 40106770881): sink 1's socket timeout, the reason line `error`, and the
    // back-off's swap (Owed 87).
    assert_pass(&judge(&w, "dest-trickle", "", "trickle-good"), "trickle-good");
    let w = dest_stall_in(5_000, 1, trickling(), ErrLine::Product, "backoff");
    assert_pass(
        &judge(&w, "dest-trickle", "backoff", "trickle-backoff"),
        "trickle-backoff",
    );
    let mut checked = 2;
    let base = w.ms("output_backoff_base_ms");
    let w = dest_stall_err(5_000, 1, trickling(), ErrLine::At(1, base - 1));
    assert_fails(
        &judge(&w, "dest-trickle", "", "trickle-err-early"),
        "stall_swap",
        "trickle-err-early",
    );
    checked += 1;
    // The spec's output budget from the error (§6 "≤5 s"): the swap placed a timeout later (x 2),
    // so that the error clears the cut's floor and the trickle keeps the growth beat close, and only
    // the budget is judged: the budget itself passes, 1 ms past it reds on it alone.
    let w = dest_stall_err(5_000, 2, trickling(), ErrLine::At(1, budget));
    assert_pass(
        &judge(&w, "dest-trickle", "", "trickle-err-budget"),
        "trickle-err-budget",
    );
    let w = dest_stall_err(5_000, 2, trickling(), ErrLine::At(1, budget + 1));
    let v = judge(&w, "dest-trickle", "", "trickle-err-late");
    assert_fails(&v, "stall_swap", "trickle-err-late");
    assert_eq!(v.json["failed"], json!(["stall_swap"]), "trickle-err-late");
    checked += 2;
    // Mutant 5: the netem line commented out: nothing trickles (a plain stall), and the qdisc is a
    // pass-through that never existed. Case 5's stall under case 12's name: the qdisc sent nothing.
    // One packet through: not a trickle the bytes can be said to have made. A trickle with no swap.
    for (tag, w) in [
        ("trickle-none", dest_stall(0, 1, netem(false, 0, 0, 0))),
        ("trickle-stalled", dest_stall(0, 1, stalled())),
        (
            "trickle-onepkt",
            dest_stall(0, 1, netem_queued(true, 1_448, 1, 0, 130_862, 45)),
        ),
        ("trickle-noswap", dest_stall(5_000, 0, trickling())),
    ] {
        assert_fails(&judge(&w, "dest-trickle", "", tag), "trickled", tag);
        checked += 1;
    }
    eprintln!("dest-trickle sims checked: {checked}");
    assert_eq!(checked, 9);
}

/// Case 17: [`dest_stall_in`]'s stall with a flat total between beats (a message that cannot leave
/// counts nothing), with the drive's send-buffer marks: the cap compose set (`wanted`), what the
/// namespace read back (`read`), each RTMP socket's buffer (`tb`) before the cut, and each one's
/// Send-Q 3 s into the fault (`queue`, measured, not judged: CI's ran past the cap, the PR #65
/// review's C-1).
fn dest_stall_rule(
    timeout_x: i64,
    err: ErrLine,
    read: Value,
    tb: Value,
    queue: Value,
    stats: Value,
) -> World {
    let w = dest_stall_in(0, timeout_x, stats, err, "pending");
    with_rule_marks(w, read, tb, queue)
}

/// Case 17's drive marks on `w`: the send buffer, the trickle's rate and the sockets in the fault.
fn with_rule_marks(mut w: World, read: Value, tb: Value, queue: Value) -> World {
    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(repo().join("contracts/fixtures/relay-job.v1/session-composed-720p.json"))
            .unwrap(),
    )
    .unwrap();
    let frame_bits =
        session["output"]["videoKbps"].as_i64().unwrap() * 1000 / session["output"]["fps"].as_i64().unwrap();
    let rate = frame_bits * 1000 / (2 * w.ms("output_sink_timeout_ms"));
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
fn dest_stall_rule_swaps_on_the_sinks_error_and_reds_on_a_slow_one_or_no_cap() {
    let capped = || json!([4096, 8192, 8192]);
    let trickling = || netem_queued(true, 4_344, 6, 0, 8_688, 6);
    // CI's world (runs 40121216406, 40121220366 and m9's baseline, PR #65): the cap set as asked,
    // the queue at 16 810 B past it, and the sink's own socket timeout 515 ms before the swap. Since
    // Owed 87 (the owner, 2026-10-24, "A") that is the pass; so is a queue the cap held.
    let mut checked = 0;
    for (tag, queue) in [("rule-ci", json!([16_810])), ("rule-held", json!([6_400]))] {
        let w = dest_stall_rule(1, ErrLine::Product, capped(), json!([8192]), queue, trickling());
        let v = judge(&w, "dest-stall-rule", "", tag);
        assert_pass(&v, tag);
        let m = &v.json["measures"]["stall_swap"];
        assert_eq!(m["reason"].as_str(), Some("error"), "{tag}: {m}");
        checked += 1;
    }
    // The sink's own wait at m9's 20 s (four timeouts; CI's m9 was closed by the destination first,
    // `rule-m9-ci` below), a detector twice as slow, and no swap at all: `stall_swap` reds, with the
    // controls `buffer_capped` and `trickled` passing (m9's leg reads them).
    for (tag, x) in [("rule-m9", 4), ("rule-slow", 2), ("rule-noswap", 0)] {
        let w = dest_stall_rule(
            x,
            ErrLine::Product,
            capped(),
            json!([8192]),
            json!([16_810]),
            trickling(),
        );
        let v = judge(&w, "dest-stall-rule", "", tag);
        assert_fails(&v, "stall_swap", tag);
        assert_eq!(
            v.json["failed"],
            json!(["stall_swap"]),
            "{tag}: the controls pass"
        );
        checked += 1;
    }
    // A detector faster than the sink's timeout, 3 s after the cut (a logic bug, not a value: within
    // the bound of the last growth), reds on the floor alone (the PR #65 review's m-3).
    let w = with_rule_marks(
        dest_stall_error_at(3_000, 0, trickling()),
        capped(),
        json!([8192]),
        json!([16_810]),
    );
    let v = judge(&w, "dest-stall-rule", "", "rule-early");
    assert_fails(&v, "stall_swap", "rule-early");
    let m = &v.json["measures"]["stall_swap"];
    assert!(
        m["from_growth_ms"]
            .as_f64()
            .is_some_and(|d| d <= m["bound_ms"].as_f64().unwrap())
            && m["reasons_paired"] == true
            && m["swap_beat_ms"].is_i64()
            && m["reason"] == "error",
        "rule-early must red on the floor alone: {m}"
    );
    checked += 1;
    // CI's m9 (run 40136067430, PR #74; the PR #73 review's I-3): with the sink's timeout at 20 s,
    // MediaMTX closed the publisher first, its message still unwritten (`connection closed
    // remotely` 10 998 ms after the cut, the swap 513 ms later). The destination's own deadline, not
    // a 20 s wait: `stall_swap` reds on the bound from the growth and on the ceiling from the cut,
    // with the controls passing. (`rule-m9` above, four timeouts, is the sink's own 20 s wait, a
    // physics CI did not show; it reds the same way.)
    let w = with_rule_marks(
        dest_stall_error_at(10_998, 0, trickling()),
        capped(),
        json!([8192]),
        json!([16_810]),
    );
    let v = judge(&w, "dest-stall-rule", "", "rule-m9-ci");
    assert_fails(&v, "stall_swap", "rule-m9-ci");
    assert_eq!(
        v.json["failed"],
        json!(["stall_swap"]),
        "rule-m9-ci: the controls pass"
    );
    let m = &v.json["measures"]["stall_swap"];
    assert!(
        m["from_growth_ms"].as_f64().unwrap() > m["bound_ms"].as_f64().unwrap()
            && m["error_from_cut_ms"].as_i64() == Some(10_998),
        "rule-m9-ci: CI's shape: {m}"
    );
    checked += 1;
    // An error later after the cut than the sink's timeout plus the output-lost budget, in a world
    // that kept growing until just before the swap (so the growth bound holds): it reds on the
    // cut-anchored ceiling alone (the PR #73 review's m-1).
    let ceiling = w.ms("output_sink_timeout_ms") + w.ms("budget_output_lost_max_ms");
    // The ceiling itself passes (the PR #73 confirm pass's c-2: `<=`, never `<`).
    let at_ceiling = with_rule_marks(
        dest_stall_error_at(ceiling, 1_000, trickling()),
        capped(),
        json!([8192]),
        json!([16_810]),
    );
    let v = judge(&at_ceiling, "dest-stall-rule", "", "rule-ceiling");
    assert_pass(&v, "rule-ceiling");
    assert_eq!(
        v.json["measures"]["stall_swap"]["error_from_cut_ms"].as_i64(),
        Some(ceiling),
        "rule-ceiling"
    );
    checked += 1;
    let late = ceiling + 1;
    let w = with_rule_marks(
        dest_stall_error_at(late, 1_000, trickling()),
        capped(),
        json!([8192]),
        json!([16_810]),
    );
    let v = judge(&w, "dest-stall-rule", "", "rule-late-error");
    assert_fails(&v, "stall_swap", "rule-late-error");
    let m = &v.json["measures"]["stall_swap"];
    assert!(
        m["from_growth_ms"].as_f64().unwrap() <= m["bound_ms"].as_f64().unwrap()
            && m["error_from_cut_ms"].as_i64() == Some(late)
            && m["reasons_paired"] == true
            && m["reason"] == "error",
        "rule-late-error must red on the ceiling alone: {m}"
    );
    checked += 1;
    // The retired reason, and no error line: each a red.
    let mut w = dest_stall_rule(
        1,
        ErrLine::Product,
        capped(),
        json!([8192]),
        json!([16_810]),
        trickling(),
    );
    w.reason_line(2, Some("stall"));
    assert_fails(
        &judge(&w, "dest-stall-rule", "", "rule-says-stall"),
        "stall_swap",
        "rule-says-stall",
    );
    let w = dest_stall_rule(
        1,
        ErrLine::Missing,
        capped(),
        json!([8192]),
        json!([16_810]),
        trickling(),
    );
    assert_fails(
        &judge(&w, "dest-stall-rule", "", "rule-no-error"),
        "stall_swap",
        "rule-no-error",
    );
    checked += 2;
    // The cap never applied: the namespace reads the kernel's default, a socket's buffer is past
    // the cap, no RTMP socket was seen at all, or ss could not read a buffer.
    for (tag, read, tb) in [
        ("rule-default", json!([4096, 16384, 4194304]), json!([8192])),
        ("rule-big-socket", capped(), json!([87_040])),
        ("rule-no-socket", capped(), json!([])),
        ("rule-unread-socket", capped(), json!([null])),
    ] {
        let w = dest_stall_rule(1, ErrLine::Product, read, tb, json!([16_810]), trickling());
        assert_fails(&judge(&w, "dest-stall-rule", "", tag), "buffer_capped", tag);
        checked += 1;
    }
    // The in-fault queue is measured, not judged: past the cap, unread or no socket seen, the case
    // passes on the sink's error.
    for (tag, queue) in [
        ("rule-queue-over", json!([8_193])),
        ("rule-queue-unread", json!([null])),
        ("rule-queue-none", json!([])),
    ] {
        let w = dest_stall_rule(
            1,
            ErrLine::Product,
            capped(),
            json!([8192]),
            queue.clone(),
            trickling(),
        );
        let v = judge(&w, "dest-stall-rule", "", tag);
        assert_pass(&v, tag);
        assert_eq!(v.json["measures"]["sockets_in_fault"]["send_q"], queue, "{tag}");
        checked += 1;
    }
    // The trickle's measured rate, netem's bytes over the 30 s fault: one frame per sink timeout is
    // 6 frames' bytes here. Unlimited (netem's rate 0 sends everything) and 5 % over red; 5 % under
    // passes.
    let w = dest_stall_rule(
        1,
        ErrLine::Product,
        capped(),
        json!([8192]),
        json!([16_810]),
        trickling(),
    );
    let frame_bytes = w.marks.iter().find(|m| m.0 == "rate").unwrap().2["frame_bits"]
        .as_i64()
        .unwrap()
        / 8;
    let frames = (w.at(50.0) - w.at(20.0)) / w.ms("output_sink_timeout_ms");
    let per_timeout = |x: f64| ((frame_bytes * frames) as f64 * x) as u64;
    for (tag, sent, red) in [
        ("rule-unlimited", 15_000_000, true),
        ("rule-fast", per_timeout(1.05), true),
        ("rule-edge", per_timeout(0.95), false),
    ] {
        let w = dest_stall_rule(
            1,
            ErrLine::Product,
            capped(),
            json!([8192]),
            json!([16_810]),
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
    let w = dest_stall_rule(
        1,
        ErrLine::Product,
        capped(),
        json!([8192]),
        json!([16_810]),
        stalled(),
    );
    assert_fails(
        &judge(&w, "dest-stall-rule", "", "rule-stalled"),
        "trickled",
        "rule-stalled",
    );
    checked += 1;
    eprintln!("dest-stall-rule sims checked: {checked}");
    assert_eq!(checked, 22);
}

/// Case 18's drive schedule (output-hung.sh's OUTPUT_HUNG_AFTER_MS): the plant's offset from sink 1's
/// build, read from the case file so the sims and the case cannot drift apart.
fn output_hung_after_ms() -> i64 {
    let text = std::fs::read_to_string(repo().join("harness/cases/output-hung.sh")).unwrap();
    let v: Vec<i64> = text
        .lines()
        .filter_map(|l| l.strip_prefix("OUTPUT_HUNG_AFTER_MS="))
        .map(|v| v.parse().expect("OUTPUT_HUNG_AFTER_MS is a number"))
        .collect();
    assert_eq!(v.len(), 1, "output-hung.sh sets OUTPUT_HUNG_AFTER_MS once: {v:?}");
    v[0]
}

/// MediaMTX's first frame from sink 2 after its build (a connect to the local server).
const HUNG_FIRST_FRAME_MS: i64 = 300;

/// A threshold or budget from harness/thresholds.json, the file the verdicts read.
fn threshold_ms(key: &str) -> i64 {
    let t: Value =
        serde_json::from_str(&std::fs::read_to_string(repo().join("harness/thresholds.json")).unwrap())
            .unwrap();
    t[key]
        .as_f64()
        .unwrap_or_else(|| panic!("thresholds.json has no {key}")) as i64
}

/// A duration as MediaMTX's API prints it (`conf.Duration`, internal/conf/duration.go at v1.21.2: a
/// day count `<n>d` when there is one, then Go's `time.Duration` String()): `1h1m0s`, `1m0s`, `10s`,
/// and a fraction after the seconds, trailing zeros dropped (`59.999s`). Written from Go's format,
/// independent of lib.jq's reader; at least a second and under a day (no `ms` or `d` form here).
fn go_duration(ms: i64) -> String {
    assert!(
        (1_000..86_400_000).contains(&ms),
        "a duration this writes: {ms} ms"
    );
    let (h, m, s, frac) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1_000 % 60, ms % 1_000);
    let secs = if frac == 0 {
        s.to_string()
    } else {
        format!("{s}.{}", format!("{frac:03}").trim_end_matches('0'))
    };
    if h > 0 {
        format!("{h}h{m}m{secs}s")
    } else if m > 0 {
        format!("{m}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

/// The readTimeout case 18 asks MediaMTX for, in ms (output-hung.sh's MTX_READ_TIMEOUT_MS), as its
/// verdict prints it: judged once, over a world with no MediaMTX reading (which reds on that alone).
fn mtx_asked_ms() -> i64 {
    static ASKED: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *ASKED.get_or_init(|| {
        let w = output_hung_bare("hold", output_hung_after_ms(), hung_to_swap());
        let v = judge(&w, "output-hung", "hold", "hung-asked");
        assert_eq!(
            v.json["failed"],
            json!(["destination_outwaits"]),
            "hung-asked: no reading reds on it alone"
        );
        v.json["measures"]["destination_outwaits"]["asked_ms"]
            .as_i64()
            .expect("the verdict prints asked_ms")
    })
}

/// Case 18's world (Owed 94): the compositor armed to stall sink 1 (`stall`) the case's
/// OUTPUT_HUNG_AFTER_MS after its build (boot()'s first swap), the plant `after` ms after that build
/// (the drive's schedule, unless a sim moves it), and the backstop's swap `to_swap` ms after
/// the plant line, both on the process's own clock: its reason `egress_stalled`, sink 2 connecting
/// and earning Connected output_connected_after_ms after its first bytes. The beats from the
/// backstop's firing (one back-off before the swap) until then read `stalled`; MediaMTX has no frame
/// from just after the plant until sink 2's first. MediaMTX reports the readTimeout the case asks
/// for (the `mtx_read_timeout` mark, in its API's format).
fn output_hung_at(stall: &str, after: i64, to_swap: i64) -> World {
    let mut w = output_hung_bare(stall, after, to_swap);
    let at = w.drive + 200;
    w.mark("mtx_read_timeout", at, json!(go_duration(mtx_asked_ms())));
    w
}

/// `output_hung_at`'s world with no MediaMTX reading.
fn output_hung_bare(stall: &str, after: i64, to_swap: i64) -> World {
    let mut w = World::new(70);
    let start = w.live - 9_000;
    let build = start + 2_400;
    let armed = output_hung_after_ms();
    w.line(
        start + 300,
        format!("WARN test hook (Owed 94): sink gen=1 stalls ({stall}) {armed} ms after its build"),
    );
    let planted = build + after;
    w.line(
        planted,
        format!("WARN test hook (Owed 94): sink stall={stall} gen=1 planted=true"),
    );
    let swap = planted + to_swap;
    let fire = swap - w.ms("output_backoff_base_ms");
    let connected = swap + HUNG_FIRST_FRAME_MS + w.ms("output_connected_after_ms");
    w.swap(swap, 2, "egress_stalled");
    for (at, body) in &mut w.beats {
        if body["state"] != "playing" {
            continue;
        }
        if *at >= fire && *at < connected {
            body["state"] = json!("stalled");
            body["output"] = json!(if *at < swap { "backoff" } else { "connecting" });
        }
        body["sinkSwaps"] = json!(i64::from(*at >= swap));
    }
    w.outages.push((planted + 30, swap + HUNG_FIRST_FRAME_MS));
    w
}

/// The backstop's swap inside its window: the readings' half poll after the plant, then the
/// threshold, a tick and the back-off.
fn output_hung(stall: &str) -> World {
    output_hung_at(stall, output_hung_after_ms(), hung_to_swap())
}

/// The good world's plant to swap: `output_hung`'s.
fn hung_to_swap() -> i64 {
    threshold_ms("output_egress_flat_ms")
        + threshold_ms("output_progress_poll_ms") / 2
        + TICK_AFTER_BACKOFF_MS
        + threshold_ms("output_backoff_base_ms")
}

#[test]
fn output_hung_swaps_on_the_egress_backstop_and_reds_on_any_other_path() {
    let w = World::new(1);
    let (flat, base, poll, tick, budget) = (
        w.ms("output_egress_flat_ms"),
        w.ms("output_backoff_base_ms"),
        w.ms("output_progress_poll_ms"),
        w.ms("tick_widest_ms"),
        w.ms("output_gap_budget_ms"),
    );
    // The domain's window (lib.jq's `hung_swap`), from its terms: the last growth up to a poll
    // before the plant and a tick for the plant line; up to two polls and two ticks after.
    let (floor, ceiling) = (flat + base - poll - tick, flat + base + 2 * poll + 2 * tick);
    assert!(
        floor > w.ms("output_sink_timeout_ms") && ceiling < budget,
        "{floor}..{ceiling}"
    );
    let mut checked = 0;
    for stall in ["hold", "hang"] {
        let w = output_hung(stall);
        let tag = format!("hung-{stall}-good");
        let v = judge(&w, "output-hung", stall, &tag);
        assert_pass(&v, &tag);
        let m = &v.json["measures"];
        let s = &m["backstop_swap"];
        let to_swap = flat + poll / 2 + TICK_AFTER_BACKOFF_MS + base;
        assert_eq!(
            (
                s["reason"].as_str(),
                s["swap_gen"].as_i64(),
                s["planted_to_swap_ms"].as_i64()
            ),
            (Some("egress_stalled"), Some(2), Some(to_swap)),
            "{tag}: {s}"
        );
        assert_eq!(
            (s["floor_ms"].as_i64(), s["ceiling_ms"].as_i64()),
            (Some(floor), Some(ceiling)),
            "{tag}: the window's floor and ceiling"
        );
        let gap = m["gap_ms"].as_i64().unwrap();
        assert!(gap > flat && gap <= budget, "{tag}: gap {gap}");
        // The PR #76 review's I-1: the idle deadline the case asks MediaMTX for is past the window's
        // ceiling here and at the owner's question's "about 15 s" (a threshold of a whole budget), so
        // no destination deadline races the backstop. The confirm pass's n-1: the check reads what
        // MediaMTX's running config reports (the mark), in ms, never the number the case derived.
        let d = &m["destination_outwaits"];
        let asked = mtx_asked_ms();
        assert_eq!(
            (
                d["read"].as_str(),
                d["read_ms"].as_i64(),
                d["asked_ms"].as_i64(),
                d["ceiling_ms"].as_i64()
            ),
            (
                Some(go_duration(asked).as_str()),
                Some(asked),
                Some(asked),
                Some(ceiling)
            ),
            "{tag}: the destination's deadline"
        );
        assert!(
            ceiling < asked && budget + ceiling - flat < asked,
            "{tag}: the asked {asked} ms outwaits this window and a 15 s threshold's"
        );
        // The confirm pass's n-2: the good world loses no programme beyond the outage (within a
        // frame of the camera's emits).
        let lost = m["lost_ms"].as_i64().unwrap();
        assert!(lost.abs() <= PERIOD_MS.ceil() as i64, "{tag}: lost {lost}");
        checked += 1;
    }
    // Judged as the other variant: the arming line and the plant name another stall.
    let v = judge(&output_hung("hold"), "output-hung", "hang", "hung-hold-as-hang");
    assert_fails(&v, "armed", "hung-hold-as-hang");
    assert_fails(&v, "planted", "hung-hold-as-hang");
    checked += 1;
    // Unarmed (the plain relay image): no arming line, no plant, no backstop swap.
    let mut w = output_hung("hold");
    w.log.retain(|(_, m)| !m.contains("test hook (Owed 94)"));
    let v = judge(&w, "output-hung", "hold", "hung-unarmed");
    for check in ["armed", "planted", "planted_in_drive", "backstop_swap"] {
        assert_fails(&v, check, "hung-unarmed");
    }
    checked += 1;
    // Armed, but the stall found no sink 1 to go in on.
    let mut w = output_hung("hold");
    for l in &mut w.log {
        if l.1.ends_with("planted=true") {
            l.1 = l.1.replace("planted=true", "planted=false");
        }
    }
    let v = judge(&w, "output-hung", "hold", "hung-not-planted");
    assert_fails(&v, "planted", "hung-not-planted");
    assert_eq!(v.json["failed"], json!(["planted"]), "hung-not-planted");
    // Armed for another moment than the drive's; and a second plant.
    let mut w = output_hung("hold");
    let armed = output_hung_after_ms();
    for l in &mut w.log {
        if l.1.ends_with(" ms after its build") {
            l.1 =
                l.1.replace(&format!(" {armed} ms "), &format!(" {} ms ", armed + 1));
        }
    }
    let v = judge(&w, "output-hung", "hold", "hung-armed-other-ms");
    assert_eq!(v.json["failed"], json!(["armed"]), "hung-armed-other-ms");
    let mut w = output_hung("hold");
    let planted = w.log.iter().find(|l| l.1.ends_with("planted=true")).unwrap().0;
    w.line(
        planted + 1_000,
        "WARN test hook (Owed 94): sink stall=hold gen=1 planted=true",
    );
    let v = judge(&w, "output-hung", "hold", "hung-planted-twice");
    assert_eq!(v.json["failed"], json!(["planted"]), "hung-planted-twice");
    checked += 3;
    // The plant inside the drive, with room before the end: each boundary reds on that alone, 1 ms
    // inside it passes (World: boot 9 s before live, sink 1 at +2.4 s, the drive 1 s after live, the
    // end 70 s after the drive). The room is the latest swap the window accepts, then
    // `beat_playing_again`'s wait and a heartbeat for its beat (longer than the outage's budget).
    let to_swap = flat + poll / 2 + TICK_AFTER_BACKOFF_MS + base;
    let room = ceiling
        + w.ms("output_connect_timeout_ms")
        + w.ms("output_connected_after_ms")
        + 2 * tick
        + w.ms("heartbeat_interval_ms");
    assert!(room > budget, "room {room} ms");
    let at_drive = 9_000 - 2_400 + 1_000;
    let at_end = at_drive + 70_000 - room;
    for (tag, after, red) in [
        ("hung-at-drive", at_drive, true),
        ("hung-after-drive", at_drive + 1, false),
        ("hung-no-room", at_end, true),
        ("hung-room", at_end - 1, false),
    ] {
        let v = judge(
            &output_hung_at("hold", after, to_swap),
            "output-hung",
            "hold",
            tag,
        );
        if red {
            assert_fails(&v, "planted_in_drive", tag);
            assert_eq!(v.json["failed"], json!(["planted_in_drive"]), "{tag}");
        } else {
            assert_pass(&v, tag);
        }
        checked += 1;
    }
    // The window, on the process's own clock: each bound passes, 1 ms past it reds on it alone.
    for (tag, to_swap, red) in [
        ("hung-floor", floor, false),
        ("hung-early", floor - 1, true),
        ("hung-ceiling", ceiling, false),
        ("hung-late", ceiling + 1, true),
    ] {
        let w = output_hung_at("hold", output_hung_after_ms(), to_swap);
        let v = judge(&w, "output-hung", "hold", tag);
        if red {
            assert_fails(&v, "backstop_swap", tag);
            assert_eq!(v.json["failed"], json!(["backstop_swap"]), "{tag}");
        } else {
            assert_pass(&v, tag);
        }
        checked += 1;
    }
    // The window is the process's interval: docker reading the plant line 2 ms late moves nothing.
    let mut w = output_hung_at("hold", output_hung_after_ms(), floor);
    let planted = w.log.iter().find(|l| l.1.ends_with("planted=true")).unwrap().0;
    w.docker_late(planted, "WARN test hook (Owed 94): sink stall=", 2);
    assert_pass(
        &judge(&w, "output-hung", "hold", "hung-floor-docker-late"),
        "hung-floor-docker-late",
    );
    checked += 1;
    // Any other path to the swap reds: the error's reason, the outgoing sink's error line before
    // the swap, no reason line, and no swap at all.
    let mut w = output_hung("hold");
    w.reason_line(2, Some("error"));
    assert_fails(
        &judge(&w, "output-hung", "hold", "hung-says-error"),
        "backstop_swap",
        "hung-says-error",
    );
    let mut w = output_hung("hold");
    let swap = w
        .log
        .iter()
        .find(|l| l.1 == "sink swap gen=2 target=rtmp")
        .unwrap()
        .0;
    w.line(
        swap - base,
        "output gen=1 error: Connection error: Socket I/O timed out (gstrtmp2sink.c)",
    );
    assert_fails(
        &judge(&w, "output-hung", "hold", "hung-error-first"),
        "backstop_swap",
        "hung-error-first",
    );
    let mut w = output_hung("hold");
    w.reason_line(2, None);
    assert_fails(
        &judge(&w, "output-hung", "hold", "hung-no-reason"),
        "backstop_swap",
        "hung-no-reason",
    );
    // The reason lines must number the swap lines: the boot's missing, sink 2's still `egress_stalled`.
    let mut w = output_hung("hold");
    w.reason_line(1, None);
    let v = judge(&w, "output-hung", "hold", "hung-unpaired");
    assert_fails(&v, "backstop_swap", "hung-unpaired");
    assert_eq!(v.json["failed"], json!(["backstop_swap"]), "hung-unpaired");
    assert_eq!(v.json["measures"]["backstop_swap"]["reason"], "egress_stalled");
    let mut w = output_hung("hold");
    w.log
        .retain(|(_, m)| !m.starts_with("sink swap") || m.ends_with("gen=1") || m.contains("gen=1 "));
    let v = judge(&w, "output-hung", "hold", "hung-no-swap");
    assert_fails(&v, "backstop_swap", "hung-no-swap");
    assert_fails(&v, "one_swap", "hung-no-swap");
    checked += 5;
    // A second swap after sink 2 (its successor failed too).
    let mut w = output_hung("hold");
    w.swap(swap + 20_000, 3, "error");
    w.line(
        swap + 20_000 - base,
        "output gen=2 error: Connection error: Socket I/O timed out (gstrtmp2sink.c)",
    );
    let v = judge(&w, "output-hung", "hold", "hung-two-swaps");
    assert_fails(&v, "one_swap", "hung-two-swaps");
    assert_eq!(v.json["failed"], json!(["one_swap"]), "hung-two-swaps");
    checked += 1;
    // A beat in the window reading playing over the dead output, and a window with no beat.
    let mut w = output_hung("hold");
    let (fire, earned) = (swap - base, swap + w.ms("output_connected_after_ms"));
    let window = |at: i64| at >= fire + 2 * tick && at < earned - 2 * tick;
    let inside = w.beats.iter().filter(|b| window(b.0)).count();
    assert_eq!(inside, 1, "one beat in the window at a 5 s beat");
    for b in &mut w.beats {
        if window(b.0) {
            b.1["state"] = json!("playing");
        }
    }
    let v = judge(&w, "output-hung", "hold", "hung-beat-playing");
    assert_fails(&v, "beat_stalled", "hung-beat-playing");
    assert_eq!(v.json["failed"], json!(["beat_stalled"]), "hung-beat-playing");
    let mut w = output_hung("hold");
    w.beats.retain(|b| !window(b.0));
    let v = judge(&w, "output-hung", "hold", "hung-no-beat");
    assert_fails(&v, "beat_stalled", "hung-no-beat");
    checked += 2;
    // The gap past the owner's 15 s target reds on it alone (the good worlds pass under it; the
    // checker dates an outage on its frames, so the target itself is not a sim's exact reading).
    let mut w = output_hung("hold");
    let planted = w.log.iter().find(|l| l.1.ends_with("planted=true")).unwrap().0;
    w.outages = vec![(planted + 30, planted + 30 + budget + 100)];
    let v = judge(&w, "output-hung", "hold", "hung-gap-over");
    assert_fails(&v, "gap_in_budget", "hung-gap-over");
    assert_eq!(v.json["failed"], json!(["gap_in_budget"]), "hung-gap-over");
    assert!(v.json["measures"]["gap_ms"].as_i64().unwrap() > budget);
    // Frames through mid-hang (two outages between the plant and the swap): nothing passes a held or
    // hung sink, so the gap is one outage or the hook did not hold.
    let mut w = output_hung("hold");
    w.outages = vec![
        (planted + 30, planted + 3_000),
        (planted + 3_500, swap + HUNG_FIRST_FRAME_MS),
    ];
    let v = judge(&w, "output-hung", "hold", "hung-gap-split");
    assert_eq!(v.json["failed"], json!(["gap_in_budget"]), "hung-gap-split");
    checked += 2;
    let mut w = output_hung("hold");
    let planted = w.log.iter().find(|l| l.1.ends_with("planted=true")).unwrap().0;
    w.outages.push((planted - 8_000, planted - 6_000));
    let v = judge(&w, "output-hung", "hold", "hung-outage-before");
    assert_fails(&v, "no_outage_outside_hang", "hung-outage-before");
    assert_eq!(
        v.json["failed"],
        json!(["no_outage_outside_hang"]),
        "hung-outage-before"
    );
    // And one after sink 2 was back (a reconnect's budget past the swap).
    let mut w = output_hung("hold");
    let back = swap + w.ms("budget_output_reconnect_ms");
    w.outages.push((back, back + 2_000));
    let v = judge(&w, "output-hung", "hold", "hung-outage-after");
    assert_fails(&v, "no_outage_outside_hang", "hung-outage-after");
    assert_eq!(
        v.json["failed"],
        json!(["no_outage_outside_hang"]),
        "hung-outage-after"
    );
    checked += 2;
    // The PR #76 review's m-3: once sink 2 could have earned Connected at the latest (a whole connect,
    // the Connected bound and a tick for each after the swap), the beats read `playing` again until
    // the end request. A beat just before that may still read stalled, one at it may not, and a window
    // with no beat witnesses nothing.
    let again = swap + w.ms("output_connect_timeout_ms") + w.ms("output_connected_after_ms") + 2 * tick;
    let with_stalled_beat = |at: i64| {
        let mut w = output_hung("hold");
        let mut body = w
            .beats
            .iter()
            .find(|b| b.1["state"] == "playing")
            .unwrap()
            .1
            .clone();
        body["state"] = json!("stalled");
        body["output"] = json!("connecting");
        w.beats.push((at, body));
        w.beats.sort_by_key(|b| b.0);
        w
    };
    let v = judge(
        &with_stalled_beat(again - 1),
        "output-hung",
        "hold",
        "hung-again-before",
    );
    assert_pass(&v, "hung-again-before");
    let v = judge(
        &with_stalled_beat(again),
        "output-hung",
        "hold",
        "hung-again-stalled",
    );
    assert_eq!(
        v.json["failed"],
        json!(["beat_playing_again"]),
        "hung-again-stalled"
    );
    let mut w = output_hung("hold");
    let end_request = w.end;
    let inside = w
        .beats
        .iter()
        .filter(|b| b.0 >= again && b.0 < end_request)
        .count();
    assert!(inside >= 1, "the good world has {inside} beats in the window");
    w.beats.retain(|b| !(b.0 >= again && b.0 < end_request));
    let v = judge(&w, "output-hung", "hold", "hung-again-no-beat");
    assert_fails(&v, "beat_playing_again", "hung-again-no-beat");
    checked += 3;
    // The confirm pass's n-1: the destination's deadline is what MediaMTX's running config reports,
    // so a setting it ignored reds (`MTX_READTIMEOUT` under a renamed key or an image bump leaves
    // MediaMTX's default, 10 s: conf.go's setDefaults at v1.21.2), as do a reading 1 ms short of what
    // the case asks, one in no unit, one with text after its parts, and none at all; one past it
    // passes.
    let asked = mtx_asked_ms();
    for (tag, read, red) in [
        ("hung-mtx-default", Some("10s".to_owned()), true),
        ("hung-mtx-short", Some(go_duration(asked - 1)), true),
        ("hung-mtx-unitless", Some(asked.to_string()), true),
        (
            "hung-mtx-junk",
            Some(format!("{} junk", go_duration(asked + 3_600_000))),
            true,
        ),
        ("hung-mtx-unread", None, true),
        ("hung-mtx-longer", Some(go_duration(asked + 3_600_000)), false),
    ] {
        let mut w = output_hung("hold");
        w.marks.retain(|m| m.0 != "mtx_read_timeout");
        if let Some(read) = read {
            let at = w.drive + 200;
            w.mark("mtx_read_timeout", at, json!(read));
        }
        let v = judge(&w, "output-hung", "hold", tag);
        if red {
            assert_eq!(v.json["failed"], json!(["destination_outwaits"]), "{tag}");
        } else {
            assert_pass(&v, tag);
        }
        checked += 1;
    }
    // The PR #84 review's m-2: every row of `mtx_duration_ms`'s unit table. Each reading's ms is worked
    // out by hand from its unit (1 d = 86 400 000 ms, 1 h = 3 600 000, 1 s = 1 000, 1 ms = 1,
    // 1 µs = 1/1 000, 1 ns = 1/1 000 000; Go's String() writes `µs`, its parser also takes `us`), and
    // judged against what the case asks: under it reds `destination_outwaits` alone, at or past it
    // passes. "500ms" is the fail-open one: read in any longer unit it would pass.
    for (tag, read, by_hand) in [
        ("hung-mtx-ms", "500ms", 500.0),
        ("hung-mtx-h", "1h", 3_600_000.0),
        ("hung-mtx-s", "90s", 90_000.0),
        ("hung-mtx-d", "1d", 86_400_000.0),
        ("hung-mtx-us", "60000000us", 60_000.0),
        ("hung-mtx-micro", "59999999µs", 59_999.999),
        ("hung-mtx-ns", "60000000000ns", 60_000.0),
        ("hung-mtx-ns-short", "59999999999ns", 59_999.999_999),
    ] {
        let mut w = output_hung("hold");
        w.marks.retain(|m| m.0 != "mtx_read_timeout");
        let at = w.drive + 200;
        w.mark("mtx_read_timeout", at, json!(read));
        let v = judge(&w, "output-hung", "hold", tag);
        let got = v.json["measures"]["destination_outwaits"]["read_ms"].as_f64();
        assert!(
            got.is_some_and(|g| (g - by_hand).abs() < 1e-6),
            "{tag}: {read} read as {got:?} ms, by hand {by_hand}"
        );
        if by_hand < asked as f64 {
            assert_eq!(v.json["failed"], json!(["destination_outwaits"]), "{tag}");
        } else {
            assert_pass(&v, tag);
        }
        checked += 1;
    }
    // The confirm pass's n-2: the programme lost beyond the hang's outage is inside spec §6's output
    // stall row ("≤2 s of programme lost", budget_output_lost_max_ms), as cases 4, 5 and 12 judge
    // theirs. At the budget passes, 1 ms past it reds, and an unread loss reds (jq sorts null below
    // every number, so `<=` alone would pass it).
    let lost_budget = threshold_ms("budget_output_lost_max_ms");
    fn lost_to(r: &mut Value, over: Option<i64>) {
        let b = threshold_ms("budget_output_lost_max_ms");
        for o in r["output_outages"].as_array_mut().unwrap() {
            o["lost_ms"] = over.map_or(Value::Null, |x| json!(b + x));
        }
    }
    type Patch = fn(&mut Value);
    let patches: [(&str, Patch, bool); 3] = [
        ("hung-lost-at-budget", |r| lost_to(r, Some(0)), false),
        ("hung-lost-over", |r| lost_to(r, Some(1)), true),
        ("hung-lost-unread", |r| lost_to(r, None), true),
    ];
    for (tag, patch, red) in patches {
        let mut w = output_hung("hold");
        w.report_patch = Some(patch);
        let v = judge(&w, "output-hung", "hold", tag);
        if red {
            assert_eq!(v.json["failed"], json!(["output_lost"]), "{tag}");
        } else {
            assert_pass(&v, tag);
        }
        checked += 1;
    }
    // And through the checker: the camera stopped emitting before the plant, short of the slate (the
    // programme repeats its last frame), so the counter gap across the outage outruns its wall span by
    // the camera's silence less its latency, here 500 ms past the budget.
    let mut w = output_hung("hold");
    let planted = w.log.iter().find(|l| l.1.ends_with("planted=true")).unwrap().0;
    let silent = lost_budget + LATENCY_MS as i64 + 500;
    assert!(
        ((silent - LATENCY_MS as i64) as f64) < w.slate_after_ms,
        "the silence {silent} ms stays short of the slate"
    );
    w.camera_gaps.push((planted + 30 - silent, planted + 30));
    let v = judge(&w, "output-hung", "hold", "hung-lost-camera-silent");
    assert_eq!(
        v.json["failed"],
        json!(["output_lost"]),
        "hung-lost-camera-silent"
    );
    let lost = v.json["measures"]["lost_ms"].as_i64().unwrap();
    assert!(lost > lost_budget, "hung-lost-camera-silent: lost {lost}");
    checked += 1;
    // The PR #84 review's m-1: spec §6's "≤2 s of programme lost" is the recovery target of one output
    // stall, so it bounds the programme lost over every outage the hang causes (the window
    // `no_outage_outside_hang` excuses, from the plant to a reconnect's budget past the swap), summed,
    // a negative reading counting as none (Owed 92: the dating's, never programme regained). With a
    // second outage in sink 2's first seconds: both small passes; the second alone over the budget
    // reds; a −budget first reading does not hide it; two under the budget whose sum is over it red.
    // And no outage in the window at all reds.
    let second = (1_000, 1_500);
    assert!(
        second.1 < w.ms("budget_output_reconnect_ms"),
        "the second outage is inside the window"
    );
    let with_second = || {
        let mut w = output_hung("hold");
        let swap = w
            .log
            .iter()
            .find(|l| l.1 == "sink swap gen=2 target=rtmp")
            .unwrap()
            .0;
        w.outages.push((swap + second.0, swap + second.1));
        w
    };
    fn two_lost(r: &mut Value, f: fn(i64) -> (i64, i64)) {
        let (a, b) = f(threshold_ms("budget_output_lost_max_ms"));
        let o = r["output_outages"].as_array_mut().unwrap();
        assert_eq!(o.len(), 2, "two outages: {o:?}");
        o[0]["lost_ms"] = json!(a);
        o[1]["lost_ms"] = json!(b);
    }
    let v = judge(&with_second(), "output-hung", "hold", "hung-lost-two-under");
    assert_pass(&v, "hung-lost-two-under");
    let lost = &v.json["measures"]["output_lost"]["lost_ms"];
    assert_eq!(
        lost.as_array().map(Vec::len),
        Some(2),
        "hung-lost-two-under: both outages judged: {lost}"
    );
    checked += 1;
    // The PR #86 review's n-1: a negative reading counts as none, never as a loss. Owed 92's dating
    // (−budget) beside a loss at the budget passes: the programme lost is the budget, where counting
    // the negative's size as lost would read twice it.
    let mut w = with_second();
    w.report_patch = Some(|r| two_lost(r, |b| (-b, b)));
    let v = judge(&w, "output-hung", "hold", "hung-lost-dating");
    assert_pass(&v, "hung-lost-dating");
    assert_eq!(
        v.json["measures"]["output_lost"]["programme_lost_ms"].as_i64(),
        Some(lost_budget),
        "hung-lost-dating: the programme lost"
    );
    checked += 1;
    let pairs: [(&str, Patch); 3] = [
        ("hung-lost-second-over", |r| two_lost(r, |b| (0, b + 1))),
        ("hung-lost-masked", |r| two_lost(r, |b| (-b, b + 1))),
        ("hung-lost-sum-over", |r| two_lost(r, |b| (b / 2 + 1, b - b / 2))),
    ];
    for (tag, patch) in pairs {
        let mut w = with_second();
        w.report_patch = Some(patch);
        let v = judge(&w, "output-hung", "hold", tag);
        assert_eq!(v.json["failed"], json!(["output_lost"]), "{tag}");
        checked += 1;
    }
    let mut w = output_hung("hold");
    w.outages.clear();
    let v = judge(&w, "output-hung", "hold", "hung-lost-no-outage");
    assert_fails(&v, "output_lost", "hung-lost-no-outage");
    checked += 1;
    eprintln!("output-hung sims checked: {checked}");
    assert_eq!(checked, 55);
}

/// What the starvation did to the compositor (case 6). The honest beat (Owed 86; the owner,
/// 2026-10-24, "Ok", relayed by the coordinator): the beat's fps is the encoder's real output rate
/// over the beat window, with a starvation alarm, and the ladder is not changed.
#[derive(Clone, Copy, PartialEq)]
enum Felt {
    /// Nothing: encoderSpeed 1.0, droppedFrames 0, rung full and every beat at the profile's rate, so
    /// no alarm (CI's first build, a sibling container's burn).
    No,
    /// The honest beat: the encoder behind realtime (encoderSpeed 0.8 in the window), each beat's
    /// rate the frames out of it over its window, so under the floor while the quota holds, and the
    /// alarm raised, then cleared once the rate is back; droppedFrames 0 and rung full (CI's run
    /// 40143263604).
    Honest,
    /// F-024's beat: the encoder behind realtime and every beat at the profile's rate (the tick
    /// grid's push count, before #69), so the alarm never fires.
    Lying,
    /// Honest, and the ladder stepped: overruns past its step, then overlay_half_rate.
    Step,
    /// Honest, and a step to overlay_half_rate with no overrun counted.
    Uncounted,
}

/// The starvation alarm's lines: as declared (`domain::starvation`, at thresholds.json's levels), or
/// one of its regressions, each a product mutant.
#[derive(Clone, Copy, PartialEq, Debug)]
enum AlarmIs {
    Declared,
    /// Never raised: its floor 0, which no rate is below.
    NeverRaised,
    /// Raised, never cleared: its recovered level above the profile's rate.
    NeverCleared,
    /// Raised on the healthy clock before the quota: its floor above the profile's rate, so a healthy
    /// beat raises it and the next one clears it.
    FalseRaise,
    /// Silent through the window (its lines there lost), then raised by the drain's short last window
    /// (Owed 91's mid-drain gap).
    RaisedLate,
    /// Cleared at the floor (the PR #69 review's clear-at-floor mutant), with the rate back only to
    /// between the floor and the recovered level.
    ClearsAtFloor,
    /// Cleared on the first beat past realtime_after's window, though the rate was back sooner.
    ClearsLate,
    /// Cleared inside the window, on the beat after the raise: its recovered level under the
    /// starved rate, so it flaps raise, clear, raise through the quota (the PR #77 review's I2).
    ClearsEarly,
}

/// The beat's rate beyond the quota's own shape: each a regression, or a phase the verdict must
/// tell from the starvation's (the PR #77 review's I2 and m1).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Shape {
    /// The quota's shape only.
    Quota,
    /// One beat inside the window at the profile's rate among starved ones.
    OneFull,
    /// One beat before the quota under the floor (the PR #69 review's m4: a short off-grid window
    /// at startup), then the rate back: the alarm raises and clears before the quota.
    StartupDip,
    /// The rate back after the quota, then a dip 0.1 under the floor inside realtime_after's window:
    /// the product re-raises, a catch-up that does not hold.
    SagsAfter,
    /// The rate back after the quota, then one healthy beat a few frames short (0.1 under the
    /// recovered level, over the floor) inside realtime_after's window: no re-raise, a pass.
    HealthyDip,
}

/// The rate CI's case 6 read while the 0.75 quota held (run 40143263604: 17.6 to 20.27 fps over its 7
/// judged beats). Sim physics only: no verdict reads it.
const STARVED_FPS: f64 = 20.0;

/// The honest beat's three lines (app/src/driver.rs; logfacts pins their formats).
const BEAT_LINE: &str = "output fps beat=";
const RAISED_LINE: &str = "output starvation alarm raised ";
const CLEARED_LINE: &str = "output starvation alarm cleared ";

/// The honest beat's log lines for `readings`, under an alarm raised below `floor` and cleared at or
/// above `recovered` (`domain::starvation`'s rule as its doc declares it), in the driver's formats.
fn honest_lines(readings: &[(i64, f64)], floor: f64, recovered: f64) -> Vec<(i64, String)> {
    let mut raised = false;
    let mut out = Vec::new();
    for &(at, fps) in readings {
        out.push((at, format!("{BEAT_LINE}{fps:.2} floor={floor:.2}")));
        if !raised && fps < floor {
            raised = true;
            out.push((at, format!("{RAISED_LINE}fps={fps:.2} floor={floor:.2}")));
        } else if raised && fps >= recovered {
            raised = false;
            out.push((at, format!("{CLEARED_LINE}fps={fps:.2} recovered={recovered:.2}")));
        }
    }
    out
}

fn level(w: &World, key: &str) -> f64 {
    w.t[key]
        .as_f64()
        .unwrap_or_else(|| panic!("thresholds.json has no {key}"))
}

/// The quota at three quarters of the compositor's 1.4-CPU demand from 20 s for 40 s, put back with
/// `done` (the drive's `stress_done`), the alarm as declared.
fn cpu_starved(done: bool, rung_back: bool, felt: Felt) -> World {
    cpu_starved_alarm(done, rung_back, felt, AlarmIs::Declared)
}

fn cpu_starved_alarm(done: bool, rung_back: bool, felt: Felt, alarm: AlarmIs) -> World {
    cpu_starved_shaped(done, rung_back, felt, alarm, Shape::Quota)
}

/// The wire's `fps` for a reading: control_http.rs's `fps_wire`, one decimal (pinned there by
/// `the_wire_fps_is_the_reading_to_one_decimal`).
fn fps_wire(fps: f64) -> f64 {
    (fps * 10.0).round() / 10.0
}

fn cpu_starved_shaped(done: bool, rung_back: bool, felt: Felt, alarm: AlarmIs, shape: Shape) -> World {
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
    let end = w.end;
    // droppedFrames is the session's overrun count (cumulative, the ladder's `overruns`); 40 is past
    // any step (ladder_overruns_to_step).
    let dropped = if felt == Felt::Step { 40 } else { 0 };
    w.beats_in(a + 1_000, end + 1, |x| x["droppedFrames"] = json!(dropped));
    if felt != Felt::No {
        w.beats_in(a, b, |x| x["encoderSpeed"] = json!(0.8));
    }
    if matches!(felt, Felt::Step | Felt::Uncounted) {
        w.beats_in(a + 6_000, end + 1, |x| x["rung"] = json!("overlay_half_rate"));
        if rung_back {
            w.beats_in(b + 5_000, end + 1, |x| x["rung"] = json!("full"));
        }
    }
    // The beat's rate: the frames out of the encoder since the beat before (adapters::stats's
    // BeatRate), so the share of that span under the quota reads STARVED_FPS. The first two beats
    // after a start read nothing (compositor/tests/wiring.rs).
    let hb = w.ms("heartbeat_interval_ms");
    let (floor, recovered) = (level(&w, "output_fps_floor"), level(&w, "output_fps_recovered"));
    let rate = 1_000.0 / level(&w, "frame_period_ms");
    let back = if alarm == AlarmIs::ClearsAtFloor {
        (floor + recovered) / 2.0
    } else {
        rate
    };
    let honest = matches!(felt, Felt::Honest | Felt::Step | Felt::Uncounted);
    let first = w.live + 2 * hb;
    let mut readings: Vec<(i64, f64)> = w
        .beats
        .iter()
        .filter(|(at, body)| *at >= first && body["state"] == "playing")
        .map(|(at, _)| {
            let at = *at;
            let starved = (at.min(b) - (at - hb).max(a)).max(0) as f64 / hb as f64;
            let base = if at > b { back } else { rate };
            let fps = if honest {
                STARVED_FPS * starved + base * (1.0 - starved)
            } else {
                rate
            };
            (at, fps)
        })
        .collect();
    // The shape's beat: the third judged one, the second reading, or the second beat whose window
    // lies after the quota.
    let at = match shape {
        Shape::Quota => None,
        Shape::OneFull => readings.iter().filter(|r| r.0 >= a + hb && r.0 <= b).nth(2),
        Shape::StartupDip => readings.get(1),
        Shape::SagsAfter | Shape::HealthyDip => readings.iter().filter(|r| r.0 >= b + hb).nth(1),
    }
    .map(|r| r.0);
    if let Some(at) = at {
        let fps = match shape {
            Shape::OneFull => rate,
            Shape::StartupDip => floor - 1.0,
            Shape::SagsAfter => floor - 0.1,
            Shape::HealthyDip => recovered - 0.1,
            Shape::Quota => unreachable!("the quota's shape moves no beat"),
        };
        readings.iter_mut().find(|r| r.0 == at).unwrap().1 = fps;
    }
    // The wire carries the same reading, to one decimal (C06's heartbeat, control_http.rs); a beat
    // with no reading sends no `fps`.
    for &(at, fps) in &readings {
        w.beats_in(at, at + 1, |x| x["fps"] = json!(fps_wire(fps)));
    }
    let (alarm_floor, alarm_recovered) = match alarm {
        AlarmIs::NeverRaised => (0.0, recovered),
        AlarmIs::NeverCleared => (floor, rate + 1.0),
        AlarmIs::FalseRaise => (rate + 0.5, recovered),
        AlarmIs::ClearsAtFloor => (floor, floor),
        AlarmIs::ClearsEarly => (floor, STARVED_FPS / 2.0),
        AlarmIs::Declared | AlarmIs::RaisedLate | AlarmIs::ClearsLate => (floor, recovered),
    };
    let mut lines = honest_lines(&readings, alarm_floor, alarm_recovered);
    match alarm {
        AlarmIs::RaisedLate => {
            lines.retain(|(_, m)| m.starts_with(BEAT_LINE));
            let at = end + w.ms("final_hold_ms") + 150;
            lines.push((at, format!("{RAISED_LINE}fps=0.40 floor={floor:.2}")));
        }
        AlarmIs::ClearsLate => {
            lines.retain(|(_, m)| !m.starts_with(CLEARED_LINE));
            let late = b + w.ms("budget_ladder_realtime_ms") + hb;
            let &(at, fps) = readings
                .iter()
                .find(|r| r.0 > late)
                .expect("a beat past realtime_after's window");
            lines.push((at, format!("{CLEARED_LINE}fps={fps:.2} recovered={recovered:.2}")));
        }
        _ => {}
    }
    for (at, m) in lines {
        w.line(at, m);
    }
    w
}

/// The beats case 6 judges: those whose window lies inside the quota's, [stress + one beat,
/// stress_done], counted from the world, not the verdict.
fn judged_beats(w: &World) -> usize {
    let at = |n: &str| w.marks.iter().find(|m| m.0 == n).map(|m| m.1).unwrap();
    let (a, b) = (at("stress") + w.ms("heartbeat_interval_ms"), at("stress_done"));
    w.log
        .iter()
        .filter(|(t, m)| m.starts_with(BEAT_LINE) && *t >= a && *t <= b)
        .count()
}

/// The sim's alarm is the product's: `domain::starvation::StarvationAlarm` over the world's logged
/// rates, at the profile the harness session runs (720p30, the fixture's), changes on the same beats.
fn assert_sim_alarm_is_the_products(w: &World) {
    use domain::starvation::{Alarm, StarvationAlarm};
    let rate = |m: &str| -> f64 {
        m.strip_prefix(BEAT_LINE)
            .and_then(|r| r.split(' ').next())
            .and_then(|r| r.parse().ok())
            .unwrap()
    };
    let mut product = StarvationAlarm::default();
    let mut want = Vec::new();
    for (at, m) in w.log.iter().filter(|l| l.1.starts_with(BEAT_LINE)) {
        match product.on_reading(rate(m), &domain::config::OutputProfile::P720) {
            Some(Alarm::Raised) => want.push((*at, "raised")),
            Some(Alarm::Cleared) => want.push((*at, "cleared")),
            None => {}
        }
    }
    let got: Vec<(i64, &str)> = w
        .log
        .iter()
        .filter_map(|(at, m)| {
            if m.starts_with(RAISED_LINE) {
                Some((*at, "raised"))
            } else if m.starts_with(CLEARED_LINE) {
                Some((*at, "cleared"))
            } else {
                None
            }
        })
        .collect();
    eprintln!("alarm changes compared with domain::starvation: {}", want.len());
    assert!(want.len() >= 2, "the good world raises and clears ({want:?})");
    assert_eq!(got, want, "the sim's alarm is not domain::starvation's");
}

/// Case 6 as CI's run 40143263604 read it under the honest beat: the beat and its alarm told the
/// truth and the ladder stayed still (by its rule), and the recording lost frames in the window:
/// its rate under the floors, score changes never shown, PTS gaps. Each outage covers one scheduled
/// change's whole time on screen (from its due time to the next one's, plus the page's lag).
fn cpu_clock_lost() -> World {
    let mut w = cpu_starved(true, false, Felt::Honest);
    let (a, b) = (w.at(20.0), w.at(60.0));
    let lag = OVERLAY_LAG_MS as i64;
    let due: Vec<i64> = w
        .timeline_events()
        .iter()
        .map(|&(_, at)| at + w.delay_ms)
        .collect();
    let spans: Vec<(i64, i64)> = due
        .windows(2)
        .filter(|d| d[0] >= a && d[1] + 2 * lag < b)
        .step_by(2)
        .map(|d| (d[0], d[1] + 2 * lag))
        .collect();
    assert!(spans.len() >= 3, "{spans:?}");
    w.outages.extend(spans);
    // The checker's PTS gaps, as CI's report carried five of them in the window.
    w.report_patch = Some(|r| r["pts_gaps"] = json!([{"at_ms": 0, "gap_ms": 1_834}]));
    w
}

fn failed_checks(v: &Verdict) -> Vec<Value> {
    v.json["failed"].as_array().unwrap().clone()
}

/// The judged wire beats: those at `[stress + one beat, stress_done]` that carry `fps`, counted from
/// the world.
fn judged_wire(w: &World) -> usize {
    let at = |n: &str| w.marks.iter().find(|m| m.0 == n).map(|m| m.1).unwrap();
    let (a, b) = (at("stress") + w.ms("heartbeat_interval_ms"), at("stress_done"));
    w.beats
        .iter()
        .filter(|(t, x)| *t >= a && *t <= b && x["fps"].is_number())
        .count()
}

// The wire's `fps` is the reading to one decimal: the producer's rounding, pinned in its source, and
// CI's run 40143263604, whose log and wire carried 20.27 and 20.3, 19.54 and 19.5 for the same beats.
// The verdict's tolerance between the two writers (half the wire's 0.1 plus half the log's 0.01)
// rests on this.
#[test]
fn the_wire_fps_is_the_reading_to_one_decimal() {
    let src = std::fs::read_to_string(repo().join("crates/adapters/src/control_http.rs")).unwrap();
    let mut pinned = 0;
    for needle in [
        ".map(|v| (v * 10.0).round() / 10.0)",
        "if let Some(v) = fps_wire(measured.fps) {",
        "put(\"fps\", v);",
    ] {
        assert_eq!(
            src.matches(needle).count(),
            1,
            "control_http.rs no longer has `{needle}` once"
        );
        pinned += 1;
    }
    let mut checked = 0;
    for (log, wire) in [(20.27, 20.3), (19.54, 19.5), (17.6, 17.6), (20.0, 20.0)] {
        assert_eq!(fps_wire(log), wire, "{log}");
        checked += 1;
    }
    eprintln!("producer lines pinned: {pinned}, CI pairs checked: {checked}");
    assert_eq!((pinned, checked), (3, 4));
}

// The good run, and the alarm's own reds: each regression of the alarm, a product mutant, reds its
// check by name, and the beats still read the truth.
#[test]
fn cpu_starved_passes_on_an_honest_beat_and_reds_on_each_alarm_regression() {
    let w = cpu_starved(true, false, Felt::Honest);
    assert_sim_alarm_is_the_products(&w);
    let v = judge(&w, "cpu-starved", "", "cpu-good");
    assert_pass(&v, "cpu-good");
    let m = &v.json["measures"];
    let (judged, wire) = (judged_beats(&w), judged_wire(&w));
    eprintln!("cpu-good: beats judged in the window: {judged} logged, {wire} on the wire");
    assert!(judged >= 1 && wire >= 1, "cpu-good judges no beat");
    assert_eq!(m["fps_beats_read"]["checked"], judged, "cpu-good: {m}");
    assert_eq!(m["fps_beats_read"]["reason"], Value::Null, "cpu-good: {m}");
    assert_eq!(m["beats_below_floor"]["above"], 0, "cpu-good: {m}");
    assert_eq!(m["wire_below_floor"]["checked"], wire, "cpu-good: {m}");
    assert_eq!(m["wire_below_floor"]["above"], 0, "cpu-good: {m}");
    assert_eq!(m["wire_agrees_with_log"]["checked"], judged, "cpu-good: {m}");
    assert_eq!(m["starvation_applied"]["applied"], true, "cpu-good: {m}");
    assert_eq!(m["ladder_justified"]["stepped"], 0, "cpu-good: {m}");
    // The recovery is judged past the catch-up: on the beats whose window lies after the quota.
    let after = m["alarm_cleared"]["after"].as_array().unwrap();
    assert!(!after.is_empty(), "cpu-good: {m}");
    // The clock is measured with its samples (the owner, 2026-10-25, "A"): the window's frames, the
    // recording's, and the score changes due.
    assert!(m["window_fps"]["frames"].as_u64() >= Some(1), "cpu-good: {m}");
    assert!(m["pts_gaps"]["frames"].as_u64() >= Some(2), "cpu-good: {m}");
    assert!(
        m["scheduled_changes_shown"]["due"].as_u64() >= Some(1),
        "cpu-good: {m}"
    );
    // A healthy beat a few frames short after the clear (0.1 under the recovered level, over the
    // floor: a 5 s window three frames short) is no re-raise, so no red: the product raises only
    // under the floor (the coordinator, medium, 2026-10-25, on #77's confirm pass).
    let (floor, recovered) = (level(&w, "output_fps_floor"), level(&w, "output_fps_recovered"));
    assert!(
        floor < recovered - 0.1,
        "the dip lies between the floor and the recovered level"
    );
    let dip = cpu_starved_shaped(true, false, Felt::Honest, AlarmIs::Declared, Shape::HealthyDip);
    let v = judge(&dip, "cpu-starved", "", "cpu-healthy-dip");
    assert_pass(&v, "cpu-healthy-dip");
    let c = &v.json["measures"]["alarm_cleared"];
    let after: Vec<f64> = c["after"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    eprintln!("cpu-healthy-dip: the recovery's beats {after:?} against {floor} and {recovered}");
    assert!(
        after.iter().any(|&f| f >= floor && f < recovered),
        "cpu-healthy-dip: the dip is not among the judged beats: {c}"
    );
    assert!(after.iter().any(|&f| f >= recovered), "cpu-healthy-dip: {c}");
    assert_eq!(
        v.json["measures"]["alarm_raised"]["raises"], 1,
        "cpu-healthy-dip: the product does not re-raise over the floor"
    );
    let mut checked = 0;
    for (tag, alarm, shape, check) in [
        (
            "cpu-never-raised",
            AlarmIs::NeverRaised,
            Shape::Quota,
            "alarm_raised",
        ),
        (
            "cpu-false-raise",
            AlarmIs::FalseRaise,
            Shape::Quota,
            "alarm_raised",
        ),
        (
            "cpu-startup-dip",
            AlarmIs::Declared,
            Shape::StartupDip,
            "alarm_raised",
        ),
        (
            "cpu-raised-late",
            AlarmIs::RaisedLate,
            Shape::Quota,
            "alarm_raised",
        ),
        (
            "cpu-never-cleared",
            AlarmIs::NeverCleared,
            Shape::Quota,
            "alarm_cleared",
        ),
        (
            "cpu-clears-early",
            AlarmIs::ClearsEarly,
            Shape::Quota,
            "alarm_cleared",
        ),
        (
            "cpu-clears-at-floor",
            AlarmIs::ClearsAtFloor,
            Shape::Quota,
            "alarm_cleared",
        ),
        (
            "cpu-clears-late",
            AlarmIs::ClearsLate,
            Shape::Quota,
            "alarm_cleared",
        ),
        (
            "cpu-sags-after",
            AlarmIs::Declared,
            Shape::SagsAfter,
            "alarm_cleared",
        ),
    ] {
        let w = cpu_starved_shaped(true, false, Felt::Honest, alarm, shape);
        let v = judge(&w, "cpu-starved", "", tag);
        assert_fails(&v, check, tag);
        // Each is the alarm's own red, or the recovery's: the judged beats still read the truth on
        // both writers.
        let failed = failed_checks(&v);
        for honest in ["beats_below_floor", "wire_below_floor", "wire_agrees_with_log"] {
            assert!(!failed.contains(&json!(honest)), "{tag}: {failed:?}");
        }
        checked += 1;
    }
    eprintln!("cpu-starved alarm regressions checked: {checked}");
    assert_eq!(checked, 9);
    // The sag is a re-raise: 0.1 under the floor, inside the judged beats.
    let sag = cpu_starved_shaped(true, false, Felt::Honest, AlarmIs::Declared, Shape::SagsAfter);
    let v = judge(&sag, "cpu-starved", "", "cpu-sags-after");
    let c = &v.json["measures"]["alarm_cleared"];
    assert!(
        c["after"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_f64)
            .any(|f| f < floor),
        "cpu-sags-after: the sag is not among the judged beats: {c}"
    );
    assert!(
        v.json["measures"]["alarm_raised"]["raises"].as_u64() >= Some(2),
        "cpu-sags-after: the product re-raises under the floor"
    );
    // The startup dip is the product's own alarm on a reading under the floor before the quota (its
    // raise is the run's first), and the run's last raise is the starvation's: the verdict reads
    // the first.
    let w = cpu_starved_shaped(true, false, Felt::Honest, AlarmIs::Declared, Shape::StartupDip);
    let v = judge(&w, "cpu-starved", "", "cpu-startup-dip");
    let r = &v.json["measures"]["alarm_raised"];
    assert!(r["raises"].as_u64() >= Some(2), "cpu-startup-dip: {r}");
    assert!(
        matches!(r["raise_from_stress_ms"].as_i64(), Some(x) if x < 0),
        "cpu-startup-dip: {r}"
    );
    // No beat whose window lies after the quota inside realtime_after's window (the log's beats
    // there lost): a recovery read on no beat is none.
    let mut w = cpu_starved(true, false, Felt::Honest);
    let (hb, b) = (w.ms("heartbeat_interval_ms"), w.at(60.0));
    w.log.retain(|(t, m)| !(m.starts_with(BEAT_LINE) && *t >= b + hb));
    let v = judge(&w, "cpu-starved", "", "cpu-no-after");
    assert_fails(&v, "alarm_cleared", "cpu-no-after");
    assert_eq!(
        v.json["measures"]["alarm_cleared"]["after"],
        json!([]),
        "cpu-no-after"
    );
}

// The beat's own reds, on either of its writers: a beat that never reads the starvation (F-024's),
// on both or on the wire alone; a wire that disagrees with the log or is missing; one judged beat at
// the full rate; a beat read nowhere (anti-vacuity, each reason named); and levels not the profile's.
#[test]
fn cpu_starved_reds_when_the_beat_lies_on_either_writer_or_is_missing() {
    // F-024's beat: the quota felt, every beat at the profile's rate on both writers, so no alarm.
    let v = judge(
        &cpu_starved(true, false, Felt::Lying),
        "cpu-starved",
        "",
        "cpu-lying",
    );
    for check in ["beats_below_floor", "wire_below_floor", "alarm_raised"] {
        assert_fails(&v, check, "cpu-lying");
    }
    assert_eq!(
        v.json["measures"]["starvation_applied"]["applied"], true,
        "cpu-lying: the drive was felt"
    );
    let mut checked = 1;
    // The wire's own regressions, the log honest: F-024 on the wire alone (the web and the organiser
    // see the profile's rate), a wire a frame per second under the log, one judged beat with no
    // `fps`, and no `fps` at all.
    let base = cpu_starved(true, false, Felt::Honest);
    let rate = 1_000.0 / level(&base, "frame_period_ms");
    let (a, b) = (base.at(20.0) + base.ms("heartbeat_interval_ms"), base.at(60.0));
    let (live, end) = (base.live, base.end);
    let first = base
        .beats
        .iter()
        .find(|(t, x)| *t >= a && *t <= b && x["fps"].is_number())
        .map(|x| x.0)
        .unwrap();
    for (tag, fails, passes) in [
        (
            "cpu-wire-lies",
            &["wire_below_floor", "wire_agrees_with_log"][..],
            &["beats_below_floor", "alarm_raised"][..],
        ),
        (
            "cpu-wire-offset",
            &["wire_agrees_with_log"][..],
            &["wire_below_floor", "beats_below_floor"][..],
        ),
        (
            "cpu-wire-missing-one",
            &["wire_agrees_with_log"][..],
            &["wire_below_floor", "beats_below_floor"][..],
        ),
        (
            "cpu-no-wire",
            &["wire_below_floor", "wire_agrees_with_log"][..],
            &["beats_below_floor", "fps_beats_read"][..],
        ),
    ] {
        let mut w = cpu_starved(true, false, Felt::Honest);
        match tag {
            "cpu-wire-lies" => w.beats_in(live, end + 1, |x| {
                if x["fps"].is_number() {
                    x["fps"] = json!(rate);
                }
            }),
            "cpu-wire-offset" => w.beats_in(live, end + 1, |x| {
                if let Some(f) = x["fps"].as_f64() {
                    x["fps"] = json!(fps_wire(f - 1.0));
                }
            }),
            "cpu-wire-missing-one" => w.beats_in(first, first + 1, |x| {
                x.as_object_mut().unwrap().remove("fps");
            }),
            _ => w.beats_in(live, end + 1, |x| {
                x.as_object_mut().unwrap().remove("fps");
            }),
        }
        let v = judge(&w, "cpu-starved", "", tag);
        for check in fails {
            assert_fails(&v, check, tag);
        }
        let failed = failed_checks(&v);
        for check in passes {
            assert!(!failed.contains(&json!(check)), "{tag}: {failed:?}");
        }
        checked += 1;
    }
    // One judged beat at the full rate among starved ones, on both writers: "every judged beat"
    // means every one.
    let w = cpu_starved_shaped(true, false, Felt::Honest, AlarmIs::Declared, Shape::OneFull);
    let v = judge(&w, "cpu-starved", "", "cpu-one-full");
    for check in ["beats_below_floor", "wire_below_floor"] {
        assert_fails(&v, check, "cpu-one-full");
    }
    let m = &v.json["measures"];
    assert_eq!(m["beats_below_floor"]["above"], 1, "cpu-one-full: {m}");
    assert_eq!(m["wire_below_floor"]["above"], 1, "cpu-one-full: {m}");
    assert!(
        m["beats_below_floor"]["checked"].as_u64() >= Some(2),
        "cpu-one-full: {m}"
    );
    checked += 1;
    // No beat read: none logged, none in the window, the window's marks lost, or a report from a
    // checker older than the honest beat. Each is a FAIL with its reason, and 0 checked.
    for (tag, reason) in [
        ("cpu-no-beats", "no `output fps beat=` line in the log"),
        ("cpu-no-window-beats", "no beat inside the starvation window"),
        (
            "cpu-old-report",
            "the report has no output_fps (a checker older than the honest beat)",
        ),
    ] {
        let mut w = cpu_starved(true, false, Felt::Honest);
        match tag {
            "cpu-no-beats" => w.log.retain(|(_, m)| !m.starts_with(BEAT_LINE)),
            "cpu-no-window-beats" => w
                .log
                .retain(|(t, m)| !(m.starts_with(BEAT_LINE) && *t >= a && *t <= b)),
            _ => {
                w.report_patch = Some(|r| {
                    r["log"].as_object_mut().unwrap().remove("output_fps");
                });
            }
        }
        let v = judge(&w, "cpu-starved", "", tag);
        for check in ["fps_beats_read", "beats_below_floor", "wire_agrees_with_log"] {
            assert_fails(&v, check, tag);
        }
        if tag != "cpu-no-window-beats" {
            assert_fails(&v, "beat_levels_known", tag);
        }
        let shown = &v.json["measures"]["fps_beats_read"];
        assert_eq!(shown["reason"], reason, "{tag}: {shown}");
        assert_eq!(shown["checked"], 0, "{tag}: {shown}");
        checked += 1;
    }
    // The log's levels are the profile's (P720, the fixture's session): a floor or a recovered
    // level from another profile (25 fps) is a red by name, never judged against 720p30's.
    let floor = level(&base, "output_fps_floor");
    let recovered = level(&base, "output_fps_recovered");
    for (tag, from, to) in [
        (
            "cpu-other-floor",
            format!(" floor={floor:.2}"),
            format!(" floor={:.2}", floor * 25.0 / rate),
        ),
        (
            "cpu-other-recovered",
            format!(" recovered={recovered:.2}"),
            format!(" recovered={:.2}", recovered * 25.0 / rate),
        ),
    ] {
        let mut w = cpu_starved(true, false, Felt::Honest);
        let mut n = 0;
        for (_, m) in &mut w.log {
            if m.contains(&from) {
                *m = m.replace(&from, &to);
                n += 1;
            }
        }
        assert!(n >= 1, "{tag}: no line carried `{from}`");
        let v = judge(&w, "cpu-starved", "", tag);
        assert_fails(&v, "beat_levels_known", tag);
        checked += 1;
    }
    eprintln!("cpu-starved beat reds checked: {checked}");
    assert_eq!(checked, 11);
}

// The ladder's logic is unchanged (the owner's ruling, no ladder change; the coordinator, medium,
// 2026-10-25, on the PR #77 review's I3): a step its own trigger justifies passes, a step without
// the overruns its rule needs is a red, and C05's rungs and bottom still hold.
#[test]
fn cpu_starved_judges_the_ladder_by_its_own_rule() {
    // A step with its overruns counted: the unchanged C09 rule at work. The whole verdict passes.
    let v = judge(
        &cpu_starved(true, false, Felt::Step),
        "cpu-starved",
        "",
        "cpu-step",
    );
    assert_pass(&v, "cpu-step");
    let l = &v.json["measures"]["ladder_justified"];
    let needs = cpu_starved(true, false, Felt::Step).ms("ladder_overruns_to_step");
    assert!(l["stepped"].as_u64() > Some(0), "cpu-step: {l}");
    assert!(l["step_rise"].as_i64() >= Some(needs), "cpu-step: {l}");
    let mut checked = 1;
    // A step with no overrun counted, and one with one fewer than the rule needs.
    let mut unjustified = cpu_starved(true, false, Felt::Step);
    let (a, end) = (unjustified.at(20.0), unjustified.end);
    unjustified.beats_in(a + 1_000, end + 1, move |x| x["droppedFrames"] = json!(needs - 1));
    for (tag, w) in [
        ("cpu-uncounted", cpu_starved(true, false, Felt::Uncounted)),
        ("cpu-unjustified", unjustified),
    ] {
        let v = judge(&w, "cpu-starved", "", tag);
        assert_fails(&v, "ladder_justified", tag);
        let l = &v.json["measures"]["ladder_justified"];
        assert!(
            matches!(l["step_rise"].as_i64(), Some(r) if r < needs),
            "{tag}: {l}"
        );
        checked += 1;
    }
    // A step, then back to full: C05's bottom does not hold, though the step was justified.
    let v = judge(
        &cpu_starved(true, true, Felt::Step),
        "cpu-starved",
        "",
        "cpu-climbs",
    );
    assert_fails(&v, "rung_holds", "cpu-climbs");
    assert!(
        !failed_checks(&v).contains(&json!("ladder_justified")),
        "cpu-climbs: {:?}",
        failed_checks(&v)
    );
    checked += 1;
    // A rung C05 retired (FastPreset), its step justified: not a rung the ladder knows.
    let mut w = cpu_starved(true, false, Felt::Step);
    let (live, end) = (w.live, w.end);
    w.beats_in(live, end + 1, |x| {
        if x["rung"] == "overlay_half_rate" {
            x["rung"] = json!("fast_preset");
        }
    });
    let v = judge(&w, "cpu-starved", "", "cpu-unknown-rung");
    assert_fails(&v, "rungs_known", "cpu-unknown-rung");
    assert!(
        !failed_checks(&v).contains(&json!("ladder_justified")),
        "cpu-unknown-rung: {:?}",
        failed_checks(&v)
    );
    checked += 1;
    // No beat carried a rung: the ladder read nowhere is no proof it followed its rule.
    let mut w = cpu_starved(true, false, Felt::Honest);
    w.beats_in(live, end + 1, |x| {
        x.as_object_mut().unwrap().remove("rung");
    });
    let v = judge(&w, "cpu-starved", "", "cpu-no-rung");
    assert_fails(&v, "ladder_justified", "cpu-no-rung");
    assert_fails(&v, "rungs_known", "cpu-no-rung");
    checked += 1;
    eprintln!("cpu-starved ladder cases checked: {checked}");
    assert_eq!(checked, 6);
}

// The drive's own reds (the quota not held, not restored, never set), the drive's witness, and the
// clock: CI's world under the honest beat, its clock measured (the owner, 2026-10-25, "A").
#[test]
fn cpu_starved_reds_on_its_drive_and_measures_a_lost_clock() {
    let w = cpu_starved(false, false, Felt::Honest);
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
        let mut w = cpu_starved(true, false, Felt::Honest);
        edit(&mut w);
        assert_fails(&judge(&w, "cpu-starved", "", tag), "stress_ran", tag);
        checked += 1;
    }
    assert_eq!(checked, 3);
    // The drive could not read the demand: no quota, the refusal shown.
    let mut w = cpu_starved(false, false, Felt::Honest);
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
    // starved, whatever the beats before its end read, and no beat is judged in it.
    let mut w = cpu_starved(true, false, Felt::Honest);
    w.marks.retain(|m| m.0 != "stress");
    let v = judge(&w, "cpu-starved", "", "cpu-nostart");
    assert_fails(&v, "starvation_applied", "cpu-nostart");
    assert_fails(&v, "fps_beats_read", "cpu-nostart");
    assert_eq!(
        v.json["measures"]["starvation_applied"]["applied"], false,
        "cpu-nostart"
    );
    // The reason names the lost mark, not an empty window (the PR #77 review's m6).
    assert_eq!(
        v.json["measures"]["fps_beats_read"]["reason"],
        "no quota window: a `stress` or `stress_done` mark is missing",
        "cpu-nostart"
    );
    // CI's first build: the stress never reached the compositor (the PR #52 review's I3). The
    // re-review's I-4: that is the drive's own red (`starvation_applied`), and no Owed row explains
    // it.
    let v = judge(
        &cpu_starved(true, false, Felt::No),
        "cpu-starved",
        "",
        "cpu-unfelt",
    );
    assert_fails(&v, "starvation_applied", "cpu-unfelt");
    // The window's frame rate alone says starved: 4 s of the 40 lost (27 fps, under the floor),
    // encoderSpeed 1.0 throughout. The recording's clock did not hold either: measured, under the bar.
    let mut w = cpu_starved(true, false, Felt::No);
    let a = w.at(30.0);
    w.outages.push((a, a + 4_000));
    let v = judge(&w, "cpu-starved", "", "cpu-fps-only");
    assert!(
        !v.json["failed"]
            .as_array()
            .unwrap()
            .contains(&json!("starvation_applied")),
        "cpu-fps-only: {:?}",
        v.json["measures"]["starvation_applied"]
    );
    let fps = v.json["measures"]["window_fps"]["fps"].as_f64();
    let bar = level(&w, "budget_fps_mean_min");
    assert!(
        fps.is_some_and(|f| f < bar),
        "cpu-fps-only: {fps:?} against {bar}"
    );
    // 1 s of the 40 lost (29.25 fps): over the beat's floor, under the recording's mean bar. The
    // witness reads the beat's floor (OutputProfile::P720.fps_floor(), the PR #69 review's m1), so
    // the product's own reading says this window was not starved: the drive's red, no Owed row's.
    let mut w = cpu_starved(true, false, Felt::No);
    let a = w.at(30.0);
    w.outages.push((a, a + 1_000));
    let v = judge(&w, "cpu-starved", "", "cpu-fps-near");
    let s = &v.json["measures"]["starvation_applied"];
    let (floor, bar) = (level(&w, "output_fps_floor"), level(&w, "budget_fps_mean_min"));
    let fps = s["window_fps"].as_f64().expect("cpu-fps-near: a window rate");
    assert!(
        floor < fps && fps < bar,
        "cpu-fps-near: {fps} between the floor {floor} and the bar {bar}"
    );
    assert_eq!(s["fps_floor"].as_f64(), Some(floor), "cpu-fps-near: {s}");
    assert_fails(&v, "starvation_applied", "cpu-fps-near");
    // CI's world under the honest beat (run 40143263604): the beat, the wire, the alarm and the
    // ladder told the truth, and the recording lost frames in the window (its rate under the bar,
    // score changes never shown, PTS gaps). Since the owner's ruling (2026-10-25, "A") case 6
    // measures the clock: the case passes, and the three values are shown, in its log and in
    // run-chaos.sh's table (the case's key measure).
    let lost = cpu_clock_lost();
    let v = judge(&lost, "cpu-starved", "", "cpu-clock-lost");
    assert_pass(&v, "cpu-clock-lost");
    let m = &v.json["measures"];
    let bar = level(&lost, "budget_fps_mean_min");
    let fps = m["window_fps"]["fps"].as_f64();
    assert!(fps.is_some_and(|f| f < bar), "cpu-clock-lost: {m}");
    assert!(m["pts_gaps"]["gaps"].as_u64() >= Some(1), "cpu-clock-lost: {m}");
    let unshown = m["scheduled_changes_shown"]["missing"].as_array().unwrap().len();
    let due = m["scheduled_changes_shown"]["due"].as_u64().unwrap();
    eprintln!("cpu-clock-lost: {fps:?} fps against {bar}, {unshown} of {due} changes unshown");
    assert!(unshown >= 3, "cpu-clock-lost: {m}");
    assert_eq!(v.json["key"], "clock_measured", "cpu-clock-lost");
    let key = &m["clock_measured"];
    assert_eq!(key["unshown"], unshown, "cpu-clock-lost: {key}");
    assert_eq!(key["due"], due, "cpu-clock-lost: {key}");
    assert_eq!(key["pts_gaps"], m["pts_gaps"]["gaps"], "cpu-clock-lost: {key}");
    assert!(
        key["fps"].as_f64().is_some_and(|f| f < bar),
        "cpu-clock-lost: {key}"
    );
    // The same recording judged by a case that keeps its clock check: input-impaired's pts_gaps, the
    // predicate case 6 judged before the ruling, still reds on it. No other case judges window_fps,
    // and cef-gpu-deaths and cef-leaks judge their own scheduled windows (after the last kill, the
    // leak), so pts_gaps is the shared witness.
    let v = judge(&lost, "input-impaired", "", "impaired-clock-lost");
    assert_fails(&v, "pts_gaps", "impaired-clock-lost");
    // A clock read on nothing is a red: no window (the quota never put back), no recording, no score
    // change scheduled after the quota, and a recording of one frame (a gap is between two; the
    // frame overlaid and late, so changes are due and only the frame count reds).
    let mut checked = 0;
    for (tag, edit) in [
        (
            "cpu-clock-nowindow",
            (|w: &mut World| w.marks.retain(|m| m.0 != "stress_done")) as fn(&mut World),
        ),
        ("cpu-clock-norecording", |w: &mut World| {
            w.report_patch = Some(|r| r["frames_table"] = json!([]));
        }),
        ("cpu-clock-nochanges", |w: &mut World| w.score_offsets.clear()),
        ("cpu-clock-oneframe", |w: &mut World| {
            w.report_patch = Some(|r| {
                let last = r["frames_table"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|f| !f[2].is_null())
                    .cloned()
                    .unwrap();
                r["frames_table"] = json!([last]);
            });
        }),
    ] {
        let mut w = cpu_starved(true, false, Felt::Honest);
        edit(&mut w);
        let v = judge(&w, "cpu-starved", "", tag);
        assert_fails(&v, "clock_measured", tag);
        if tag == "cpu-clock-oneframe" {
            let m = &v.json["measures"];
            assert_eq!(m["pts_gaps"]["frames"], 1, "{tag}: {m}");
            assert!(
                m["scheduled_changes_shown"]["due"].as_u64() >= Some(1),
                "{tag}: {m}"
            );
        }
        checked += 1;
    }
    eprintln!("cpu-starved unsampled clocks checked: {checked}");
    assert_eq!(checked, 4);
}

// A page that goes off air after the quota (Owed 96, PR #79's finding): the changes due are bounded
// by the last PROGRAMME frame, not by the last frame whose overlay read, so the changes the page
// never showed are measured missing, not dropped from the due set. Each expected index comes from
// the world's own schedule (the timeline's `at` plus its delay), never from the verdict.
#[test]
fn cpu_starved_counts_the_changes_due_after_the_page_goes_off_air() {
    let on_air = judge(
        &cpu_starved(true, false, Felt::Honest),
        "cpu-starved",
        "",
        "cpu-on-air",
    );
    assert_pass(&on_air, "cpu-on-air");
    let shown = &on_air.json["measures"]["scheduled_changes_shown"];
    assert_eq!(shown["missing"], json!([]), "cpu-on-air: {shown}");
    let mut w = cpu_starved(true, false, Felt::Honest);
    let done = w
        .marks
        .iter()
        .find(|m| m.0 == "stress_done")
        .map(|m| m.1)
        .unwrap();
    let delay = w.delay_ms;
    let due: Vec<(u16, i64)> = w
        .timeline_events()
        .iter()
        .map(|&(i, at)| (i, at + delay))
        .collect();
    // The first change due a second past the quota with half a second clear before it; the page
    // goes off air 50 ms before it is due, so every change before it was shown and none from it on.
    let k = (0..due.len())
        .find(|&j| due[j].1 > done + 1_000 && (j == 0 || due[j].1 - due[j - 1].1 > 500))
        .expect("a change due after the quota with a gap before it");
    let off = due[k].1 - 50;
    // The guard on the programme's last frame: no change is due in its last second, so "due by the
    // last frame less the frame-to-encoder lag" cannot turn on a frame's phase.
    let tail = w.end - 1_000;
    assert!(
        due.iter().all(|&(_, d)| d <= tail || d > w.end),
        "a change is due in the programme's last second: {due:?}"
    );
    w.overlay_none.push((off, w.end + 1_000));
    let v = judge(&w, "cpu-starved", "", "cpu-off-air-later");
    let expected: Vec<u16> = due[k..]
        .iter()
        .filter(|&&(_, d)| d <= tail)
        .map(|&(i, _)| i)
        .collect();
    let m = &v.json["measures"]["scheduled_changes_shown"];
    eprintln!(
        "cpu-off-air-later: off air {} ms after the quota, {} changes unshown of {} due",
        off - done,
        expected.len(),
        m["due"]
    );
    assert!(!expected.is_empty(), "no change due after the page went off air");
    assert!(m["due"].as_u64() >= Some(expected.len() as u64), "{m}");
    assert_eq!(m["missing"], json!(expected), "cpu-off-air-later: {m}");
    assert_eq!(
        v.json["measures"]["clock_measured"]["unshown"],
        expected.len(),
        "cpu-off-air-later: the key measure"
    );
    // Off air is still case 6's red, by its own check; the clock is measured, not judged.
    assert_fails(&v, "score_on_air", "cpu-off-air-later");
    assert!(
        !failed_checks(&v).contains(&json!("clock_measured")),
        "cpu-off-air-later: {:?}",
        failed_checks(&v)
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
    // PR #79's fix round 1 (the one-frame hole): the first change after the recovery shows, and the
    // overlay goes off air 300 ms after the page's first good poll could show it, to the end. A
    // recovery is held for resume_after_ms: red.
    let mut w = score_down(true, false);
    let until = w.score_blind.unwrap().1;
    let poll = w.ms("harness_page_poll_ms");
    let hold = w.ms("resume_after_ms");
    w.overlay_none.push((until + poll + 300, w.end + 60_000));
    let v = judge(&w, "score-down", "", "score-back-leaves");
    assert_fails(&v, "changes_after_recovery", "score-back-leaves");
    let shown = &v.json["measures"]["changes_after_recovery"];
    assert!(
        shown["stayed_ms"].as_i64().is_some_and(|s| s > 0 && s < hold),
        "score-back-leaves: on air less than the hold: {shown}"
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
    // PR #79's fix round 1 (the one-frame hole): the overlay is back with the first programme frame
    // after the restart and off air again 400 ms later, to the end. Back means held for
    // resume_after_ms: red, and the measure says how long it stayed.
    let mut w = cef_killed(good(), 133);
    let start = *w.starts().last().unwrap();
    let hold = w.ms("resume_after_ms");
    w.overlay_none
        .push((start + FIRST_PACKET_MS + 400, w.end + 60_000));
    let v = judge(&w, "cef-killed", "", "cef-back-leaves");
    assert_fails(&v, "overlay_back", "cef-back-leaves");
    let back = &v.json["measures"]["overlay_back"];
    assert!(
        back["stayed_ms"].as_i64().is_some_and(|s| s > 0 && s < hold),
        "cef-back-leaves: on air less than the hold: {back}"
    );
    // CI's restart legs (PR run 40154210504, all 16): the restart's catch-up burst lands its
    // strip-less frames and its first strip frames on ONE wall ms, the strip-less ones first. Back
    // is judged in frame order from the first strip frame, so the burst's outage frames before it
    // are not "off air after back" (a wall-ms window redded every real restart leg).
    let mut w = cef_killed(good(), 133);
    let start = *w.starts().last().unwrap();
    w.overlay_none.push((k + 1_200, start + FIRST_PACKET_MS + 900));
    w.report_patch = Some(|r| {
        let f = r["frames_table"].as_array_mut().unwrap();
        let back = (1..f.len())
            .rev()
            .find(|&i| !f[i][2].is_null() && f[i - 1][2].is_null())
            .expect("a strip frame after strip-less ones");
        let wall = f[back][0].clone();
        let mut i = back;
        while i > 0 && f[i - 1][2].is_null() {
            i -= 1;
            f[i][0] = wall.clone();
        }
        assert!(
            back - i >= 2,
            "a burst of strip-less frames on the back frame's wall ms"
        );
    });
    let v = judge(&w, "cef-killed", "", "cef-back-burst");
    assert_pass(&v, "cef-back-burst");
    let report = judged_report("cef-back-burst", "cef-killed");
    let back_ms = report["frames_table"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f[0].as_i64().is_some_and(|w| w > start) && !f[2].is_null())
        .map(|f| f[0].as_i64().unwrap())
        .next()
        .unwrap();
    let burst = report["frames_table"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f[0].as_i64() == Some(back_ms) && f[2].is_null())
        .count();
    eprintln!("cef-back-burst: {burst} strip-less frames on the back frame's wall ms");
    assert!(burst >= 2, "cef-back-burst: {burst}");
    // The PR #79 confirm pass's C-1: the same burst, then ONE strip-less frame AFTER the back frame
    // on the back frame's own wall ms. That frame is off air after back: red (a window that skipped
    // the back frame's timestamp passed it).
    let mut w = cef_killed(good(), 133);
    let start = *w.starts().last().unwrap();
    w.overlay_none.push((k + 1_200, start + FIRST_PACKET_MS + 900));
    w.report_patch = Some(|r| {
        let f = r["frames_table"].as_array_mut().unwrap();
        let back = (1..f.len())
            .rev()
            .find(|&i| !f[i][2].is_null() && f[i - 1][2].is_null())
            .expect("a strip frame after strip-less ones");
        let wall = f[back][0].clone();
        let mut i = back;
        while i > 0 && f[i - 1][2].is_null() {
            i -= 1;
            f[i][0] = wall.clone();
        }
        assert!(!f[back + 1][2].is_null(), "the frame after back shows the strip");
        f[back + 1][0] = wall;
        f[back + 1][2] = Value::Null;
    });
    let v = judge(&w, "cef-killed", "", "cef-back-burst-leaves");
    assert_fails(&v, "overlay_back", "cef-back-burst-leaves");
    let back = &v.json["measures"]["overlay_back"];
    assert_eq!(
        back["stayed_ms"],
        json!(0),
        "cef-back-burst-leaves: off air on the back frame's own wall ms: {back}"
    );
    // C-2: back, then an output gap inside the hold (no programme frames for 400 ms) and on air
    // again. Every frame in the hold shows the strip, but too few frames are in it to call the
    // output held: red. The floor is the output's own fps floor over the hold, never a literal.
    let mut w = cef_killed(good(), 133);
    let start = *w.starts().last().unwrap();
    let back_at = start + FIRST_PACKET_MS;
    w.outages.push((back_at + 300, back_at + 700));
    let v = judge(&w, "cef-killed", "", "cef-back-gap");
    assert_fails(&v, "overlay_back", "cef-back-gap");
    let back = &v.json["measures"]["overlay_back"];
    let floor = w.t["output_fps_floor"].as_f64().unwrap();
    let hold = w.t["resume_after_ms"].as_f64().unwrap();
    let needed = (floor * hold / 1000.0).ceil() as u64;
    let frames = back["hold_frames"].as_u64().unwrap_or(u64::MAX);
    eprintln!("cef-back-gap: {frames} frames in the hold, {needed} needed: {back}");
    assert!(
        frames < needed && back["hold_frames_needed"] == json!(needed) && back["stayed_ms"].is_null(),
        "cef-back-gap: on air throughout, too few frames: {back}"
    );
    // And the healthy restart's hold, for the floor's other side: at least the floor's frames.
    let v = judge(&cef_killed(good(), 133), "cef-killed", "", "cef-back-held");
    let back = &v.json["measures"]["overlay_back"];
    assert!(
        back["hold_frames"].as_u64().is_some_and(|n| n >= needed),
        "cef-back-held: {back}"
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
    // PR #79's fix round 1 (the one-frame hole): back after the new generation's ready, and off
    // air again 400 ms later, to the end. Back means held for resume_after_ms: red.
    let mut w = cef_hung(true);
    let back = w.overlay_none[0].1;
    let hold = w.ms("resume_after_ms");
    w.overlay_none.push((back + 400, w.end + 60_000));
    let v = judge(&w, "cef-hung", "", "hung-back-leaves");
    assert_fails(&v, "overlay_back", "hung-back-leaves");
    let shown = &v.json["measures"]["overlay_back"];
    assert!(
        shown["stayed_ms"].as_i64().is_some_and(|s| s > 0 && s < hold),
        "hung-back-leaves: on air less than the hold: {shown}"
    );
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

/// A reloaded page's ready, after its reload. The C15 cost legs measured a crashed page ready 4.2 to
/// 5.2 s after the renderer KILL, which includes the crash path's back-off (`cef_backoff_base_ms`,
/// 1 s) and its detection; a reload has no back-off, so its own build to ready is about 3.2 to
/// 4.2 s (the PR #87 review's m-1). 4.5 s keeps the sims on the slow side of that.
const RELOAD_READY_MS: i64 = 4_500;

/// GPU-process kills from 20 s, `spacing_ms` apart, `kills` of them; the child poll counts each
/// `count_lag_ms` after it. With `restarts`, the compositor restarts in place after the poll that
/// counted `cef_gpu_deaths_to_restart`, `exit_after_ms` after that poll. Each count below that
/// reloads the page (Owed 96; owner ruling 2026-10-25 "A"): `gpu_deaths_reloading`.
fn gpu_deaths(kills: u32, spacing_ms: i64, count_lag_ms: i64, restarts: bool, exit_after_ms: i64) -> World {
    gpu_deaths_reloading(
        kills,
        spacing_ms,
        count_lag_ms,
        restarts,
        exit_after_ms,
        RELOAD_READY_MS,
    )
}

/// `gpu_deaths`, the reloads' ready `ready_ms` after each. The driver logs `cef reload gen=<g>
/// reason=gpu_process_death deaths=<n>` in the tick that read a count below the restart's, right
/// after its `cef health` line; generation `g` signals ready `ready_ms` later unless another reload
/// (or the die) comes first, and from the first reload of a run of them to that ready the overlay
/// holds the frame it had (the programme keeps the last overlay frame while a reload loads).
fn gpu_deaths_reloading(
    kills: u32,
    spacing_ms: i64,
    count_lag_ms: i64,
    restarts: bool,
    exit_after_ms: i64,
    ready_ms: i64,
) -> World {
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
    // The reloads: each count below the restart's, read before the die.
    let reloads: Vec<i64> = counted
        .iter()
        .take(w.gpu_deaths.len())
        .take(n - 1)
        .copied()
        .filter(|&c| die.is_none_or(|d| c < d))
        .collect();
    let mut held_from: Option<i64> = None;
    for (j, &at) in reloads.iter().enumerate() {
        let gen = 2 + j as u64;
        w.line(
            at,
            format!("cef reload gen={gen} reason=gpu_process_death deaths={}", j + 1),
        );
        let from = *held_from.get_or_insert(at);
        let ready = at + ready_ms;
        let superseded = reloads.get(j + 1).is_some_and(|&next| next < ready);
        if superseded {
            continue;
        }
        match die {
            Some(d) if ready >= d => w.overlay_hold.push((from, d)),
            _ => {
                w.line(ready, format!("cef ready gen={gen}"));
                w.overlay_hold.push((from, ready));
            }
        }
        held_from = None;
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
    // Owed 96 (owner ruling 2026-10-25 "A"): each count below the restart's reloads once, and the
    // count at it reloads nothing. The healthy run's reloads are the counts 1 to n - 1, from the
    // declared threshold.
    let w = gpu_deaths(n, 2_000, 800, true, 600);
    let v = judge(&w, "cef-gpu-deaths", "threshold", "gpu-reloads");
    let want: Vec<u32> = (1..n).collect();
    assert_eq!(
        v.json["measures"]["gpu_death_reloads"]["got"],
        json!(want),
        "gpu-reloads: {}",
        v.json["measures"]["gpu_death_reloads"]
    );
    // The harness's mutant "reload on the threshold death too": a reload line at the deciding count.
    let mut w = gpu_deaths(n, 2_000, 800, true, 600);
    let deciding = w.gpu_deaths[n as usize - 1];
    w.line(
        deciding,
        format!("cef reload gen={} reason=gpu_process_death deaths={n}", n + 1),
    );
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "threshold", "gpu-reload-at-restart"),
        "gpu_death_reloads",
        "gpu-reload-at-restart",
    );
    // The build before the fix: no reload at all.
    let mut w = gpu_deaths(n, 2_000, 800, true, 600);
    without_reloads(&mut w);
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "threshold", "gpu-noreload"),
        "gpu_death_reloads",
        "gpu-noreload",
    );
    // A reload named for another cause, one a poll late after its count, and one 1 ms BEFORE its
    // count's health line (the PR #87 review's m-3: the driver writes the health line first): each
    // reds.
    let mut checked = 0;
    for tag in ["gpu-reload-cause", "gpu-reload-late", "gpu-reload-early"] {
        let mut w = gpu_deaths(n, 2_000, 800, true, 600);
        let poll = w.ms("cef_child_poll_ms");
        let first = w
            .log
            .iter_mut()
            .find(|(_, m)| m.starts_with("cef reload gen=2 "))
            .expect("the first reload");
        match tag {
            "gpu-reload-cause" => first.1 = first.1.replace("reason=gpu_process_death", "reason=recycle"),
            "gpu-reload-late" => first.0 += poll + 1,
            _ => first.0 -= 1,
        }
        assert_fails(
            &judge(&w, "cef-gpu-deaths", "threshold", tag),
            "gpu_death_reloads",
            tag,
        );
        checked += 1;
    }
    assert_eq!(checked, 3);
}

/// The report.json `judge` wrote under `tag` for the leg `name` (`<case>-<variant>`).
fn judged_report(tag: &str, name: &str) -> Value {
    let p = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("case-verdicts")
        .join(tag)
        .join(name)
        .join("report.json");
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("{tag}: no report.json")))
        .unwrap()
}

/// The score changes `w`'s timeline makes due after `from` and no later than the run's last
/// PROGRAMME frame or its end request, whichever is first, less the frame-to-encoder budget, as
/// (index, due): what a check of the changes shown must find (Owed 96: never bounded by the last
/// frame whose overlay read; fix round 1's m-1: nor by the final hold's end slate). From the world's
/// own timeline and the frames its run recorded, never from a verdict.
fn due_after(w: &World, report: &Value, from: i64) -> Vec<(u16, i64)> {
    let last = report["frames_table"]
        .as_array()
        .and_then(|f| f.last())
        .and_then(|f| f[0].as_i64())
        .expect("the run recorded a programme frame")
        .min(w.end);
    let budget = w.ms("budget_frame_to_encoder_ms");
    w.timeline_events()
        .into_iter()
        .map(|(i, at)| (i, at + w.delay_ms))
        .filter(|&(_, due)| due > from && due <= last - budget)
        .collect()
}

/// Whether log line `m` is a reloaded generation's ready (a generation after the first).
fn is_reload_ready(m: &str) -> bool {
    m.strip_prefix("cef ready gen=")
        .and_then(|g| g.parse::<u64>().ok())
        .is_some_and(|g| g >= 2)
}

/// The ready of the last reload `w` logged (a generation after the first), from the world's own log.
fn last_reload_ready(w: &World) -> Option<i64> {
    w.log
        .iter()
        .filter(|(_, m)| is_reload_ready(m))
        .map(|l| l.0)
        .max()
}

/// The first programme frame at or after `at` whose overlay read, from the run's recorded frames.
fn first_strip_frame(report: &Value, at: i64) -> Option<i64> {
    report["frames_table"]
        .as_array()?
        .iter()
        .find(|f| f[0].as_i64().is_some_and(|x| x >= at) && !f[2].is_null())
        .and_then(|f| f[0].as_i64())
}

/// Where the due set starts (Owed 96): the overlay's first strip frame after the last reload's
/// ready, or the reload budget after the last kill when that comes first or nothing came back. From
/// the world's log and the frames its run recorded, never from a verdict.
fn reload_window_end(w: &World, report: &Value, last_kill: i64) -> i64 {
    let cap = last_kill + w.ms("budget_gpu_death_reload_ms");
    last_reload_ready(w)
        .and_then(|r| first_strip_frame(report, r))
        .map_or(cap, |b| b.min(cap))
}

/// The checks a verdict failed, sorted.
fn failed_sorted(v: &Verdict) -> Vec<String> {
    let mut f: Vec<String> = v.json["failed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect();
    f.sort();
    f
}

/// `checks`, sorted, as owned strings.
fn sorted(checks: &[&str]) -> Vec<String> {
    let mut c: Vec<String> = checks.iter().map(|&x| x.to_owned()).collect();
    c.sort();
    c
}

/// `w` with the product's reloads removed (the build before Owed 96's fix): no reload line, no
/// reloaded generation's ready, and nothing held for one; the page paints on through every kill.
fn without_reloads(w: &mut World) {
    w.log
        .retain(|(_, m)| !(m.starts_with("cef reload ") || is_reload_ready(m)));
    w.overlay_hold.clear();
}

const ONE_FEWER: &str = "cef-gpu-deaths-one-fewer";

#[test]
fn cef_gpu_deaths_one_fewer_reloads_after_each_kill_and_reds_on_a_restart() {
    let n = World::new(1).ms("cef_gpu_deaths_to_restart") as u32;
    let budget = World::new(1).ms("budget_gpu_death_reload_ms");
    let kills = |w: &World| {
        let mut k: Vec<i64> = w.marks.iter().filter(|m| m.0 == "kill").map(|m| m.1).collect();
        k.sort_unstable();
        k
    };
    let indices = |d: &[(u16, i64)]| d.iter().map(|x| x.0).collect::<Vec<_>>();
    let mut checked = 0;
    // The healthy run: each kill counted and reloaded (the second while the first reload loads, so
    // the first never gets ready), the overlay held through the reloads and back with the last
    // one's ready, within the budget of the last kill; every change due after the reload window
    // shown, and nothing off air.
    let w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-good");
    assert_pass(&v, "fewer-good");
    let report = judged_report("fewer-good", ONE_FEWER);
    let ks = kills(&w);
    let last = *ks.last().unwrap();
    let reloads = &v.json["measures"]["gpu_death_reloads"];
    let counts: Vec<u32> = (1..n).collect();
    assert_eq!(
        (&reloads["want"], &reloads["got"]),
        (&json!(counts), &json!(counts)),
        "fewer-good: one reload a count below the restart's: {reloads}"
    );
    let ready = last_reload_ready(&w).expect("fewer-good: a reload got ready");
    let back = first_strip_frame(&report, ready).expect("fewer-good: a frame after the ready");
    let backs = &v.json["measures"]["overlay_back_after_each_kill"]["kills"];
    let want: Vec<Value> = (0..ks.len())
        .map(|i| {
            json!({"kill_ms": ks[i], "reload_gen": 2 + i, "ready_gen": 1 + ks.len(),
                "back_ms": back, "back_after_ms": back - last})
        })
        .collect();
    assert_eq!(backs, &json!(want), "fewer-good: {backs}");
    assert!(back - last <= budget, "fewer-good: back within the budget");
    let from = reload_window_end(&w, &report, last);
    assert_eq!(from, back, "fewer-good: the window ends at the back frame");
    let due = due_after(&w, &report, from);
    let excused = w
        .timeline_events()
        .into_iter()
        .filter(|&(_, at)| at + w.delay_ms > last && at + w.delay_ms <= from)
        .count();
    let shown = &v.json["measures"]["scheduled_changes_shown"];
    eprintln!(
        "fewer-good: {} due after the reload window, {excused} due inside it: {shown}",
        due.len()
    );
    assert!(
        due.len() >= 2 && excused >= 1,
        "fewer-good: changes fall due inside the reload window and after it: {due:?}"
    );
    assert_eq!(
        (
            &shown["due_after_reload"],
            &shown["due_from_ms"],
            &shown["missing"]
        ),
        (&json!(due.len()), &json!(from), &json!([])),
        "fewer-good: {shown}"
    );
    checked += 1;
    // CI's green 40148976014: a 2-frame blip off air 51 ms after the first kill. Still green.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let k0 = kills(&w)[0];
    w.overlay_none.push((k0 + 51, k0 + 51 + 66));
    assert_pass(
        &judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-blip"),
        "fewer-blip",
    );
    checked += 1;
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let at = w.at(40.0);
    w.restart(at, 75, Some("restart in place: 2 CEF GPU-process deaths"));
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-restart"),
        "no_restart",
        "fewer-restart",
    );
    checked += 1;
    // The build before the fix (the harness's own mutant, "remove the reload"): no reload, the page
    // painting on. The reload check reds, and with no reload nothing is back after a kill; the
    // changes due after the budget all show, so that check holds.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    without_reloads(&mut w);
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-noreload");
    assert_eq!(
        failed_sorted(&v),
        sorted(&[
            "gpu_death_reloads",
            "overlay_back_after_each_kill",
            "stays_on_air"
        ]),
        "fewer-noreload"
    );
    assert_eq!(
        v.json["measures"]["gpu_death_reloads"]["got"],
        json!([]),
        "fewer-noreload"
    );
    checked += 1;
    // Owed 96's red, CI's 40148980537, after the fix: the reloads logged, and the overlay off air
    // 77 ms after the last kill to the end. Nothing comes back, so the due set starts at the budget,
    // and every change due after it is missing. Owed 96 left chaos.json with this fix, so the red
    // reads NOT OWED (chaos_table.rs).
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let last = *kills(&w).last().unwrap();
    w.overlay_none.push((last + 77, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-nooverlay");
    assert_eq!(
        failed_sorted(&v),
        sorted(&[
            "overlay_back_after_each_kill",
            "scheduled_changes_shown",
            "stays_on_air"
        ]),
        "fewer-nooverlay"
    );
    let report = judged_report("fewer-nooverlay", ONE_FEWER);
    let from = reload_window_end(&w, &report, last);
    assert_eq!(from, last + budget, "fewer-nooverlay: no back frame");
    let due = due_after(&w, &report, from);
    let shown = &v.json["measures"]["scheduled_changes_shown"];
    eprintln!("fewer-nooverlay: {} due, all missing: {shown}", due.len());
    assert!(due.len() >= 2, "fewer-nooverlay: {due:?}");
    assert_eq!(
        (
            &shown["due_after_reload"],
            &shown["due_from_ms"],
            &shown["missing"]
        ),
        (&json!(due.len()), &json!(from), &json!(indices(&due))),
        "fewer-nooverlay: {shown}"
    );
    checked += 1;
    // The reload budget, at its edge (a slow ready): back 100 ms inside it is green, 100 ms past it
    // reds on the back alone (no change falls due in the 134 ms between, and the overlay stays on
    // air from the late back). The product would have hidden the held frame at its bound before
    // either ready: fewer-slow-reload adds that hide.
    let lag = 800;
    for (tag, ready_ms, green) in [
        ("fewer-edge-back", budget - lag - 100, true),
        ("fewer-late-back", budget - lag + 100, false),
    ] {
        let w = gpu_deaths_reloading(n - 1, 2_000, lag, false, 0, ready_ms);
        let v = judge(&w, "cef-gpu-deaths", "one-fewer", tag);
        let report = judged_report(tag, ONE_FEWER);
        let last = *kills(&w).last().unwrap();
        let back = first_strip_frame(&report, last_reload_ready(&w).unwrap()).unwrap();
        eprintln!(
            "{tag}: back {} ms after the last kill, budget {budget}",
            back - last
        );
        if green {
            assert!(back - last <= budget, "{tag}");
            assert_pass(&v, tag);
        } else {
            assert!(back - last > budget, "{tag}");
            assert_eq!(
                failed_sorted(&v),
                sorted(&["overlay_back_after_each_kill"]),
                "{tag}"
            );
        }
        checked += 1;
    }
    // The PR #87 review's m-2: a reload slower than the held frame's bound, back inside the budget.
    // The domain hides the held frame at its bound (its proof at the latest the first kill, so
    // `cef_hang_after_ms` after it), with `reason=reload`, and the overlay is off air to the back:
    // the back budget passes it, and `no_hide_after_kill` alone reds.
    let mut w = gpu_deaths_reloading(n - 1, 2_000, lag, false, 0, budget - lag - 100);
    let (k0, ready) = (kills(&w)[0], last_reload_ready(&w).unwrap());
    let hide_at = k0 + w.ms("cef_hang_after_ms");
    assert!(
        hide_at < ready,
        "fewer-slow-reload: the hide falls before the ready"
    );
    w.line(hide_at, "overlay hidden reason=reload gen=1");
    w.overlay_none.push((hide_at, ready));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-slow-reload");
    assert_eq!(
        failed_sorted(&v),
        sorted(&["no_hide_after_kill"]),
        "fewer-slow-reload"
    );
    assert_eq!(
        v.json["measures"]["no_hide_after_kill"]["hides"],
        json!([{"at_ms": hide_at, "reason": "reload", "gen": 1, "after_kill_ms": hide_at - k0}]),
        "fewer-slow-reload"
    );
    checked += 1;
    // The PR #87 review's m-4: kills spaced past the budget (the case's 2 s spacing never lets a
    // reload ready before the next kill), so each reload readies before the next kill and each kill
    // is judged on its own back frame. Green, each kill back within the budget of itself; then the
    // first reload's page blank until 300 ms past its budget, still before the second kill: that kill
    // alone reds, which a check of the last kill alone would pass.
    let spacing = budget + 3_000;
    let w = gpu_deaths(n - 1, spacing, lag, false, 0);
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-spaced");
    assert_pass(&v, "fewer-spaced");
    let backs = v.json["measures"]["overlay_back_after_each_kill"]["kills"]
        .as_array()
        .unwrap()
        .clone();
    let ks = kills(&w);
    assert_eq!(backs.len(), ks.len(), "fewer-spaced: {backs:?}");
    for (b, &k) in backs.iter().zip(&ks) {
        let back = b["back_ms"].as_i64().unwrap();
        assert!(
            back > k && back - k <= budget && ks.iter().all(|&o| o <= k || o > back),
            "fewer-spaced: kill {k} back on its own before the next kill: {b}"
        );
    }
    let mut w = gpu_deaths(n - 1, spacing, lag, false, 0);
    let ks = kills(&w);
    let first_ready = w
        .log
        .iter()
        .find(|(_, m)| m == "cef ready gen=2")
        .map(|l| l.0)
        .expect("fewer-spaced-late-first: the first reload readied");
    let blank_to = ks[0] + budget + 300;
    assert!(
        first_ready < blank_to && blank_to < ks[1],
        "fewer-spaced-late-first: late, and before the second kill"
    );
    w.overlay_none.push((first_ready, blank_to));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-spaced-late-first");
    assert_eq!(
        failed_sorted(&v),
        sorted(&["overlay_back_after_each_kill"]),
        "fewer-spaced-late-first"
    );
    checked += 2;
    // Back at the reload's ready, then off air 500 ms later for good: one frame's worth of back is no
    // back (on_air_for), and the overlay never stays on air.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let ready = last_reload_ready(&w).unwrap();
    w.overlay_none.push((ready + 500, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-back-blinks");
    for check in [
        "overlay_back_after_each_kill",
        "stays_on_air",
        "scheduled_changes_shown",
    ] {
        assert!(
            failed_sorted(&v).contains(&check.to_owned()),
            "fewer-back-blinks: {check} held: {:?}",
            v.json["failed"]
        );
    }
    checked += 1;
    // The hole the old bound left: the page paints on after the reload, then goes off air for good
    // 30 s after the last kill. Back after every kill, so only staying on air and the changes due
    // after the off-air start red.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let last = *kills(&w).last().unwrap();
    let off_from = last + 30_000;
    w.overlay_none.push((off_from, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-offair-later");
    assert_eq!(
        failed_sorted(&v),
        sorted(&["scheduled_changes_shown", "stays_on_air"]),
        "fewer-offair-later"
    );
    let report = judged_report("fewer-offair-later", ONE_FEWER);
    let due = due_after(&w, &report, reload_window_end(&w, &report, last));
    // A change due within the overlay's lag (and two frames) of the off-air start would make the
    // split a coin: the world keeps clear of one.
    let slack = OVERLAY_LAG_MS + 2.0 * PERIOD_MS;
    assert!(
        due.iter()
            .all(|&(_, d)| (d as f64) < off_from as f64 - slack || d >= off_from),
        "fewer-offair-later: a change due at the off-air start: {due:?}"
    );
    let gone: Vec<(u16, i64)> = due.iter().copied().filter(|&(_, d)| d >= off_from).collect();
    assert!(
        !gone.is_empty() && gone.len() < due.len(),
        "fewer-offair-later: changes due on both sides of the off-air start: {due:?}"
    );
    let shown = &v.json["measures"]["scheduled_changes_shown"];
    assert_eq!(
        (&shown["due_after_reload"], &shown["missing"]),
        (&json!(due.len()), &json!(indices(&gone))),
        "fewer-offair-later: {shown}"
    );
    checked += 1;
    // A page that stops painting and keeps its last frame on air from the last kill (a stale score;
    // the re-review's m-1: round 3's `overlay_back` passed it): the strip reads at the reload's
    // ready, so it is "back" and stays on air, and only the changes shown red.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let last = *kills(&w).last().unwrap();
    w.overlay_hold.insert(0, (last, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-held");
    assert_eq!(
        failed_sorted(&v),
        sorted(&["scheduled_changes_shown"]),
        "fewer-held"
    );
    let shown = &v.json["measures"]["scheduled_changes_shown"];
    assert!(
        shown["due_after_reload"].as_u64().is_some_and(|n| n >= 1)
            && shown["missing"].as_array().is_some_and(|m| !m.is_empty()),
        "fewer-held: the changes due after the reload window are missing: {shown}"
    );
    checked += 1;
    // The same stale page after CI's 2-frame blip at the first kill.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let (k0, last) = (kills(&w)[0], *kills(&w).last().unwrap());
    w.overlay_none.push((k0 + 51, k0 + 51 + 66));
    w.overlay_hold.insert(0, (last, w.end + 60_000));
    assert_fails(
        &judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-blip-held"),
        "scheduled_changes_shown",
        "fewer-blip-held",
    );
    checked += 1;
    // Off air from BEFORE the first kill, to the end: never back, every change after the budget
    // missing.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let (k0, last) = (kills(&w)[0], *kills(&w).last().unwrap());
    w.overlay_none.push((k0 - 5_000, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-offair-before");
    assert_eq!(
        failed_sorted(&v),
        sorted(&[
            "overlay_back_after_each_kill",
            "scheduled_changes_shown",
            "stays_on_air"
        ]),
        "fewer-offair-before"
    );
    let due = due_after(
        &w,
        &judged_report("fewer-offair-before", ONE_FEWER),
        last + budget,
    );
    assert_eq!(
        v.json["measures"]["scheduled_changes_shown"]["missing"],
        json!(indices(&due)),
        "fewer-offair-before"
    );
    checked += 1;
    // Fix round 1's I-1 and m-1: the run records the final hold's end slate, off air from 500 ms
    // after the end request. A change due in the hold is no change the run owed (the due set ends at
    // the end request), so the run is green.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    w.tail_ms = w.ms("final_hold_ms");
    let end = w.end;
    w.overlay_none.push((end + 500, end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-final-hold");
    let fte = w.ms("budget_frame_to_encoder_ms");
    let report = judged_report("fewer-final-hold", ONE_FEWER);
    let last_frame = report["frames_table"]
        .as_array()
        .and_then(|f| f.last())
        .and_then(|f| f[0].as_i64())
        .unwrap();
    let in_hold = w
        .timeline_events()
        .into_iter()
        .filter(|&(_, at)| at + w.delay_ms > end + 500 && at + w.delay_ms <= last_frame - fte)
        .count();
    eprintln!("fewer-final-hold: {in_hold} changes fall due in the recorded hold, off air");
    assert!(
        last_frame > end + 500 && in_hold >= 1,
        "fewer-final-hold: the hold is recorded and a change falls due off air in it"
    );
    assert_pass(&v, "fewer-final-hold");
    checked += 1;
    // The graphics hid the overlay on purpose after the last kill and it stayed off: red.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let last = *kills(&w).last().unwrap();
    w.line(last + 60, "overlay hidden reason=hang gen=1");
    w.overlay_none.push((last + 77, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-hidden");
    assert_fails(&v, "overlay_back_after_each_kill", "fewer-hidden");
    assert_fails(&v, "scheduled_changes_shown", "fewer-hidden");
    assert_fails(&v, "no_hide_after_kill", "fewer-hidden");
    checked += 1;
    // A stale page then off air (spaced kills, so the first reload is superseded too): never back.
    let mut w = gpu_deaths(n - 1, 3_800, 800, false, 0);
    let last = *kills(&w).last().unwrap();
    let off_from = last + 850;
    w.overlay_hold.insert(0, (last, off_from));
    w.overlay_none.push((off_from, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-held-offair");
    assert_fails(&v, "overlay_back_after_each_kill", "fewer-held-offair");
    assert_fails(&v, "scheduled_changes_shown", "fewer-held-offair");
    checked += 1;
    // Off air at the last kill for 5 s, then a stale page to the end: back (the strip reads at the
    // reload's ready) and on air, but the changes due after the window are missing.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let last = *kills(&w).last().unwrap();
    let stale = last + 5_000;
    w.overlay_none.push((last + 77, stale));
    w.overlay_hold.insert(0, (stale, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-offair-held");
    assert_eq!(
        failed_sorted(&v),
        sorted(&["scheduled_changes_shown"]),
        "fewer-offair-held"
    );
    checked += 1;
    // CI's blip at the first kill, then CI's red at the last (the shape fix round 1's I-2 named): the
    // blip masks nothing, and every check over the last kill reds.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    let (k0, last) = (kills(&w)[0], *kills(&w).last().unwrap());
    w.overlay_none.push((k0 + 51, k0 + 51 + 66));
    w.overlay_none.push((last + 77, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-blip-offair");
    assert_eq!(
        failed_sorted(&v),
        sorted(&[
            "overlay_back_after_each_kill",
            "scheduled_changes_shown",
            "stays_on_air"
        ]),
        "fewer-blip-offair"
    );
    checked += 1;
    // Fix round 1's m-3: no kill landed. Every check over the kills reds on nothing (anti-vacuity),
    // and the measures still print: nothing due, nothing reloaded, no kill to come back after.
    let mut w = gpu_deaths(0, 2_000, 800, false, 0);
    let off_from = w.at(30.0);
    w.overlay_none.push((off_from, w.end + 60_000));
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-nokills");
    for check in [
        "gpu_death_reloads",
        "overlay_back_after_each_kill",
        "scheduled_changes_shown",
    ] {
        assert_fails(&v, check, "fewer-nokills");
    }
    let m = &v.json["measures"];
    assert_eq!(
        (
            &m["scheduled_changes_shown"]["due_after_reload"],
            &m["scheduled_changes_shown"]["missing"],
            &m["overlay_back_after_each_kill"]["kills"],
            &m["gpu_death_reloads"]["want"],
        ),
        (&json!(0), &json!([]), &json!([]), &json!([])),
        "fewer-nokills: {m}"
    );
    checked += 1;
    // The PR #79 confirm pass's C-3: the run never recorded its end request. The bound is null, so
    // nothing is due: a red whose measure prints the missing end request, never a jq error and a
    // null measure.
    let mut w = gpu_deaths(n - 1, 2_000, 800, false, 0);
    w.run_patch = Some(|r| r["end_request_ms"] = Value::Null);
    let v = judge(&w, "cef-gpu-deaths", "one-fewer", "fewer-noend");
    assert_fails(&v, "scheduled_changes_shown", "fewer-noend");
    let shown = &v.json["measures"]["scheduled_changes_shown"];
    assert_eq!(
        (
            &shown["end_request_ms"],
            &shown["due_after_reload"],
            &shown["missing"]
        ),
        (&Value::Null, &json!(0), &json!([])),
        "fewer-noend: {shown}"
    );
    assert!(shown.is_object(), "fewer-noend: the measure printed: {shown}");
    checked += 1;
    eprintln!("one-fewer worlds checked: {checked}");
    assert_eq!(checked, 22);
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

/// The PR #71 review's m1: only the hook cases run on the relay-test-hooks image, case 16
/// (cef-leaks, Owed 82's hook) and case 18 (output-hung, Owed 94's). Every leg's environment carries
/// `RELAY_TEST_HOOKS_IMAGE` (chaos.yml), and no other verdict reads which image ran, so a case that
/// copied their pattern, or a shared helper that picked the variable up, would run the hooks binary
/// while its verdict claims the relay image. Every file under harness/ (out/ aside) is swept: the
/// image's variable is named only by the two cases' files, each hook's keys and overlay only by its
/// own case's, and only their case files assign `RELAY_IMAGE` outside a comment. harness/soak/ is
/// swept like every other directory (the #71 confirm pass's r-1: its exemption excused no file).
#[test]
fn only_the_hook_cases_name_the_test_hooks_image_or_reassign_the_relay_image() {
    fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "out") {
                    walk(&p, files);
                }
            } else {
                files.push(p);
            }
        }
    }
    // An assignment outside a comment: `RELAY_IMAGE=`, `export RELAY_IMAGE=` or `${RELAY_IMAGE:=`,
    // never a longer name that ends in it.
    fn assigns_relay_image(text: &str) -> bool {
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .any(|l| {
                ["RELAY_IMAGE=", "RELAY_IMAGE:="].iter().any(|n| {
                    l.match_indices(n).any(|(i, _)| {
                        !l[..i]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                    })
                })
            })
    }
    let harness = repo().join("harness");
    let mut files = Vec::new();
    walk(&harness, &mut files);
    let case16 = ["cases/cef-leaks.sh", "compose.cef-leaks.yml"];
    let case18 = ["cases/output-hung.sh", "compose.output-hung.yml"];
    let case19 = ["cases/cef-blank.sh", "compose.cef-blank.yml"];
    let hooked = [case16[0], case16[1], case18[0], case18[1], case19[0], case19[1]];
    // Each hook's keys, in its overlay's order under the compositor's `environment`.
    let keys16 = ["COMPOSITOR_TEST_HOLD_TEARDOWNS"];
    let keys18 = [
        "COMPOSITOR_TEST_HOLD_SINK_AFTER_MS",
        "COMPOSITOR_TEST_HANG_SINK_AFTER_MS",
    ];
    let keys19 = ["COMPOSITOR_TEST_WITHHOLD_PAGES"];
    let names: [(&str, &[&str]); 8] = [
        ("RELAY_TEST_HOOKS_IMAGE", &hooked),
        (keys16[0], &case16),
        ("compose.cef-leaks.yml", &case16[..1]),
        (keys18[0], &case18),
        (keys18[1], &case18),
        ("compose.output-hung.yml", &case18[..1]),
        (keys19[0], &case19),
        ("compose.cef-blank.yml", &case19[..1]),
    ];
    let (mut swept, mut naming, mut assigning) = (0, Vec::new(), Vec::new());
    for p in &files {
        let rel = p.strip_prefix(&harness).unwrap().to_string_lossy().into_owned();
        let text = String::from_utf8_lossy(&std::fs::read(p).unwrap()).into_owned();
        swept += 1;
        for (name, allowed) in names {
            if text.contains(name) {
                assert!(
                    allowed.contains(&rel.as_str()),
                    "harness/{rel} names {name}: only {allowed:?} may"
                );
                naming.push(format!("{rel}: {name}"));
            }
        }
        if assigns_relay_image(&text) {
            assert!(
                [case16[0], case18[0], case19[0]].contains(&rel.as_str()),
                "harness/{rel} reassigns RELAY_IMAGE: only the hook cases point it at another image"
            );
            assigning.push(rel);
        }
    }
    eprintln!("harness files swept: {swept}; names: {naming:?}; RELAY_IMAGE assigned in: {assigning:?}");
    assert!(swept >= 25, "the sweep read too few harness files ({swept})");
    // The positive pairs, read from code and never from a comment (the #71 confirm pass's n2), so a
    // renamed variable cannot pass this vacuously and a reworded comment cannot red it: each hook
    // case names the image's variable, its keys and its overlay, and points RELAY_IMAGE at the image,
    // and its overlay's compositor `environment` carries its keys, in order.
    let code = |rel: &str| -> Vec<String> {
        std::fs::read_to_string(harness.join(rel))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .map(str::to_owned)
            .collect()
    };
    let mut pairs = 0;
    for (case, keys) in [
        (case16, &keys16[..]),
        (case18, &keys18[..]),
        (case19, &keys19[..]),
    ] {
        let file = code(case[0]);
        for name in ["RELAY_TEST_HOOKS_IMAGE", case[1]].iter().chain(keys) {
            assert!(
                file.iter().any(|l| l.contains(name)),
                "harness/{} no longer names {name} outside a comment",
                case[0]
            );
            pairs += 1;
        }
        let overlay = code(case[1]);
        let at = overlay
            .iter()
            .position(|l| l.trim_start().starts_with(&format!("{}:", keys[0])));
        assert!(
            at.is_some_and(|i| i >= 2
                && overlay[i - 1].trim() == "environment:"
                && overlay[i - 2].trim() == "compositor:"
                && keys.iter().enumerate().all(|(k, key)| overlay
                    .get(i + k)
                    .is_some_and(|l| l.trim_start().starts_with(&format!("{key}:"))))),
            "harness/{}: the compositor's `environment` no longer carries {keys:?}: {overlay:?}",
            case[1]
        );
        pairs += 1;
    }
    assigning.sort();
    assert_eq!(
        assigning,
        [case19[0], case16[0], case18[0]],
        "each hook case points RELAY_IMAGE at the image"
    );
    eprintln!("hook cases' positive pairs checked: {pairs}");
    assert_eq!(
        pairs,
        4 + 5 + 4,
        "case 16: three names and its overlay; case 18: four and its overlay; case 19: three and its overlay"
    );
    // The legs' own image (the #71 confirm pass's n1): chaos.yml sets each leg's RELAY_IMAGE, which
    // the harness/ sweep cannot see. Every job's RELAY_IMAGE is the images job's (the relay image it
    // builds), none names the test-hooks image, and no step reassigns it.
    let workflow = std::fs::read_to_string(repo().join(".github/workflows/chaos.yml")).unwrap();
    let (mut job, mut images) = (String::new(), Vec::new());
    for l in workflow.lines() {
        if let Some(name) = l
            .strip_prefix("  ")
            .and_then(|r| r.strip_suffix(':'))
            .filter(|r| !r.starts_with(' ') && !r.contains(' '))
        {
            job = name.to_owned();
        }
        if let Some(v) = l.trim_start().strip_prefix("RELAY_IMAGE:") {
            images.push((job.clone(), v.trim().to_owned()));
        }
    }
    eprintln!("chaos.yml's RELAY_IMAGE per job: {images:?}");
    let built = images
        .iter()
        .find(|(j, _)| j == "images")
        .map(|(_, v)| v.clone())
        .expect("the images job sets RELAY_IMAGE");
    assert!(
        images.iter().any(|(j, _)| j == "leg"),
        "the leg job sets RELAY_IMAGE"
    );
    for (j, v) in &images {
        assert_eq!(v, &built, "chaos.yml's {j} job runs another RELAY_IMAGE");
        assert!(!v.contains("test-hooks"), "chaos.yml's {j} job: {v}");
    }
    assert!(
        !assigns_relay_image(&workflow),
        "a chaos.yml step reassigns RELAY_IMAGE"
    );
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
    let ready = w.log.iter().find(|(_, m)| *m == next_ready).map(|l| l.0).unwrap();
    let v = judge(&w, "cef-leaks", "two", "leaks2-overlay-off-measure");
    let due = due_after(
        &w,
        &judged_report("leaks2-overlay-off-measure", "cef-leaks-two"),
        ready,
    );
    let on_air = &v.json["measures"]["overlay_on_air"];
    // Bounded by the last programme frame (Owed 96's hole): every change due after the newest
    // build's ready is missing, not "0 due".
    assert!(due.len() >= 2, "leaks2-overlay-off: {due:?}");
    assert_eq!(
        on_air["due_after_ready"],
        json!(due.len()),
        "leaks2-overlay-off: {on_air}"
    );
    assert_eq!(
        on_air["missing"],
        json!(due.iter().map(|d| d.0).collect::<Vec<_>>()),
        "leaks2-overlay-off: {on_air}"
    );
    red(w, &["overlay_on_air"], "leaks2-overlay-off");
    // The newest build on air, then off air for good 20 s later: the old bound (the last frame
    // whose overlay read) cut the due set there and passed.
    let mut w = leaks(hold);
    let ready = w.log.iter().find(|(_, m)| *m == next_ready).map(|l| l.0).unwrap();
    let off_from = ready + 20_000;
    w.overlay_none.push((off_from, w.end + 60_000));
    let v = judge(&w, "cef-leaks", "two", "leaks2-overlay-off-later-measure");
    let due = due_after(
        &w,
        &judged_report("leaks2-overlay-off-later-measure", "cef-leaks-two"),
        ready,
    );
    let lag = OVERLAY_LAG_MS + 2.0 * PERIOD_MS;
    assert!(
        due.iter()
            .all(|&(_, d)| (d as f64) < off_from as f64 - lag || d >= off_from),
        "leaks2-overlay-off-later: a change due at the off-air start: {due:?}"
    );
    let gone: Vec<u16> = due.iter().filter(|d| d.1 >= off_from).map(|d| d.0).collect();
    assert!(
        !gone.is_empty() && gone.len() < due.len(),
        "leaks2-overlay-off-later: changes due on both sides of the off-air start: {due:?}"
    );
    let on_air = &v.json["measures"]["overlay_on_air"];
    assert_eq!(
        on_air["missing"],
        json!(gone),
        "leaks2-overlay-off-later: {on_air}"
    );
    red(w, &["overlay_on_air"], "leaks2-overlay-off-later");
    // PR #79's fix round 1 (m-1): the run records the final hold's end slate, off air from 500 ms
    // after the end request. The due set ends at the end request, so the changes due in the hold
    // are owed by no one: green.
    let mut w = leaks(hold);
    w.tail_ms = w.ms("final_hold_ms");
    let end = w.end;
    w.overlay_none.push((end + 500, end + 60_000));
    let v = judge(&w, "cef-leaks", "two", "leaks2-final-hold");
    let report = judged_report("leaks2-final-hold", "cef-leaks-two");
    let last_frame = report["frames_table"]
        .as_array()
        .and_then(|f| f.last())
        .and_then(|f| f[0].as_i64())
        .unwrap();
    let budget = w.ms("budget_frame_to_encoder_ms");
    let in_hold = w
        .timeline_events()
        .into_iter()
        .filter(|&(_, at)| at + w.delay_ms > end + 500 && at + w.delay_ms <= last_frame - budget)
        .count();
    eprintln!("leaks2-final-hold: {in_hold} changes fall due in the recorded hold, off air");
    assert!(
        last_frame > end + 500 && in_hold >= 1,
        "leaks2-final-hold: the hold is recorded and a change falls due off air in it"
    );
    assert_pass(&v, "leaks2-final-hold");
    // The PR #79 confirm pass's C-3: no end request recorded. Nothing is due, and the red prints it.
    let mut w = leaks(hold);
    w.run_patch = Some(|r| r["end_request_ms"] = Value::Null);
    let v = judge(&w, "cef-leaks", "two", "leaks2-noend");
    assert_fails(&v, "overlay_on_air", "leaks2-noend");
    let on_air = &v.json["measures"]["overlay_on_air"];
    assert_eq!(
        (&on_air["end_request_ms"], &on_air["due_after_ready"]),
        (&Value::Null, &json!(0)),
        "leaks2-noend: {on_air}"
    );
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
    assert_eq!(checked, 33);
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

// ---- case 19, cef-blank (Owed 99) ----

/// Owed 99's arming line, before a process's `compositor started` (boot()'s, at +200). A build
/// armed with no page to withhold prints none (`arm_test_hooks`), and neither does the relay image.
fn arm_withhold(w: &mut World, start: i64, n: u64) {
    w.line(
        start + 150,
        format!("WARN test hook (Owed 99): the next {n} CEF pages' frames never reach the slot"),
    );
}

/// The adapter's line for one withheld build, `left` the builds still to withhold after it.
fn withheld_line(gen: u64, left: u64) -> String {
    format!("WARN test hook (Owed 99) gen={gen}: this CEF page's frames never reach the slot ({left} more to withhold)")
}

/// Case 19's world (Owed 99, option B): a compositor armed to withhold every frame of its first
/// `withhold` CEF pages. Each withheld page loads, is ready (boot()'s `cef ready gen=1` the first)
/// and beats, and nothing of it reaches the slot. `after_bound_ms` past `cef_hang_after_ms` from its
/// ready, the driver logs its blank reload as the next gen (`cef reload gen=<g> reason=no_frames
/// blank_pages=<k>`), then `graphics restart` and the load, and that gen is ready `ready_ms` later.
/// The page after the last withheld one paints at its ready (the latched first frame: `cef
/// painted`), and the overlay is off air from the programme's first frame to just after it.
/// `withhold` 0 is the relay image: no arming line, the first page painting at once.
fn blank(withhold: u64, after_bound_ms: i64, ready_ms: i64) -> World {
    let mut w = World::new(60);
    let hang = w.ms("cef_hang_after_ms");
    let boot = w.live - 9_000;
    if withhold > 0 {
        arm_withhold(&mut w, boot, withhold);
    }
    w.line(boot + 1_000, format!("gen=1: loading {LEAK_ORIGIN}"));
    if withhold > 0 {
        w.line(boot + 1_001, withheld_line(1, withhold - 1));
    }
    let mut ready = boot + 1_500;
    for k in 1..=withhold {
        let gen = k + 1;
        let at = ready + hang + after_bound_ms;
        w.line(
            at,
            format!("cef reload gen={gen} reason=no_frames blank_pages={k}"),
        );
        w.line(at + 1, format!("graphics restart gen={gen}"));
        w.line(at + 2, format!("gen={gen}: loading {LEAK_ORIGIN}"));
        if gen <= withhold {
            w.line(at + 3, withheld_line(gen, withhold - gen));
        }
        ready = at + ready_ms;
        w.line(ready, format!("cef ready gen={gen}"));
    }
    w.line(ready + 1, format!("cef painted gen={}", withhold + 1));
    w.overlay_none.push((w.live - 30_000, ready + 100));
    w
}

/// The withheld pages case 19 drives: one fewer than the shared page-fault count, so each is
/// reloaded and none restarts the process.
fn blank_n() -> u64 {
    World::new(1).ms("cef_gpu_deaths_to_restart") as u64 - 1
}

const BLANK: &str = "cef-blank";

#[test]
fn cef_blank_reloads_each_blank_page_and_reds_without_the_watchdog_or_its_witnesses() {
    let n = blank_n();
    let t = World::new(1);
    let (hang, fp) = (
        t.ms("cef_hang_after_ms"),
        t.t["frame_period_ms"].as_f64().unwrap(),
    );
    let budget = t.ms("budget_blank_page_reload_ms");
    assert!(n >= 1, "case 19 withholds at least one page");
    let w = blank(n, 20, RELOAD_READY_MS);
    let v = judge(&w, BLANK, "", "blank-good");
    assert_pass(&v, "blank-good");
    // Anti-vacuity: every withheld page was judged, frames were read off air while they were, the
    // back came after the bound (the watchdog's, never a page that painted on its own), and a score
    // change fell due after it.
    let m = &v.json["measures"];
    let pages = m["blank_reloads"]["pages"].as_array().unwrap().len();
    let off = m["off_air_while_withheld"]["frames_before"].as_u64().unwrap();
    let back_after = m["overlay_back"]["back_after_ms"].as_i64().unwrap();
    let due = m["scheduled_changes_shown"]["due_after_back"].as_u64().unwrap();
    eprintln!(
        "blank-good: {pages} pages, {off} frames off air, back {back_after} ms (budget {budget}), {due} due"
    );
    assert_eq!(pages as u64, n, "blank-good: the pages judged");
    assert!(off >= 1 && due >= 1, "blank-good: {m}");
    assert!(
        back_after > hang && back_after <= budget,
        "blank-good: {back_after}"
    );
    let mut checked = 0;
    let mut red = |w: World, want: &[&str], tag: &str| {
        let v = judge(&w, BLANK, "", tag);
        assert_eq!(v.status, "fail", "{tag}");
        assert_eq!(v.json["checks_run"], v.json["checks_declared"], "{tag}");
        assert_eq!(failed_sorted(&v), sorted(want), "{tag}");
        checked += 1;
    };
    // Mutant "no watchdog": the first page is never called blank, so nothing reloads and nothing
    // is ever on air.
    let mut w = blank(n, 20, RELOAD_READY_MS);
    w.log.retain(|(_, m)| {
        !(m.starts_with("cef reload ")
            || m.starts_with("graphics restart ")
            || m.starts_with("cef painted ")
            || (m.starts_with("cef ready gen=") && m != "cef ready gen=1")
            || (m.starts_with("gen=") && !m.starts_with("gen=1:"))
            || (m.starts_with("WARN test hook (Owed 99) gen=") && !m.contains(" gen=1:")))
    });
    w.overlay_none = vec![(w.live - 30_000, w.end + 60_000)];
    red(
        w,
        &[
            "withheld_n",
            "blank_reloads",
            "off_air_while_withheld",
            "painted_once",
            "overlay_back",
            "stays_on_air",
            "scheduled_changes_shown",
        ],
        "blank-no-watchdog",
    );
    // Mutant "bound flipped": the blank called two frame periods early, or three late.
    red(
        blank(n, -2 * fp as i64, RELOAD_READY_MS),
        &["blank_reloads"],
        "blank-early",
    );
    red(
        blank(n, 3 * fp as i64, RELOAD_READY_MS),
        &["blank_reloads"],
        "blank-late",
    );
    // The page that painted is called blank too (the watch never ended): a reload past the count.
    let mut w = blank(n, 20, RELOAD_READY_MS);
    let after = w.end - 10_000;
    w.line(
        after,
        format!("cef reload gen={} reason=no_frames blank_pages={}", n + 2, n + 1),
    );
    red(w, &["blank_reloads"], "blank-painted-page-reloaded");
    // The relay image (no hook): no arming line, nothing withheld, the first page on air at once.
    red(
        blank(0, 20, RELOAD_READY_MS),
        &[
            "armed",
            "withheld_n",
            "blank_reloads",
            "off_air_while_withheld",
            "overlay_back",
        ],
        "blank-unarmed",
    );
    // The hook leaked a frame: a withheld page reported painted, or the overlay on air while the
    // pages were withheld.
    let mut w = blank(n, 20, RELOAD_READY_MS);
    w.line(w.live - 7_499, "cef painted gen=1");
    red(w, &["painted_once"], "blank-withheld-painted");
    let mut w = blank(n, 20, RELOAD_READY_MS);
    w.overlay_none = vec![(w.live - 30_000, w.live)];
    red(w, &["off_air_while_withheld"], "blank-withheld-on-air");
    // The budget, at its edge: back three frame periods inside it passes, one past it reds.
    let edge = budget - hang - 20 - 100;
    assert_pass(
        &judge(
            &blank(n, 20, edge - (3.0 * fp) as i64),
            BLANK,
            "",
            "blank-budget-inside",
        ),
        "blank-budget-inside",
    );
    red(
        blank(n, 20, edge + fp as i64),
        &["overlay_back"],
        "blank-budget-past",
    );
    // A blank page hidden on its reload (nothing was on air to hide).
    let mut w = blank(n, 20, RELOAD_READY_MS);
    let first = w.live - 9_000 + 1_500 + hang + 20;
    w.line(first, "overlay hidden reason=reload gen=1");
    red(w, &["no_hide"], "blank-hidden");
    // The page after paints one frame and freezes: on air, never fresh.
    let mut w = blank(n, 20, RELOAD_READY_MS);
    let back = w.overlay_none[0].1;
    w.overlay_hold.push((back, w.end + 60_000));
    red(w, &["scheduled_changes_shown"], "blank-frozen");
    // A blank counted early: the first blank page restarts the process in place, below the count.
    let mut w = blank(n, 20, RELOAD_READY_MS);
    let first = w
        .log
        .iter()
        .find(|(_, m)| m.starts_with("cef reload gen=2 "))
        .map(|(at, _)| *at)
        .unwrap();
    w.log.retain(|(at, m)| {
        *at < first
            || ![
                "cef reload ",
                "graphics restart ",
                "gen=",
                "cef ready ",
                "cef painted ",
                "WARN test hook (Owed 99) gen=",
            ]
            .iter()
            .any(|p| m.starts_with(p))
    });
    w.overlay_none = vec![(w.live - 30_000, w.end + 60_000)];
    w.restart(
        first + 300,
        75,
        Some("restart in place: 1 blank CEF pages, 0 GPU-process deaths"),
    );
    let v = judge(&w, BLANK, "", "blank-counted-early");
    for check in ["no_restart", "fp_no_exit_75", "blank_reloads"] {
        assert_fails(&v, check, "blank-counted-early");
    }
    checked += 1;
    eprintln!("cef-blank's reds checked: {checked}");
    assert_eq!(checked, 11);
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
        n, 19,
        "the brief's 16 cases (amended 2026-10-20), case 17 (2026-10-24), case 18 (Owed 94) and case 19 (Owed 99)"
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

/// The verdict ran `check` and it passed (whatever else the world reds).
fn assert_check_passes(v: &Verdict, check: &str, tag: &str) {
    assert!(
        v.lines.iter().any(|l| l.starts_with(&format!("PASS {check}"))),
        "{tag}: no `PASS {check}` line; failed {:?}",
        v.json["failed"]
    );
}

/// A leg with a back check: (tag, case, variant, its world, its back frame's time).
type BackLeg = (
    &'static str,
    &'static str,
    &'static str,
    fn() -> World,
    fn(&World) -> i64,
);

/// The PR #79 confirm pass's C-4 (a coordinator decision): a restart that comes back, holds for the
/// hold and then leaves the air passed every restart leg. `stays_on_air` judges each leg with a back
/// check from its back frame to the end request: no more strip-less frames in all than the
/// frame-to-encoder budget holds (a total, so a strobe reds too; no run set aside at a kill, the
/// approval's F-1), and an empty span red.
#[test]
fn stays_on_air_reds_a_page_that_leaves_after_coming_back_in_every_leg_with_a_back_check() {
    let t = World::new(1);
    let allowance = (t.t["budget_frame_to_encoder_ms"].as_f64().unwrap()
        / t.t["frame_period_ms"].as_f64().unwrap())
    .floor() as u64;
    let good = || json!({"killed": [201, 202, 203], "types": ["zygote", "zygote", "gpu-process"]});
    let restart_back = |w: &World| *w.starts().last().unwrap() + FIRST_PACKET_MS;
    // Each leg with a back check, its world and its back: the restart legs' first programme frame
    // after the restart, cef-hung's strip back after the hide, score-down's first good poll,
    // cef-blank's first frame of the page after the withheld ones.
    let legs: Vec<BackLeg> = vec![
        (
            "sty-8",
            "cef-killed",
            "",
            || {
                cef_killed(
                    json!({"killed": [201, 202, 203], "types": ["zygote", "zygote", "gpu-process"]}),
                    133,
                )
            },
            |w| *w.starts().last().unwrap() + FIRST_PACKET_MS,
        ),
        (
            "sty-13",
            "cef-gpu-zygote-killed",
            "",
            || zygote("gpu-process", true, "zygote"),
            |w| *w.starts().last().unwrap() + FIRST_PACKET_MS,
        ),
        (
            "sty-14",
            "cef-renderer-zygote-killed",
            "",
            || zygote("renderer", true, "zygote"),
            |w| *w.starts().last().unwrap() + FIRST_PACKET_MS,
        ),
        (
            "sty-15t",
            "cef-gpu-deaths",
            "threshold",
            || {
                let n = World::new(1).ms("cef_gpu_deaths_to_restart") as u32;
                gpu_deaths(n, 2_000, 800, true, 600)
            },
            |w| *w.starts().last().unwrap() + FIRST_PACKET_MS,
        ),
        (
            "sty-15f",
            "cef-gpu-deaths",
            "fast",
            || {
                let a = World::new(1).ms("chromium_gpu_deaths_abort") as u32;
                gpu_deaths(a - 1, 700, 350, true, 800)
            },
            |w| *w.starts().last().unwrap() + FIRST_PACKET_MS,
        ),
        (
            "sty-16",
            "cef-leaks",
            "three",
            || leaks(World::new(1).ms("cef_leaks_to_restart") as u64),
            |w| *w.starts().last().unwrap() + FIRST_PACKET_MS,
        ),
        (
            "sty-11",
            "cef-hung",
            "",
            || cef_hung(true),
            |w| w.overlay_none[0].1,
        ),
        (
            "sty-7",
            "score-down",
            "",
            || score_down(true, false),
            |w| w.score_blind.unwrap().1 + w.ms("harness_page_poll_ms"),
        ),
        (
            "sty-19",
            BLANK,
            "",
            || blank(blank_n(), 20, RELOAD_READY_MS),
            |w| w.overlay_none[0].1,
        ),
    ];
    let mut checked = 0;
    for (tag, case, variant, make, back_of) in &legs {
        // Healthy: the span holds frames, none of them off air.
        let w = make();
        let v = judge(&w, case, variant, &format!("{tag}-good"));
        assert_check_passes(&v, "stays_on_air", tag);
        let m = &v.json["measures"]["stays_on_air"];
        assert!(
            m["span_frames"].as_u64().is_some_and(|n| n > 0)
                && m["longest_frames"] == json!(0)
                && m["off_air_frames"] == json!(0)
                && m["allowance_frames"] == json!(allowance),
            "{tag}-good: {m}"
        );
        // The C-4 probe's shape: back, held 1.5 s (past the hold), then off air to the end.
        let mut w = make();
        let back = back_of(&w);
        w.overlay_none.push((back + 1_500, w.end + 60_000));
        let v = judge(&w, case, variant, &format!("{tag}-leaves"));
        assert_fails(&v, "stays_on_air", &format!("{tag}-leaves"));
        let m = &v.json["measures"]["stays_on_air"];
        assert!(
            m["longest_frames"].as_u64().is_some_and(|n| n > allowance),
            "{tag}-leaves: {m}"
        );
        checked += 1;
    }
    eprintln!("legs with a back check judged: {checked}");
    assert_eq!(checked, 9);

    // A blip the allowance holds passes; one frame more reds (cef-killed, 2 s after back).
    let w = cef_killed(good(), 133);
    let healthy = judge(&w, "cef-killed", "", "sty-blip-ref");
    let back = restart_back(&w);
    let w0 = judged_report("sty-blip-ref", "cef-killed")["frames_table"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f[0].as_i64())
        .find(|&ms| ms >= back + 2_000)
        .unwrap();
    assert_check_passes(&healthy, "stays_on_air", "sty-blip-ref");
    for (frames, red) in [(allowance, false), (allowance + 1, true)] {
        let mut w = cef_killed(good(), 133);
        let from = w0 - 10;
        w.overlay_none
            .push((from, from + (frames as f64 * PERIOD_MS) as i64));
        let tag = format!("sty-blip-{frames}");
        let v = judge(&w, "cef-killed", "", &tag);
        let m = &v.json["measures"]["stays_on_air"];
        assert_eq!(m["longest_frames"], json!(frames), "{tag}: {m}");
        if red {
            assert_fails(&v, "stays_on_air", &tag);
        } else {
            assert_check_passes(&v, "stays_on_air", &tag);
        }
    }
    // The approval's F-1: no run is set aside at a kill. CI's 2-frame blip at a kill after the back
    // frame fits the allowance and passes; a run at that kill longer than the allowance reds, and so
    // does one from that kill to the end.
    let later = back + 3_000;
    let later_kill = |w: &mut World| {
        w.mark("kill", later, json!({"at_ms": later, "killed": [], "types": []}));
    };
    let mut w = cef_killed(good(), 133);
    later_kill(&mut w);
    w.overlay_none.push((later + 51, later + 51 + 66));
    let v = judge(&w, "cef-killed", "", "sty-later-blip");
    assert_check_passes(&v, "stays_on_air", "sty-later-blip");
    let m = &v.json["measures"]["stays_on_air"];
    assert!(
        m["off_air_frames"]
            .as_u64()
            .is_some_and(|n| (1..=allowance).contains(&n)),
        "sty-later-blip: {m}"
    );
    for (tag, to) in [
        (
            "sty-later-long",
            later + 51 + ((allowance + 3) as f64 * PERIOD_MS) as i64,
        ),
        ("sty-later-to-end", w.end + 60_000),
    ] {
        let mut w = cef_killed(good(), 133);
        later_kill(&mut w);
        w.overlay_none.push((later + 51, to));
        let v = judge(&w, "cef-killed", "", tag);
        assert_fails(&v, "stays_on_air", tag);
        let m = &v.json["measures"]["stays_on_air"];
        assert!(
            m["longest_frames"].as_u64().is_some_and(|n| n > allowance),
            "{tag}: {m}"
        );
    }
    // The strobe: off for the allowance, on for one frame, three times. No run is longer than the
    // allowance, but the span's total off air is: red.
    let mut w = cef_killed(good(), 133);
    let cycle = (allowance + 1) as f64 * PERIOD_MS;
    for i in 0..3 {
        let from = w0 - 10 + (f64::from(i) * cycle) as i64;
        w.overlay_none
            .push((from, from + (allowance as f64 * PERIOD_MS) as i64));
    }
    let v = judge(&w, "cef-killed", "", "sty-strobe");
    assert_fails(&v, "stays_on_air", "sty-strobe");
    let m = &v.json["measures"]["stays_on_air"];
    assert_eq!(
        (&m["runs"], &m["longest_frames"], &m["off_air_frames"]),
        (&json!(3), &json!(allowance), &json!(3 * allowance)),
        "sty-strobe: {m}"
    );
    // The span ends at the end request: the final hold's end slate, off air, is not this check's.
    let mut w = cef_killed(good(), 133);
    w.tail_ms = w.ms("final_hold_ms");
    let end = w.end;
    w.overlay_none.push((end + 500, end + 60_000));
    let v = judge(&w, "cef-killed", "", "sty-final-hold");
    assert_check_passes(&v, "stays_on_air", "sty-final-hold");
    assert!(
        judged_report("sty-final-hold", "cef-killed")["frames_table"]
            .as_array()
            .and_then(|f| f.last())
            .and_then(|f| f[0].as_i64())
            .is_some_and(|l| l > end + 500),
        "sty-final-hold: the hold is recorded"
    );
    // Zero frames in the span proves nothing: an end request before the back frame reds.
    let mut w = cef_killed(good(), 133);
    w.run_patch = Some(|r| r["end_request_ms"] = json!(0));
    let v = judge(&w, "cef-killed", "", "sty-empty");
    assert_fails(&v, "stays_on_air", "sty-empty");
    assert_eq!(
        v.json["measures"]["stays_on_air"]["span_frames"],
        json!(0),
        "sty-empty"
    );
}
