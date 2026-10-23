#![allow(clippy::print_stdout)] // harness tools report on stdout by design (C13)
//! fake-camera (C13): 30 fps frames with the burned-in strip and the tone bursts, encoded as the
//! capture app does (H.264 + AAC in MPEG-TS) and published to MediaMTX over SRT
//! (`harness::media::CameraPipeline`). Every frame pushed is one line of `--emit`
//! (`{"counter","wall_ms"}`). The controls (`harness::camera`) listen on `--listen`.
//!
//! ```text
//! fake-camera --emit /out/emit.jsonl --listen 0.0.0.0:7001 --srt-host mediamtx --srt-port 8890 \
//!   --stream-id publish:cam [--size 1280x720] [--pattern ball|snow|noise] [--latency-ms 200]
//! ```
//!
//! The SRT passphrase is `HARNESS_SRT_PASSPHRASE` (environment, never argv). A failed build (MediaMTX
//! not up yet, a kicked publisher) is logged and rebuilt a second later; the counter keeps its pace
//! meanwhile, so the frames due in between are missing from `--emit`, as a real camera's would be.
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{LineWriter, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use adapters::redact::Scrubber;
use harness::camera::{self, parse_command, Pattern, Schedule};
use harness::clock::now_ms;
use harness::media::{self, CameraPipeline, CameraSink, SrtTarget, FRAME_NS};
use serde_json::json;

const FLAGS: [&str; 8] = [
    "--emit",
    "--listen",
    "--srt-host",
    "--srt-port",
    "--stream-id",
    "--size",
    "--pattern",
    "--latency-ms",
];
const REQUIRED: [&str; 5] = ["--emit", "--listen", "--srt-host", "--srt-port", "--stream-id"];
/// How long a failed build waits before the next.
const REBUILD_AFTER: Duration = Duration::from_secs(1);
/// How often the camera prints its count.
const REPORT_EVERY_FRAMES: u64 = 10 * camera::FPS;

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
    if let Some(missing) = REQUIRED.iter().find(|f| !out.contains_key(*f)) {
        return Err(format!("{missing} is required"));
    }
    Ok(out)
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("--size is WxH")?;
    let (w, h) = (
        w.parse().map_err(|_| "--size: bad width")?,
        h.parse().map_err(|_| "--size: bad height")?,
    );
    camera::check_size(w, h)?;
    Ok((w, h))
}

/// The control port: one request at a time, each answered and logged (method, path, status).
fn controls(server: tiny_http::Server, schedule: Arc<Mutex<Schedule>>, pushed: Arc<Mutex<u64>>) {
    for req in server.incoming_requests() {
        let (method, url) = (req.method().as_str().to_owned(), req.url().to_owned());
        let path = url.split('?').next().unwrap_or("").to_owned();
        let (status, body) = match (method.as_str(), path.as_str()) {
            ("GET", "/healthz") => (200, json!({"ok": true})),
            ("GET", "/state") => {
                let s = locked(&schedule);
                (
                    200,
                    json!({"paused": s.paused(), "backlog": s.backlog(), "size": s.size(), "pushed": *locked(&pushed)}),
                )
            }
            _ => match parse_command(&method, &url).and_then(|c| locked(&schedule).command(c, now_ms())) {
                Ok(()) => (200, json!({"ok": true})),
                Err(e) => (400, json!({"ok": false, "error": e})),
            },
        };
        println!("fake-camera {method} {path} {status}");
        let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .expect("a static header");
        let _ = req.respond(
            tiny_http::Response::from_string(body.to_string())
                .with_status_code(status)
                .with_header(header),
        );
    }
}

fn run() -> Result<(), (u8, String)> {
    let with = |code: u8| move |e: String| (code, e);
    let a = args().map_err(with(64))?;
    let passphrase = std::env::var("HARNESS_SRT_PASSPHRASE").unwrap_or_default();
    if passphrase.is_empty() {
        return Err((64, "HARNESS_SRT_PASSPHRASE is not set".into()));
    }
    let mut scrub = Scrubber::default();
    scrub.add(&passphrase);
    let (w, h) = size(a.get("--size").map_or("1280x720", String::as_str)).map_err(with(64))?;
    let pattern = Pattern::parse(a.get("--pattern").map_or("ball", String::as_str)).map_err(with(64))?;
    let target = SrtTarget {
        host: a["--srt-host"].clone(),
        port: a["--srt-port"]
            .parse()
            .map_err(|_| (64, "--srt-port: not a port".to_owned()))?,
        stream_id: a["--stream-id"].clone(),
        passphrase,
        latency_ms: a
            .get("--latency-ms")
            .map_or(Ok(200), |v| v.parse())
            .map_err(|_| (64, "--latency-ms".to_owned()))?,
    };
    let emit = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&a["--emit"])
        .map_err(|e| (73, format!("{}: {e}", a["--emit"])))?;
    let mut emit = LineWriter::new(emit);
    media::init().map_err(with(69))?;

    let schedule = Arc::new(Mutex::new(Schedule::new(w, h)));
    let pushed = Arc::new(Mutex::new(0u64));
    let server = tiny_http::Server::http(a["--listen"].as_str()).map_err(|e| (69, format!("listen: {e}")))?;
    {
        let (s, p) = (Arc::clone(&schedule), Arc::clone(&pushed));
        std::thread::spawn(move || controls(server, s, p));
    }
    println!(
        "fake-camera: {w}x{h} {pattern:?} at {} fps to {}:{}; controls on {}",
        camera::FPS,
        target.host,
        target.port,
        a["--listen"]
    );

    let start = Instant::now();
    let due = |k: u64| start + Duration::from_nanos(k * FRAME_NS);
    let mut k: u64 = 0;
    let mut builds = 0u64;
    loop {
        let size = locked(&schedule).size();
        let mut cam = match CameraPipeline::start(&CameraSink::Srt(target.clone()), size.0, size.1, pattern) {
            Ok(c) => c,
            Err(e) => {
                println!("fake-camera: build failed: {}", scrub.scrub(&e));
                std::thread::sleep(REBUILD_AFTER);
                continue;
            }
        };
        builds += 1;
        // A new build starts at the counter due now: the frames due while there was none are gone.
        k = k.max(start.elapsed().as_nanos() as u64 / FRAME_NS);
        let failure = loop {
            if let Some(wait) = due(k).checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let counter = u32::try_from(k + 1).map_err(|_| (70, "the counter overflowed".to_owned()))?;
            k += 1;
            let (out, want) = {
                let mut s = locked(&schedule);
                (s.on_frame(counter, now_ms()), s.size())
            };
            if want != cam.size() {
                if let Err(e) = cam.set_size(want.0, want.1) {
                    break e;
                }
            }
            let mut failed = None;
            for c in out {
                if let Err(e) = cam.push(c) {
                    failed = Some(e);
                    break;
                }
                let line = json!({"counter": c, "wall_ms": now_ms()});
                writeln!(emit, "{line}").map_err(|e| (74, format!("emit: {e}")))?;
                let mut n = locked(&pushed);
                *n += 1;
                if (*n).is_multiple_of(REPORT_EVERY_FRAMES) {
                    println!("fake-camera: {} frames pushed (build {builds})", *n);
                }
            }
            if let Some(e) = failed.or_else(|| cam.failure()) {
                break e;
            }
        };
        println!("fake-camera: build {builds} ended: {}", scrub.scrub(&failure));
        drop(cam);
        std::thread::sleep(REBUILD_AFTER);
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, e)) => {
            let mut s = Scrubber::default();
            if let Ok(x) = std::env::var("HARNESS_SRT_PASSPHRASE") {
                s.add(&x);
            }
            println!("fake-camera: {}", s.scrub(&e));
            ExitCode::from(code)
        }
    }
}
