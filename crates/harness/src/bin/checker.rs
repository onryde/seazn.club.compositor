#![allow(clippy::print_stdout)] // harness tools report on stdout by design (C13)
//! The reference checker (C13): decodes the destination's recorded segments and writes the run's
//! facts as one JSON report (`harness::checker::Report`). It judges nothing but its own inputs; the
//! case's verdict reads the report.
//!
//! ```text
//! checker --recordings out/rec --emit out/emit.jsonl --heartbeats out/heartbeats.jsonl \
//!   --timeline out/timeline.json --compositor-log out/compositor.log [--end-request-ms N] \
//!   --out out/report.json
//! ```
//!
//! Exit 0: a report, from at least one decoded frame, with no checker failure. Exit 1: a report was
//! written, but nothing decoded (zero frames is never a pass) or the analysis failed the run
//! (`checker_failures`, e.g. a rung outside the declared ones). Exit 2: an input could not be read,
//! and no report was written.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use harness::checker::{analyze, Inputs, Settings};
use harness::inputs::{crash_times, read_beats, read_emit, read_timeline};
use harness::media;

const FLAGS: [&str; 7] = [
    "--recordings",
    "--emit",
    "--heartbeats",
    "--timeline",
    "--compositor-log",
    "--end-request-ms",
    "--out",
];
const OPTIONAL: [&str; 1] = ["--end-request-ms"];

fn args() -> Result<BTreeMap<&'static str, String>, String> {
    let mut out = BTreeMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let flag = FLAGS
            .iter()
            .find(|f| **f == a)
            .ok_or(format!("unknown argument {a}"))?;
        out.insert(*flag, it.next().ok_or(format!("{flag} needs a value"))?);
    }
    if let Some(missing) = FLAGS
        .iter()
        .find(|f| !OPTIONAL.contains(f) && !out.contains_key(*f))
    {
        return Err(format!("{missing} is required"));
    }
    Ok(out)
}

fn run() -> Result<bool, String> {
    let a = args()?;
    media::init()?;
    let segments = media::scan_recordings(Path::new(&a["--recordings"]))?;
    let emit = read_emit(Path::new(&a["--emit"]))?;
    let beats = read_beats(Path::new(&a["--heartbeats"]))?;
    let timeline = read_timeline(Path::new(&a["--timeline"]))?;
    let log = std::fs::read_to_string(&a["--compositor-log"])
        .map_err(|e| format!("{}: {e}", a["--compositor-log"]))?;
    let end_request_ms = match a.get("--end-request-ms") {
        Some(v) => Some(v.parse::<i64>().map_err(|_| "--end-request-ms: not an integer")?),
        None => None,
    };
    let inputs = Inputs {
        segments,
        emit: emit.items,
        emit_truncated: emit.truncated,
        beats: beats.items,
        beats_truncated: beats.truncated,
        timeline,
        crashes_ms: crash_times(&log),
        end_request_ms,
    };
    let report = analyze(
        &inputs,
        &Settings::from_thresholds(&domain::config::Thresholds::default()),
    );
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&a["--out"], text + "\n").map_err(|e| format!("{}: {e}", a["--out"]))?;

    let strip_frames = report.camera.strip_frames;
    println!(
        "checker: {} segments read ({} decode errors), {} frames decoded ({strip_frames} with a camera strip, {} with a score strip), {} emit lines, {} heartbeats",
        report.inputs.segments_read,
        report.inputs.decode_errors.len(),
        report.frames,
        report.overlay.frames_read,
        report.inputs.emit_lines,
        report.heartbeats.count,
    );
    for e in &report.inputs.decode_errors {
        println!("checker: decode error in {}: {}", e.file, e.error);
    }
    let mut ok = true;
    if report.frames == 0 {
        println!("checker: FAIL nothing was decoded (zero frames is never a pass)");
        ok = false;
    }
    for f in &report.checker_failures {
        println!("checker: FAIL {f}");
        ok = false;
    }
    println!("checker: report written to {}", a["--out"]);
    Ok(ok)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            println!("checker: {e}");
            ExitCode::from(2)
        }
    }
}
