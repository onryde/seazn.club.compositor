//! Owed 100 (`_FINDINGS.md` F-031): input rebuilds do not grow the process's heap. The soak's camera
//! dropouts grew the compositor's anon memory by +5.6 MB each (run 40166664556); one dropout is one
//! input rebuild. Each build ran on new threads (its build thread and its streaming tasks), glibc gave
//! them other arenas, and each arena kept the frames a build had freed: +1.2 to +1.5 MB a rebuild in
//! CI's reproduction, with no object leaked. The input's threads now outlive the builds
//! (`input_threads`), and the next build reuses what the last one freed.
//!
//! One test, three roles by `INPUT_HEAP_ROLE`. The parent spawns a listener child (a 720p30 camera
//! with audio over SRT, as fake-camera is to MediaMTX) and a measuring child, so the measured process
//! holds the adapter alone: the listener's encoder and its SRT sockets are another process's heap.
//! The measuring child drives rebuilds as the domain drives a dropout (connect, decode, `abandon`,
//! and the next `rebuild` tears it down) and prints one sample a rebuild: the process's anon memory
//! (`/proc/self/status` `RssAnon`), split as `/proc/self/smaps` shows it into the main heap, glibc's
//! thread arenas and the rest, and its threads. The parent judges the growth per rebuild.
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use adapters::frame_store::FrameStore;
use adapters::gst_input::{GstInput, StartHold};
use app::ports::{Inbound, InputSource, InputSpec};
use crossbeam_channel::Receiver;
use domain::config::{OutputProfile, Thresholds};
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;

const TEST: &str = "input_rebuilds_do_not_grow_the_heap";
const ROLE: &str = "INPUT_HEAP_ROLE";
const PORT: &str = "INPUT_HEAP_PORT";
/// Rebuilds before the growth is read: the first builds load plugins, fill GStreamer's caches, start
/// the input's threads and give glibc its arenas.
const WARMUP: usize = 8;
/// Rebuilds the growth is read over.
const MEASURED: usize = 32;
/// How long each build decodes before the domain gives it up.
const DECODE_MS: u64 = 1_000;
/// The listener's frame rate (its pipeline below).
const SOURCE_FPS: u64 = 30;
/// glibc's `HEAP_MAX_SIZE` on 64-bit (malloc/arena.c: 2 × `DEFAULT_MMAP_THRESHOLD_MAX`, 2 × 32 MiB):
/// every thread arena's heap is a mapping of this size at an address aligned to it.
const ARENA_HEAP: u64 = 64 << 20;

/// The bound on the growth per rebuild: 512 KiB, between this harness's own two readings (F-031).
/// The cause (each build on new threads: the fix's halves removed) read 1 592 to 2 447 KiB a rebuild
/// in 7 runs, more than one P720 frame (I420, 1280 × 720 × 3 / 2 = 1 382 400 B, 1 350 KiB); the fix
/// read 6 to 119 KiB in 12 (CI runs 40191976912 and 40194261349). 512 KiB is the power of two nearest their geometric
/// middle (434 KiB), so each side clears it by about the same factor: the fix by 4.3, the cause by
/// 3.1. One shared pool for every streaming task read 2 to 620 KiB in 24 runs (its arenas settle in
/// steps of 6 to 10 MB), which is why the pools are one a role; that split has its own guard in
/// `tests/input.rs`. The soak brackets the same figure: 0.14 MB of anon a dropout with glibc's arenas
/// capped, 2.8 MB with the default (arms 2 and 1).
const BOUND_KIB: f64 = 512.0;

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .expect("a free udp port")
}

// ---------------------------------------------------------------------------------------------
// The listener child

fn listener_child() {
    let port: u16 = var(PORT).parse().expect("INPUT_HEAP_PORT");
    gst::init().expect("gstreamer");
    // fake-camera's picture: 1280x720 ball at 30 fps with audio, ts alignment 7 (F-012).
    let desc = format!(
        "videotestsrc is-live=true pattern=ball \
         ! video/x-raw,format=I420,width=1280,height=720,framerate={SOURCE_FPS}/1 \
         ! x264enc tune=zerolatency speed-preset=ultrafast key-int-max=30 ! h264parse config-interval=-1 \
         ! mpegtsmux name=m alignment=7 \
         ! srtsink uri=srt://127.0.0.1:{port}?mode=listener wait-for-connection=false \
         audiotestsrc is-live=true wave=sine freq=440 volume=0.5 ! audio/x-raw,rate=48000,channels=2 \
         ! audioconvert ! voaacenc ! aacparse ! m."
    );
    let pipeline = gst::parse::launch(&desc).expect("listener pipeline");
    pipeline.set_state(gst::State::Playing).expect("listener plays");
    eprintln!("heap listener: ready on {port}");
    // Until the parent closes this child's stdin, or dies.
    let mut line = String::new();
    while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
        line.clear();
    }
    let _ = pipeline.set_state(gst::State::Null);
}

// ---------------------------------------------------------------------------------------------
// The measuring child

fn status_kb(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

struct Mapping {
    start: u64,
    end: u64,
    perms: String,
    path: String,
    anon_kb: u64,
}

fn mappings() -> Vec<Mapping> {
    let text = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
    let mut maps: Vec<Mapping> = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let first = it.next().unwrap_or("");
        if let Some((a, b)) = first.split_once('-') {
            if let (Ok(start), Ok(end)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) {
                let perms = it.next().unwrap_or("").to_owned();
                let path = it.nth(3).unwrap_or("").to_owned();
                maps.push(Mapping {
                    start,
                    end,
                    perms,
                    path,
                    anon_kb: 0,
                });
                continue;
            }
        }
        if let Some(rest) = line.strip_prefix("Anonymous:") {
            if let Some(m) = maps.last_mut() {
                m.anon_kb = rest
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    maps
}

/// (main arena `[heap]`, thread arenas, their count, every other anon mapping), in kB of anon pages.
fn split_anon() -> (u64, u64, u64, u64) {
    let maps = mappings();
    let (mut heap, mut arena, mut arenas, mut other) = (0, 0, 0, 0);
    for (i, m) in maps.iter().enumerate() {
        if m.path == "[heap]" {
            heap += m.anon_kb;
            continue;
        }
        if !m.path.is_empty() && !m.path.starts_with("[anon") {
            continue;
        }
        let guard = maps
            .get(i + 1)
            .filter(|n| n.start == m.end && n.perms.starts_with("---") && n.path.is_empty())
            .map_or(0, |n| n.end - n.start);
        let is_arena =
            m.perms.starts_with("rw") && m.start % ARENA_HEAP == 0 && m.end - m.start + guard == ARENA_HEAP;
        if is_arena {
            arena += m.anon_kb;
            arenas += 1;
        } else {
            other += m.anon_kb;
        }
    }
    (heap, arena, arenas, other)
}

fn srt_connected(src: &gst::Element) -> bool {
    src.property::<Option<gst::Structure>>("stats")
        .is_some_and(|s| s.has_field("rtt-ms"))
}

/// input.rs's harness guard: the start is let go only once the caller's SRT handshake has
/// completed (libsrt 1.5.3's postConnect gap, Owed 97).
fn wait_held_past_the_handshake(input: &GstInput, hold: &StartHold, within: Duration) -> Result<(), String> {
    let t0 = Instant::now();
    loop {
        let held = hold.waiting() > 0;
        let connected = input
            .pipeline()
            .and_then(|p| p.by_name("src"))
            .is_some_and(|src| srt_connected(&src));
        if held && connected {
            return Ok(());
        }
        if t0.elapsed() >= within {
            return Err(format!("held {held}, connected {connected} after {within:?}"));
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(rx: &Receiver<Inbound>, within: Duration, pred: impl Fn(&Event) -> bool) -> bool {
    let end = Instant::now() + within;
    while let Ok(m) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
        if let Inbound::Event(e) = m {
            if pred(&e) {
                return true;
            }
        }
    }
    false
}

fn frames_for(rx: &Receiver<Inbound>, window: Duration, gen: u64) -> usize {
    let end = Instant::now() + window;
    let mut n = 0;
    while let Ok(m) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
        if matches!(m, Inbound::Event(Event::InputFrame { gen: g }) if g == gen) {
            n += 1;
        }
    }
    n
}

fn wait_teardowns(input: &GstInput, within: Duration) -> bool {
    let end = Instant::now() + within;
    while input.teardowns_in_flight() > 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(10));
    }
    input.teardowns_in_flight() == 0
}

fn measure_child() {
    let port: u16 = var(PORT).parse().expect("INPUT_HEAP_PORT");
    let t = Thresholds::default();
    let out = OutputProfile::P720;
    let (tx, rx) = crossbeam_channel::bounded(8_192);
    let frames = FrameStore::default();
    let mut input = GstInput::new(frames.clone(), tx, &t, out.width, out.height)
        .with_log(Arc::new(|l: &str| eprintln!("heap adapter: {l}")));
    let spec = InputSpec {
        url: format!("srt://127.0.0.1:{port}"),
        stream_id: String::new(),
        passphrase: String::new(),
    };
    let budget = Duration::from_millis(t.input_connect_timeout_ms);
    let cycles = (WARMUP + MEASURED) as u64;
    for gen in 1..=cycles {
        let t0 = Instant::now();
        let hold = input.hold_starts();
        input.rebuild(gen, &spec);
        let past = wait_held_past_the_handshake(&input, &hold, budget);
        hold.release();
        if let Err(why) = &past {
            eprintln!("heap child: gen {gen}: {why}");
        }
        let connected = wait_for(&rx, budget, |e| *e == Event::InputConnected { gen });
        let n = frames_for(&rx, Duration::from_millis(DECODE_MS), gen);
        // The domain gives the build up; the next rebuild tears it down, and this one's sample is
        // taken once the build before it is torn down.
        input.abandon(gen);
        let torn = wait_teardowns(&input, Duration::from_secs(10));
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let (heap_kb, arena_kb, arenas, other_kb) = split_anon();
        eprintln!(
            "heap cycle={gen} ok={} frames={n} torn={} took_ms={} anon_kb={} threads={} heap_kb={heap_kb} \
             arena_kb={arena_kb} arenas={arenas} other_anon_kb={other_kb}",
            u8::from(past.is_ok() && connected),
            u8::from(torn),
            t0.elapsed().as_millis(),
            status_kb(&status, "RssAnon:"),
            status_kb(&status, "Threads:"),
        );
    }
    input.stop();
    let torn = wait_teardowns(&input, Duration::from_secs(10));
    drop(input);
    drop(frames);
    eprintln!("heap done cycles={cycles} torn={}", u8::from(torn));
}

// ---------------------------------------------------------------------------------------------
// The parent

fn field(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        .and_then(|v| v.parse().ok())
}

/// The median of the slopes between every two points of `ys` (Theil–Sen), against 0, 1, 2…: the
/// typical growth from one rebuild to the next. glibc's heap grows in steps (an arena takes a new
/// region, then holds still), and one step is not a cost every rebuild pays: it moves the median by
/// little, where it tilts a least-squares line by the step over the run's length.
fn median_slope(ys: &[f64]) -> f64 {
    let mut slopes = Vec::new();
    for (i, a) in ys.iter().enumerate() {
        for (j, b) in ys.iter().enumerate().skip(i + 1) {
            slopes.push((b - a) / (j - i) as f64);
        }
    }
    slopes.sort_by(f64::total_cmp);
    match slopes.len() {
        0 => f64::NAN,
        n if n % 2 == 1 => slopes[n / 2],
        n => (slopes[n / 2 - 1] + slopes[n / 2]) / 2.0,
    }
}

/// Least-squares slope of `ys` against 0, 1, 2…, printed beside the median for the record.
fn line_slope(ys: &[f64]) -> f64 {
    let n = ys.len() as f64;
    let mx = (n - 1.0) / 2.0;
    let my = ys.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (i, y) in ys.iter().enumerate() {
        let dx = i as f64 - mx;
        num += dx * (y - my);
        den += dx * dx;
    }
    num / den
}

/// The estimator itself, on series whose answer is known by construction: a steady growth reads as
/// itself; one step in a heap that otherwise holds still reads as nothing (and tilts a least-squares
/// line past the bound); a steady growth with an alternating wobble reads as the growth.
#[test]
fn the_median_slope_reads_a_steady_growth_and_ignores_one_step() {
    let steady: Vec<f64> = (0..24).map(|i| 1_000.0 + 1_200.0 * f64::from(i)).collect();
    assert_eq!(median_slope(&steady), 1_200.0);
    let one_step: Vec<f64> = (0..24)
        .map(|i| if i < 16 { 40_000.0 } else { 47_000.0 })
        .collect();
    assert_eq!(median_slope(&one_step), 0.0);
    // Σ (x − 11.5) over x = 16..23 is 64, and Σ (x − 11.5)² over 0..23 is 1 150.
    let tilted = line_slope(&one_step);
    assert!(
        (tilted - 7_000.0 * 64.0 / 1_150.0).abs() < 1e-6,
        "one step tilts a least-squares line: {tilted}"
    );
    let wobble: Vec<f64> = (0..24)
        .map(|i| 1_000.0 * f64::from(i) + f64::from(i % 2) * 300.0)
        .collect();
    let s = median_slope(&wobble);
    assert!((1_000.0..=1_300.0).contains(&s), "{s}");
    assert!(median_slope(&[]).is_nan(), "no points, no slope");
    eprintln!("checked 4 series");
}

#[test]
fn input_rebuilds_do_not_grow_the_heap() {
    match var(ROLE).as_str() {
        "listener" => return listener_child(),
        "measure" => return measure_child(),
        _ => {}
    }
    let exe = std::env::current_exe().expect("test binary");
    let port = free_port();
    let mut listener = Command::new(&exe)
        .args(["--exact", TEST, "--test-threads=1", "--nocapture"])
        .env(ROLE, "listener")
        .env(PORT, port.to_string())
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("listener child");
    let stderr = listener.stderr.take().expect("listener stderr");
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("heap listener: ready") {
                let _ = ready_tx.send(());
            }
            eprintln!("listener| {line}");
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the listener child is ready");

    let mut child = Command::new(&exe)
        .args(["--exact", TEST, "--test-threads=1", "--nocapture"])
        .env(ROLE, "measure")
        .env(PORT, port.to_string())
        .stderr(Stdio::piped())
        .stdout(Stdio::inherit())
        .spawn()
        .expect("measuring child");
    let stderr = child.stderr.take().expect("child stderr");
    let mut cycles: Vec<String> = Vec::new();
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        eprintln!("child| {line}");
        if line.starts_with("heap cycle=") {
            cycles.push(line);
        }
    }
    let status = child.wait().expect("child exit");
    drop(listener.stdin.take());
    let _ = listener.wait();

    let min_frames = (DECODE_MS * SOURCE_FPS / 1_000 / 2) as f64;
    let short: Vec<&String> = cycles
        .iter()
        .filter(|l| field(l, "ok") != Some(1.0) || field(l, "frames").is_none_or(|n| n < min_frames))
        .collect();
    let measured: Vec<&String> = cycles.iter().skip(WARMUP).collect();
    let series = |key: &str| -> Vec<f64> { measured.iter().filter_map(|l| field(l, key)).collect() };
    let anon = series("anon_kb");
    let growth = median_slope(&anon);
    eprintln!(
        "HEAP SUMMARY exit={:?} cycles={} measured={} anon_median_slope_kib={growth:.1} \
         anon_line_slope_kib={:.1} bound_kib={:.1} anon_first_kib={:?} anon_last_kib={:?} \
         arena_median_slope_kib={:.1} heap_median_slope_kib={:.1} arenas_last={:?} threads_first={:?} \
         threads_last={:?}",
        status.code(),
        cycles.len(),
        anon.len(),
        line_slope(&anon),
        BOUND_KIB,
        anon.first(),
        anon.last(),
        median_slope(&series("arena_kb")),
        median_slope(&series("heap_kb")),
        series("arenas").last(),
        series("threads").first(),
        series("threads").last(),
    );
    let _ = std::io::stderr().flush();
    assert!(status.success(), "the measuring child failed: {status:?}");
    assert_eq!(cycles.len(), WARMUP + MEASURED, "every rebuild was sampled");
    assert!(
        short.is_empty(),
        "every build connected and decoded at least {min_frames} frames: {short:?}"
    );
    assert_eq!(anon.len(), MEASURED, "every measured sample read RssAnon");
    assert!(anon.iter().all(|&kb| kb > 0.0), "RssAnon read as 0: {anon:?}");
    eprintln!("checked {} rebuilds after {WARMUP} to warm up", anon.len());
    assert!(
        growth <= BOUND_KIB,
        "the heap grows {growth:.1} KiB a rebuild, over the bound of {:.1} KiB",
        BOUND_KIB
    );
}
