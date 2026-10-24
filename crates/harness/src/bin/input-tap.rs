#![allow(clippy::print_stdout)] // harness tools report on stdout by design (C13)
//! input-tap (Owed 81's probe): a second reader of MediaMTX's camera path, built as the compositor's
//! input is (`adapters::gst_input`: `srtsrc` with the same latency → `tsdemux` → a leaky 1 s queue →
//! `h264parse` → `avdec_h264` with `output-corrupt` off → `videoconvertscale` into I420 at the
//! programme's size → an appsink that never syncs, keeps two samples and drops the oldest). It writes
//! one line per decoded frame, the moment the appsink gets it: `{"build","counter","pts_ns",
//! "wall_us"}` (`counter` is the camera strip, `null` when it does not read). The relay image reports
//! no per-frame arrival, so this is the closest outside look at when each camera frame reaches the
//! compositor's camera slot.
//!
//! ```text
//! input-tap --out /out/tap.jsonl --srt-host mediamtx --srt-port 8890 --stream-id read:cam \
//!   [--latency-ms 1000] [--size 1280x720]
//! ```
//!
//! The SRT passphrase is `HARNESS_SRT_PASSPHRASE` (environment, never argv). A build that fails or
//! ends (no stream on the path yet, a kicked reader) is logged and rebuilt a second later, forever:
//! the stack's teardown stops it. A build that decodes nothing for 5 s says so once per silence,
//! with the H264 buffers that reached the parser, so a run tells "no H264 arrived" from "H264
//! arrived and did not decode" (`harness::silence`; the PR #59 review's m-4).
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{LineWriter, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adapters::redact::Scrubber;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use harness::clock::{now_ms, now_us};
use harness::silence::Silence;
use harness::strip::{self, Plane};
use serde_json::json;

const FLAGS: [&str; 7] = [
    "--out",
    "--srt-host",
    "--srt-port",
    "--stream-id",
    "--latency-ms",
    "--size",
    "--report-every",
];
const REQUIRED: [&str; 4] = ["--out", "--srt-host", "--srt-port", "--stream-id"];
/// The compositor's input latency (its log: `gen=1: srt://mediamtx:8890 latency=1000`).
const DEFAULT_LATENCY_MS: u32 = 1_000;
/// adapters::gst_input's queue and appsink bounds, restated (the harness does not link its
/// private constants): the leaky queue holds 1 s, the appsink two samples.
const VIDEO_QUEUE_NS: u64 = 1_000_000_000;
const APPSINK_MAX_BUFFERS: u32 = 2;
/// How long a failed or ended build waits before the next.
const REBUILD_AFTER: Duration = Duration::from_secs(1);
/// How often the tap prints its count (frames).
const DEFAULT_REPORT_EVERY: u64 = 300;
/// How long a build decodes nothing before it says so.
const SILENT_AFTER_MS: i64 = 5_000;

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

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|e| format!("no {factory} element: {e}"))
}

struct Tap {
    out: Mutex<LineWriter<std::fs::File>>,
    frames: AtomicU64,
    report_every: u64,
}

/// The camera strip of a decoded sample (`None`: no strip, or no readable picture).
fn counter_of(sample: &gst::Sample) -> Option<u32> {
    let buffer = sample.buffer()?;
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let map = buffer.map_readable().ok()?;
    let start = info.offset()[0];
    let stride = usize::try_from(info.stride()[0]).ok()?;
    let rows = stride * info.height() as usize;
    strip::read(&Plane {
        data: map.as_slice().get(start..start + rows)?,
        stride,
        width: info.width(),
        height: info.height(),
    })
}

struct Target {
    host: String,
    port: u16,
    stream_id: String,
    passphrase: String,
    latency_ms: u32,
    size: (u32, u32),
}

/// One build: plays until an error or EOS, then returns why (scrubbed by the caller).
fn run_build(t: &Target, tap: &Arc<Tap>, build: u64) -> Result<String, String> {
    let pipeline = gst::Pipeline::new();
    let src = make("srtsrc")?;
    src.set_property("uri", format!("srt://{}:{}?mode=caller", t.host, t.port));
    src.set_property("streamid", &t.stream_id);
    if !t.passphrase.is_empty() {
        src.set_property("passphrase", &t.passphrase);
    }
    src.set_property("latency", t.latency_ms as i32);
    src.set_property("auto-reconnect", false);
    let demux = make("tsdemux")?;
    let vqueue = make("queue")?;
    vqueue.set_property_from_str("leaky", "downstream");
    vqueue.set_property("max-size-time", VIDEO_QUEUE_NS);
    let vparse = make("h264parse")?;
    let vdec = make("avdec_h264")?;
    vdec.set_property("output-corrupt", false);
    let scale = make("videoconvertscale")?;
    scale.set_property("add-borders", true);
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "I420")
        .field("width", t.size.0 as i32)
        .field("height", t.size.1 as i32)
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .field("colorimetry", "bt709")
        .build();
    let vcaps = make("capsfilter")?;
    vcaps.set_property("caps", &caps);
    let vsink = gst_app::AppSink::builder().build();
    vsink.set_max_buffers(APPSINK_MAX_BUFFERS);
    vsink.set_drop(true);
    vsink.set_sync(false);
    vsink.set_property("async", false);
    let asink = make("fakesink")?;
    asink.set_property("sync", false);
    asink.set_property("async", false);

    pipeline
        .add_many([
            &src,
            &demux,
            &vqueue,
            &vparse,
            &vdec,
            &scale,
            &vcaps,
            vsink.upcast_ref(),
            &asink,
        ])
        .map_err(|e| e.to_string())?;
    src.link(&demux).map_err(|e| format!("src to demux: {e}"))?;
    gst::Element::link_many([&vqueue, &vparse, &vdec, &scale, &vcaps, vsink.upcast_ref()])
        .map_err(|e| format!("video branch: {e}"))?;
    {
        let (vq, asink) = (vqueue.clone(), asink.clone());
        demux.connect_pad_added(move |_, pad| {
            let name = pad
                .current_caps()
                .or_else(|| pad.allowed_caps())
                .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
                .unwrap_or_default();
            let target = match name.as_str() {
                "video/x-h264" => vq.static_pad("sink"),
                // The audio is read by no one here; a linked sink keeps tsdemux's flow combined.
                "audio/mpeg" => asink.static_pad("sink"),
                _ => None,
            };
            match target.filter(|p| !p.is_linked()).map(|p| pad.link(&p)) {
                Some(Ok(_)) => {}
                Some(Err(e)) => println!("input-tap: a {name} stream did not link ({e:?})"),
                None => println!("input-tap: a {name} stream is not used"),
            }
        });
    }
    // What reached the parser (H264 from tsdemux), and when the last frame came out of the decoder.
    let h264 = Arc::new(AtomicU64::new(0));
    let decoded = Arc::new(AtomicU64::new(0));
    let silence = Arc::new(Mutex::new(Silence::new(SILENT_AFTER_MS, now_ms())));
    {
        let h264 = Arc::clone(&h264);
        vparse
            .static_pad("sink")
            .ok_or("h264parse has no sink pad")?
            .add_probe(
                gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
                move |_, info| {
                    let n = match &info.data {
                        Some(gst::PadProbeData::BufferList(l)) => l.len() as u64,
                        _ => 1,
                    };
                    h264.fetch_add(n, Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                },
            );
    }
    let tap2 = Arc::clone(tap);
    let (decoded2, silence2) = (Arc::clone(&decoded), Arc::clone(&silence));
    vsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let at = now_us();
                decoded2.fetch_add(1, Ordering::Relaxed);
                locked(&silence2).frame(at / 1_000);
                let counter = counter_of(&sample);
                let pts_ns = sample.buffer().and_then(|b| b.pts()).map(|p| p.nseconds());
                let line = json!({"build": build, "counter": counter, "pts_ns": pts_ns, "wall_us": at});
                let _ = writeln!(locked(&tap2.out), "{line}");
                let n = tap2.frames.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(tap2.report_every) {
                    println!("input-tap: {n} frames (build {build})");
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("cannot play: {e}"))?;
    let bus = pipeline.bus().ok_or("no bus")?;
    let why = loop {
        let Some(msg) = bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(1),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) else {
            if let Some(ms) = locked(&silence).check(now_ms()) {
                println!(
                    "input-tap: build {build}: no video frame in {} s ({} H264 buffers reached the \
                     parser, {} frames decoded in this build)",
                    ms / 1_000,
                    h264.load(Ordering::Relaxed),
                    decoded.load(Ordering::Relaxed)
                );
            }
            continue;
        };
        break match msg.view() {
            gst::MessageView::Error(e) => format!(
                "{}: {}",
                e.src().map(|s| s.name().to_string()).unwrap_or_default(),
                e.error()
            ),
            _ => "end of stream".to_owned(),
        };
    };
    let _ = pipeline.set_state(gst::State::Null);
    Ok(format!(
        "{why} ({} H264 buffers reached the parser, {} frames decoded)",
        h264.load(Ordering::Relaxed),
        decoded.load(Ordering::Relaxed)
    ))
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
    let size = {
        let s = a.get("--size").map_or("1280x720", String::as_str);
        let (w, h) = s.split_once('x').ok_or((64, "--size is WxH".to_owned()))?;
        (
            w.parse().map_err(|_| (64, "--size: bad width".to_owned()))?,
            h.parse().map_err(|_| (64, "--size: bad height".to_owned()))?,
        )
    };
    let target = Target {
        host: a["--srt-host"].clone(),
        port: a["--srt-port"]
            .parse()
            .map_err(|_| (64, "--srt-port: not a port".to_owned()))?,
        stream_id: a["--stream-id"].clone(),
        passphrase,
        latency_ms: a
            .get("--latency-ms")
            .map_or(Ok(DEFAULT_LATENCY_MS), |v| v.parse())
            .map_err(|_| (64, "--latency-ms".to_owned()))?,
        size,
    };
    let report_every = a
        .get("--report-every")
        .map_or(Ok(DEFAULT_REPORT_EVERY), |v| v.parse())
        .map_err(|_| (64, "--report-every".to_owned()))?
        .max(1);
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&a["--out"])
        .map_err(|e| (73, format!("{}: {e}", a["--out"])))?;
    let tap = Arc::new(Tap {
        out: Mutex::new(LineWriter::new(out)),
        frames: AtomicU64::new(0),
        report_every,
    });
    gst::init().map_err(|e| (69, format!("gstreamer: {e}")))?;
    println!(
        "input-tap: {}:{} latency {} ms, {}x{}",
        target.host, target.port, target.latency_ms, target.size.0, target.size.1
    );
    let mut build = 0u64;
    loop {
        build += 1;
        let why = run_build(&target, &tap, build).unwrap_or_else(|e| format!("build failed: {e}"));
        println!("input-tap: build {build} ended: {}", scrub.scrub(&why));
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
            println!("input-tap: {}", s.scrub(&e));
            ExitCode::from(code)
        }
    }
}
