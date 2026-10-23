#![allow(clippy::print_stdout)] // harness tools report on stdout by design (C13)
//! fake-control (C13): R1's relay routes, the score feed and the overlay page, on one port
//! (`harness::control`). Credentials come from the environment, never argv: `JOB_TOKEN`,
//! `SESSION_ID` and `HARNESS_SRT_PASSPHRASE` (MediaMTX's read passphrase for the camera's path).
//!
//! ```text
//! fake-control --fixture session-composed-720p.json --page overlay-test.html --score score.json \
//!   --beats /out/heartbeats.jsonl --listen 0.0.0.0:7000 --input-origin srt://mediamtx:8890 \
//!   --target-origin rtmp://mediamtx:1935 --overlay-origin http://fake-control:7000 \
//!   --stream-id read:cam --stream-key dest
//! ```
//!
//! Before it listens it parses the rewritten session with C07's own parser
//! (`adapters::control_http::parse_session`) and checks the parsed id is `SESSION_ID`: a rewrite the
//! compositor would refuse stops here (exit 78), not as a confusing compositor exit later.
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::LineWriter;
use std::process::ExitCode;
use std::sync::Arc;

use adapters::control_http::parse_session;
use adapters::redact::Scrubber;
use harness::clock::now_ms;
use harness::control::{rewrite_session, serve, Fake, RewriteMap, ScriptedTimeline};
use serde_json::Value;

const FLAGS: [&str; 10] = [
    "--fixture",
    "--page",
    "--score",
    "--beats",
    "--listen",
    "--input-origin",
    "--target-origin",
    "--overlay-origin",
    "--stream-id",
    "--stream-key",
];

/// The environment's credentials (never argv).
const SECRETS: [&str; 3] = ["JOB_TOKEN", "SESSION_ID", "HARNESS_SRT_PASSPHRASE"];

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
    if let Some(missing) = FLAGS.iter().find(|f| !out.contains_key(*f)) {
        return Err(format!("{missing} is required"));
    }
    Ok(out)
}

fn env(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(format!("{name} is not set")),
    }
}

fn json_file(path: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
}

fn run() -> Result<(), (u8, String)> {
    let with = |code: u8| move |e: String| (code, e);
    let a = args().map_err(with(64))?;
    let token = env("JOB_TOKEN").map_err(with(64))?;
    let session_id = env("SESSION_ID").map_err(with(64))?;
    let passphrase = env("HARNESS_SRT_PASSPHRASE").map_err(with(64))?;
    let map = RewriteMap {
        input_origin: a["--input-origin"].clone(),
        target_origin: a["--target-origin"].clone(),
        overlay_origin: a["--overlay-origin"].clone(),
        stream_id: a["--stream-id"].clone(),
        stream_key: a["--stream-key"].clone(),
        passphrase,
    };
    let fixture = json_file(&a["--fixture"]).map_err(with(66))?;
    let session = rewrite_session(&fixture, &session_id, &map).map_err(with(78))?;
    // The self-check: C07's parser, and C07 M3's id check (case-insensitive, as the compositor's).
    let parsed =
        parse_session(&session).map_err(|e| (78, format!("the rewritten session does not parse: {e:?}")))?;
    if !parsed.session_id.eq_ignore_ascii_case(&session_id) {
        return Err((78, "the rewritten session's id is not SESSION_ID".into()));
    }
    let timeline = ScriptedTimeline::parse(&json_file(&a["--score"]).map_err(with(66))?).map_err(with(78))?;
    let page = std::fs::read(&a["--page"]).map_err(|e| (66, format!("{}: {e}", a["--page"])))?;
    let beats = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&a["--beats"])
        .map_err(|e| (73, format!("{}: {e}", a["--beats"])))?;
    let events = timeline.events.len();
    // The timeline waits for `POST /control/timeline-start` (run-case.sh, once the programme is live).
    let fake = Fake::new(
        token,
        session,
        page,
        timeline,
        None,
        Box::new(LineWriter::new(beats)),
    )
    .map_err(with(78))?;
    let server = tiny_http::Server::http(a["--listen"].as_str()).map_err(|e| (69, format!("listen: {e}")))?;
    println!(
        "fake-control: listening on {}; the session parses (composed, {}x{}); {events} score events, waiting for the timeline start",
        a["--listen"], parsed.output.width, parsed.output.height
    );
    serve(Arc::new(fake), Arc::new(server), 4, now_ms);
    Err((70, "the server stopped".into()))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, e)) => {
            // No error above quotes a credential; every line passes the scrubber anyway.
            let mut s = Scrubber::default();
            for v in SECRETS {
                if let Ok(x) = std::env::var(v) {
                    s.add(&x);
                }
            }
            println!("fake-control: {}", s.scrub(&e));
            ExitCode::from(code)
        }
    }
}
