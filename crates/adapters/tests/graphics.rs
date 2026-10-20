//! C11: the overlay page in CEF, against the real gstcefsrc (F-001's build). These tests need cefsrc
//! and an X display, so they run in the C12 image (or the C00 spike image with a Rust toolchain)
//! only: `Xvfb :99 & DISPLAY=:99 cargo test -p adapters --features cef --test graphics --
//! --test-threads=1`. CEF is initialised once per process and outlives every adapter, so the tests
//! take `serial()` and run one at a time; the two that need a CEF of their own (a real GST_DEBUG, a
//! killed zygote) run again in a child process.
//!
//! Expected values come from the page (`harness/pages/overlay-test.html`, its CSS and its lines),
//! the brief, `Thresholds::default()` and the findings (F-001, F-013), never from the adapter.
//!
//! **Waits** (PR #32 review m5): a test waits for an event, a line or a count, never a fixed sleep
//! it then trusts. Each wait's bound is the domain's own: a ready within `cef_load_timeout_ms` (a
//! slower page is restarted, a cold CEF start included), and anything a live page does continually
//! (a paint, a beat, a crash's report) within `cef_hang_after_ms`. A bound only ends a wait that would
//! otherwise hang; none is the claim under test, except the brief's 500 ms for the drop.
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use adapters::cef_graphics::{
    cef_children, CefGraphics, ReaperPath, CEF_NICE, CHILD_POLL_MS, REAPER_KILL_AFTER_MS, SPARE_RENDERER_OFF,
    TEARDOWN_LEAK_AFTER_MS,
};
use adapters::frame_store::FrameStore;
use adapters::gst_programme::GstProgramme;
use app::ports::{GraphicsRenderer, Inbound, ProgrammeOutput, TargetSpec};
use crossbeam_channel::Receiver;
use domain::cef_health::CefHealth;
use domain::config::{OutputProfile, Thresholds, CHROMIUM_GPU_DEATHS_ABORT};
use domain::health::PhaseChannel;
use domain::programme::{FrameChoice, SlateKind};
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

/// The page's test square: `#0000ff`, 100 × 100 at the top left, opaque. BGRA in memory.
const BLUE_BGRA: [u8; 4] = [255, 0, 0, 255];
/// The page's `#white50`: `rgba(255, 255, 255, 0.5)` at (100, 300), 200 × 100. Straight alpha in the
/// slot (the adapter's job: cefsrc's own reads (128, 128, 128, 128), F-001). 0.5 × 255 rounds to 128.
const WHITE50_STRAIGHT: [u8; 4] = [255, 255, 255, 128];
/// Chromium's History backend commits every 10 s (`kCommitIntervalSeconds`): the token reaches
/// History's file no sooner (the review's P1 waited 12 s).
const HISTORY_COMMIT: Duration = Duration::from_secs(10);

/// The page beats every `cef_alive_every_ms` after ready (R2005; the page's own setInterval).
fn beat() -> Duration {
    Duration::from_millis(Thresholds::default().cef_alive_every_ms)
}

/// The domain's load timeout: no test waits longer for a ready.
fn load_timeout() -> Duration {
    Duration::from_millis(Thresholds::default().cef_load_timeout_ms)
}

/// The domain's hang bound: the wait for anything a live page does continually.
fn hang_bound() -> Duration {
    Duration::from_millis(Thresholds::default().cef_hang_after_ms)
}

// ---------------------------------------------------------------------------------------------
// Harness

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn pages_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/pages")
        .canonicalize()
        .expect("the harness pages")
}

fn page(query: &str) -> String {
    format!("file://{}/overlay-test.html{query}", pages_dir().display())
}

type Lines = Arc<Mutex<Vec<String>>>;

struct Rig {
    g: CefGraphics,
    rx: Receiver<Inbound>,
    frames: FrameStore,
    lines: Lines,
}

fn rig_with(frames: FrameStore, cap_mb: u64) -> Rig {
    gst::init().expect("gstreamer");
    let (tx, rx) = crossbeam_channel::bounded(8_192);
    let lines: Lines = Arc::default();
    let sink = Arc::clone(&lines);
    // A child test process prints the adapter's lines too: they join the stderr its parent reads.
    let echo = std::env::var_os("C11_CHILD").is_some();
    let g = CefGraphics::new(frames.clone(), tx, 30, cap_mb).with_log(Arc::new(move |l: &str| {
        if echo {
            eprintln!("adapter: {l}");
        }
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(l.to_owned());
    }));
    Rig { g, rx, frames, lines }
}

fn rig() -> Rig {
    rig_with(FrameStore::default(), Thresholds::default().cef_rss_cap_mb)
}

fn lines(r: &Rig) -> Vec<String> {
    r.lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// True once a line of the adapter's log contains every one of `needles` (within `timeout`).
fn wait_line(r: &Rig, timeout: Duration, needles: &[&str]) -> bool {
    wait_for(timeout, || {
        lines(r).iter().any(|l| needles.iter().all(|n| l.contains(n)))
    })
}

/// Every event until `n` of them match `pred` (the last included), or panics after `timeout`.
fn events_until_n(
    rx: &Receiver<Inbound>,
    timeout: Duration,
    n: usize,
    pred: impl Fn(&Event) -> bool,
) -> Vec<Event> {
    let end = Instant::now() + timeout;
    let mut seen = Vec::new();
    let mut hits = 0;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Inbound::Event(e)) => {
                if pred(&e) {
                    hits += 1;
                }
                seen.push(e);
                if hits >= n {
                    return seen;
                }
            }
            Ok(Inbound::Session(_)) => {}
            Err(_) => panic!("{hits} of {n} matching events within {timeout:?}; saw {seen:?}"),
        }
    }
}

fn events_until(rx: &Receiver<Inbound>, timeout: Duration, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    events_until_n(rx, timeout, 1, pred)
}

/// Every event within `window`, each with when it arrived.
fn events_for(rx: &Receiver<Inbound>, window: Duration) -> Vec<(Instant, Event)> {
    let end = Instant::now() + window;
    let mut seen = Vec::new();
    while let Ok(m) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
        if let Inbound::Event(e) = m {
            seen.push((Instant::now(), e));
        }
    }
    seen
}

/// Every event already queued.
fn drain(rx: &Receiver<Inbound>) -> Vec<Event> {
    rx.try_iter()
        .filter_map(|m| match m {
            Inbound::Event(e) => Some(e),
            Inbound::Session(_) => None,
        })
        .collect()
}

fn ready(gen: u64) -> impl Fn(&Event) -> bool {
    move |e| *e == Event::CefReady { gen }
}

fn crashed(gen: u64) -> impl Fn(&Event) -> bool {
    move |e| *e == Event::CefCrashed { gen }
}

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// Two child polls after this call (renice, OOM order, claims and the cap are each a poll's work).
fn two_polls(g: &CefGraphics) -> bool {
    let p0 = g.polls();
    wait_for(hang_bound(), || g.polls() >= p0 + 2)
}

fn bytes(s: &gst::Sample) -> Vec<u8> {
    s.buffer()
        .expect("buffer")
        .map_readable()
        .expect("map")
        .as_slice()
        .to_vec()
}

fn bgra_at(s: &gst::Sample, x: u32, y: u32) -> [u8; 4] {
    let info = gst_video::VideoInfo::from_caps(s.caps().expect("caps")).expect("info");
    let stride = info.stride()[0] as usize;
    let map = s.buffer().expect("buffer").map_readable().expect("map");
    let o = y as usize * stride + x as usize * 4;
    [map[o], map[o + 1], map[o + 2], map[o + 3]]
}

fn close(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

fn alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // A zombie has exited; only its parent's wait is left.
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

/// The renderers among this process's CEF children, read here from `/proc`.
fn renderer_count() -> usize {
    cef_children().iter().filter(|c| c.kind == "renderer").count()
}

/// Runs test `name` again in a child process, with a HOME of its own (this process's CEF holds its
/// profile for good, and a second CEF on one HOME refuses to start, F-013), a TMPDIR inside it (a
/// child that dies leaves its private profile there, removed with the HOME) and `env`. The child
/// takes its own branch on `C11_CHILD`.
///
/// The path stays short: Chromium makes its sockets under TMPDIR, and a TMPDIR of 85 characters (the
/// test's name in it) killed the child by SIGTRAP before its first ready, with no FATAL line.
fn rerun_in_child(name: &str, env: &[(&str, &str)]) -> std::process::Output {
    static CHILDREN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = CHILDREN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("c11-child-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let tmp = home.join("tmp");
    std::fs::create_dir_all(&tmp).expect("child HOME");
    let mut all = vec![("TMPDIR", tmp.to_str().expect("utf-8"))];
    all.extend_from_slice(env);
    let out = run_child(name, &home, &all);
    let _ = std::fs::remove_dir_all(&home);
    out
}

/// `rerun_in_child` on a HOME the caller made (and reads after the child exits).
fn run_child(name: &str, home: &Path, env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary"));
    // The HOME is the working directory too: a core file of a child that aborts lands in it.
    cmd.args(["--exact", name, "--test-threads=1", "--nocapture"])
        .env("C11_CHILD", "1")
        .env("HOME", home)
        .current_dir(home);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("child")
}

/// Every file under `root` whose bytes hold `needle`, and how many files were read.
fn files_holding(root: &Path, needle: &[u8]) -> (Vec<PathBuf>, usize) {
    fn walk(dir: &Path, needle: &[u8], hits: &mut Vec<PathBuf>, files: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                walk(&p, needle, hits, files);
            } else if ft.is_file() {
                *files += 1;
                // An empty needle lists every file.
                if needle.is_empty()
                    || std::fs::read(&p).is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle))
                {
                    hits.push(p);
                }
            }
        }
    }
    let mut hits = Vec::new();
    let mut files = 0;
    walk(root, needle, &mut hits, &mut files);
    (hits, files)
}

// ---------------------------------------------------------------------------------------------
// The brief's tests (Step 1, amended)

#[test]
fn ready_signal_arrives_with_its_generation() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(
        wait_for(hang_bound(), || r.frames.overlay().is_some()),
        "no overlay sample after CefReady"
    );
    let s = r.frames.overlay().expect("overlay");
    let info = gst_video::VideoInfo::from_caps(s.caps().expect("caps")).expect("info");
    assert_eq!(
        (info.format(), info.width(), info.height()),
        (gst_video::VideoFormat::Bgra, 1_920, 1_080)
    );
    assert_eq!(bgra_at(&s, 50, 50), BLUE_BGRA, "the test square");
    assert_eq!(bgra_at(&s, 960, 540)[3], 0, "the transparent centre");
    let white = bgra_at(&s, 150, 350);
    assert!(close(white, WHITE50_STRAIGHT, 2), "straight alpha: {white:?}");
    // F-001's readiness mechanism, verbatim: the console line, js-signals off; F-009 (a): no flag
    // but F-013's spare renderer one.
    let src =
        r.g.pipeline()
            .expect("pipeline")
            .iterate_sources()
            .into_iter()
            .flatten()
            .next()
            .expect("cefsrc");
    assert!(!src.property::<bool>("listen-for-js-signals"));
    let flags = src
        .property::<Option<String>>("chrome-extra-flags")
        .unwrap_or_default();
    assert!(flags.starts_with(SPARE_RENDERER_OFF), "{flags}");
    r.g.stop();
}

#[test]
fn page_without_ready_signal_times_out() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page("?noready=1"));
    assert!(
        wait_for(load_timeout(), || r.g.samples(1).delivered > 0),
        "the page painted no frame at all"
    );
    // The page paints, and for the domain's whole load timeout after that it never signals.
    let seen = events_for(&r.rx, load_timeout());
    let samples = r.g.samples(1);
    eprintln!(
        "noready: {} samples delivered, events {seen:?}",
        samples.delivered
    );
    assert!(
        !seen.iter().any(|(_, e)| matches!(e, Event::CefReady { .. })),
        "the adapter faked ready: {seen:?}"
    );
    r.g.stop();
}

#[test]
fn frames_before_ready_never_reach_the_slot() {
    let _s = serial();
    let mut r = rig();
    // The first generation, on an empty slot: a second of its paints (30 fps).
    r.g.restart(1, &page("?noready=1"));
    assert!(
        wait_for(load_timeout(), || r.g.samples(1).delivered >= 30),
        "gen 1 painted too little: {:?}",
        r.g.samples(1)
    );
    let first = r.g.samples(1);
    assert!(
        r.frames.overlay().is_none(),
        "a frame of an unready page reached the slot"
    );
    // A previous generation's frame, held through an unready one.
    r.g.restart(2, &page(""));
    events_until(&r.rx, load_timeout(), ready(2));
    assert!(wait_for(hang_bound(), || r.frames.overlay().is_some()));
    r.g.restart(3, &page("?noready=1"));
    let held = bytes(&r.frames.overlay().expect("gen 2's frame"));
    assert!(
        wait_for(load_timeout(), || r.g.samples(3).delivered >= 30),
        "gen 3 painted too little: {:?}",
        r.g.samples(3)
    );
    let third = r.g.samples(3);
    eprintln!("unready samples: gen 1 {first:?}; gen 3 {third:?}");
    assert_eq!(third.written, 0);
    assert_eq!(
        bytes(&r.frames.overlay().expect("held")),
        held,
        "the held frame changed"
    );
    r.g.stop();
}

#[test]
fn restart_holds_the_last_overlay_frame() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(wait_for(hang_bound(), || r.frames.overlay().is_some()));
    // Gen 2 loads and paints, and is not ready (it never will be): the slot holds gen 1's frame.
    r.g.restart(2, &page("?noready=1"));
    let held = r.frames.overlay().expect("gen 1's last frame");
    assert!(
        wait_for(load_timeout(), || r.g.samples(2).delivered >= 3),
        "gen 2 produced no frame"
    );
    let now = r.frames.overlay().expect("still held");
    eprintln!(
        "gen 2 delivered {:?} while gen 1's frame was held",
        r.g.samples(2)
    );
    assert_eq!(
        now.buffer().expect("buffer").as_ptr(),
        held.buffer().expect("buffer").as_ptr(),
        "the slot no longer holds gen 1's last frame"
    );
    r.g.stop();
}

#[test]
fn crash_of_the_renderer_reports_cefcrashed() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(wait_for(hang_bound(), || !r.g.renderer_pids(1).is_empty()));
    let pids = r.g.renderer_pids(1);
    // F-013: no spare renderer, so the build has exactly one.
    assert_eq!(pids.len(), 1, "gen 1's renderers: {pids:?}");
    assert!(wait_for(hang_bound(), || r.frames.overlay().is_some()));
    let killed = Command::new("kill")
        .args(["-KILL", &pids[0].to_string()])
        .status()
        .expect("kill");
    assert!(killed.success());
    events_until(&r.rx, hang_bound(), crashed(1));
    let held = bytes(&r.frames.overlay().expect("held"));
    // cefsrc reloads the page, and the reloaded page signals ready again: the crashed build's gate
    // stays shut (the reload's frames never reach the slot).
    events_until(&r.rx, load_timeout(), ready(1));
    let at_reload = r.g.samples(1);
    assert!(
        wait_for(hang_bound(), || r.g.samples(1).dropped >= at_reload.dropped + 10),
        "the reloaded page painted nothing: {:?}",
        r.g.samples(1)
    );
    let later = r.g.samples(1);
    eprintln!(
        "killed renderer {}; after the reload: {at_reload:?} → {later:?}",
        pids[0]
    );
    assert_eq!(later.written, at_reload.written);
    assert_eq!(bytes(&r.frames.overlay().expect("held")), held);
    // A renderer's death is a crash, not CEF's end (the zygote test's negative pair).
    assert_eq!(r.g.health().unrecoverable, None);
    r.g.stop();
}

#[test]
fn rss_over_cap_reports_crash() {
    let _s = serial();
    let mut r = rig_with(FrameStore::default(), 1);
    r.g.restart(1, &page(""));
    // CEF's children exist once it starts (a load's work), and the cap is read every child poll.
    let bound = load_timeout() + Duration::from_millis(2 * CHILD_POLL_MS);
    events_until(&r.rx, bound, crashed(1));
    r.g.stop();
}

// PR #32 review G2: a retiring build's renderer is not the new build's memory. Gen 1's teardown is
// held (and the reaper's kill put off), so its renderer lives on beside gen 2's. A cap between gen
// 2's share and the whole sum crashes nothing; a cap below gen 2's share crashes gen 2.
#[test]
fn a_retiring_builds_renderer_does_not_count_against_the_new_builds_cap() {
    let _s = serial();
    let mut r = rig();
    r.g.hold_teardowns(true);
    r.g.set_reaper_kill_after(Duration::from_secs(60));
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    r.g.restart(2, &page(""));
    events_until(&r.rx, load_timeout(), ready(2));
    assert!(two_polls(&r.g), "the child poll stopped");
    let gen1 = r.g.renderer_pids(1);
    assert!(
        !gen1.is_empty() && gen1.iter().all(|p| alive(*p)),
        "gen 1's renderer ended: this run proves nothing ({gen1:?})"
    );
    let g = r.g.last_guard();
    let other = g.pss_mb - g.pss_counted_mb;
    eprintln!(
        "PSS {} MB in all, {} MB counted for gen 2, {other} MB not; gen 1 {gen1:?}, gen 2 {:?}",
        g.pss_mb,
        g.pss_counted_mb,
        r.g.renderer_pids(2)
    );
    // A renderer is tens of MB (F-013; the review's P6: 22-38 MB for a respawned one).
    assert!(other >= 10, "gen 1's renderer counted for gen 2: {g:?}");
    let mid = g.pss_counted_mb + other / 2;
    r.g.set_rss_cap_mb(mid);
    assert!(two_polls(&r.g), "the child poll stopped");
    let seen = drain(&r.rx);
    let after = r.g.last_guard();
    eprintln!("cap {mid} MB; later {after:?}; events {seen:?}");
    assert!(
        after.pss_mb > mid,
        "the whole sum fell under the cap: this run proves nothing ({after:?})"
    );
    assert!(
        !seen.iter().any(crashed(2)),
        "gen 2 crashed for gen 1's renderer: {seen:?}"
    );
    // The positive pair: under gen 2's own share.
    r.g.set_rss_cap_mb(after.pss_counted_mb / 2);
    events_until(&r.rx, hang_bound(), crashed(2));
    r.g.hold_teardowns(false);
    r.g.set_reaper_kill_after(Duration::from_millis(REAPER_KILL_AFTER_MS));
    r.g.stop();
}

#[test]
fn a_reported_crash_closes_its_generations_gate() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(wait_for(hang_bound(), || r.g.samples(1).written >= 5));
    r.g.set_rss_cap_mb(1);
    events_until(&r.rx, hang_bound(), crashed(1));
    let held = bytes(&r.frames.overlay().expect("the last frame"));
    let at_report = r.g.samples(1);
    assert!(
        wait_for(hang_bound(), || r.g.samples(1).dropped >= at_report.dropped + 10),
        "the page painted nothing after the report: {:?}",
        r.g.samples(1)
    );
    let later = r.g.samples(1);
    eprintln!("at the report {at_report:?}; later {later:?}");
    assert_eq!(
        later.written, at_report.written,
        "a sample reached the slot after the report"
    );
    assert_eq!(
        bytes(&r.frames.overlay().expect("held")),
        held,
        "the held frame changed"
    );
    r.g.stop();
}

#[test]
fn stale_generation_events_are_tagged_with_their_own_gen() {
    let _s = serial();
    let mut r = rig();
    // Gen 1's teardown is held (and its renderer left alone), so its page signals ready, stale and
    // its channel after it was retired.
    r.g.hold_teardowns(true);
    r.g.set_reaper_kill_after(Duration::from_secs(60));
    r.g.restart(1, &page("?ready_after_ms=1200&stale=1&channel=web"));
    r.g.restart(2, &page(""));
    let mut seen = events_until(&r.rx, load_timeout(), ready(1));
    let at_ready = r.g.samples(1);
    // The page logs both lines right after its ready; the dispatcher handles them in order.
    let handled = wait_for(hang_bound(), || {
        seen.extend(drain(&r.rx));
        let not_relayed = lines(&r)
            .iter()
            .filter(|l| l.contains("from a retired page; not relayed"))
            .count();
        not_relayed >= 2
            || seen
                .iter()
                .any(|e| matches!(e, Event::ScoreStale(_) | Event::PhaseChannel(_)))
    });
    assert!(handled, "gen 1's stale and channel lines were never handled");
    if !seen.iter().any(ready(2)) {
        seen.extend(events_until(&r.rx, load_timeout(), ready(2)));
    }
    assert!(
        wait_for(hang_bound(), || r.g.samples(1).delivered
            >= at_ready.delivered + 10),
        "the retired page painted nothing after its ready"
    );
    seen.extend(drain(&r.rx));
    r.g.hold_teardowns(false);
    r.g.set_reaper_kill_after(Duration::from_millis(REAPER_KILL_AFTER_MS));
    let readies = |gen: u64| seen.iter().filter(|e| ready(gen)(e)).count();
    eprintln!(
        "readies: gen 1 {}, gen 2 {}; all events {seen:?}",
        readies(1),
        readies(2)
    );
    assert_eq!(
        (readies(1), readies(2)),
        (1, 1),
        "the first page's ready must carry gen 1"
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::ScoreStale(_) | Event::PhaseChannel(_))),
        "a retired page's stale or channel line was relayed"
    );
    assert_eq!(
        r.g.samples(1).written,
        0,
        "the retired page's ready opened the gate"
    );
    r.g.stop();
}

#[test]
fn score_stale_signal_is_relayed() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page("?stale=1&fresh_after_ms=1000"));
    let seen = events_until(&r.rx, load_timeout() + hang_bound(), |e| {
        *e == Event::ScoreStale(false)
    });
    let order: Vec<&Event> = seen
        .iter()
        .filter(|e| matches!(e, Event::CefReady { .. } | Event::ScoreStale(_)))
        .collect();
    assert_eq!(
        order,
        vec![
            &Event::CefReady { gen: 1 },
            &Event::ScoreStale(true),
            &Event::ScoreStale(false)
        ]
    );
    r.g.stop();
}

#[test]
fn phase_channel_signal_is_relayed() {
    let _s = serial();
    let mut r = rig();
    let cases = [
        ("web", Some(PhaseChannel::Web)),
        ("loopback", Some(PhaseChannel::Loopback)),
        ("bogus", None),
    ];
    for (n, (value, want)) in cases.iter().enumerate() {
        let gen = n as u64 + 1;
        r.g.restart(gen, &page(&format!("?channel={value}")));
        events_until(&r.rx, load_timeout(), ready(gen));
        // The page logs its channel right after ready and beats a beat later: everything relayed
        // for the line comes before the first beat.
        let got: Vec<PhaseChannel> = events_until(&r.rx, hang_bound(), |e| *e == Event::CefAlive { gen })
            .into_iter()
            .filter_map(|e| match e {
                Event::PhaseChannel(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(got, want.iter().copied().collect::<Vec<_>>(), "channel={value}");
    }
    assert!(
        lines(&r)
            .iter()
            .any(|l| l.contains("WARN") && l.contains("seazn:phase-channel=bogus")),
        "the bogus value was not logged at WARN: {:?}",
        lines(&r)
    );
    eprintln!("channel values checked: {}", cases.len());
    r.g.stop();
}

// ---------------------------------------------------------------------------------------------
// Amendment 2026-10-18 (hang detection)

#[test]
fn alive_beats_are_relayed_with_their_generation() {
    let _s = serial();
    let mut r = rig();
    // `alive_twin=1`: the page logs `seazn:alive-x` right after ready, a near miss.
    r.g.restart(1, &page("?alive_twin=1"));
    let before = events_until(&r.rx, load_timeout(), ready(1));
    assert!(
        !before.iter().any(|e| matches!(e, Event::CefAlive { .. })),
        "a beat before ready: {before:?}"
    );
    let seen = events_until_n(&r.rx, hang_bound() * 2, 2, |e| *e == Event::CefAlive { gen: 1 });
    eprintln!("after ready: {seen:?}");
    assert!(
        seen.iter()
            .all(|e| !matches!(e, Event::CefAlive { gen } if *gen != 1)),
        "{seen:?}"
    );
    // The twin is no beat: it is logged at WARN as a line that is no signal.
    assert!(
        wait_line(&r, hang_bound(), &["WARN", "seazn:alive-x"]),
        "the twin was not logged at WARN: {:?}",
        lines(&r)
    );
    r.g.stop();
}

#[test]
fn hung_page_stops_beating() {
    let _s = serial();
    let mut r = rig();
    // Two beats (one beat apart, from ready), then the hang half a beat later.
    let hang_after = beat() * 2 + beat() / 4;
    r.g.restart(
        1,
        &page(&format!("?crash=1&crash_after_ms={}", hang_after.as_millis())),
    );
    events_until(&r.rx, load_timeout(), ready(1));
    let pids = r.g.renderer_pids(1);
    assert_eq!(pids.len(), 1, "{pids:?}");
    // The page warns `hang starts` just before its busy loop; every beat before it was relayed
    // before the line was logged.
    assert!(
        wait_line(&r, hang_after + hang_bound(), &["page console: hang starts"]),
        "the page never hung"
    );
    let before = drain(&r.rx);
    let during = events_for(&r.rx, beat() * 3);
    let beats_before = before
        .iter()
        .filter(|e| **e == Event::CefAlive { gen: 1 })
        .count();
    eprintln!("beats before the hang: {beats_before}; events during it: {during:?}");
    assert!(beats_before >= 1, "{before:?}");
    assert!(during.is_empty(), "{during:?}");
    assert!(alive(pids[0]), "the hung renderer {} died", pids[0]);
    assert_eq!(r.g.renderer_pids(1), pids);
    r.g.stop();
}

#[test]
fn reaper_kills_a_renderer_that_will_not_tear_down() {
    let _s = serial();
    let mut r = rig();
    // F-001's js-signal hang: a page that never calls gstSendMsg holds start(), and so
    // set_state(NULL), for good.
    r.g.js_signal_builds(true);
    r.g.restart(1, &page("?noready=1"));
    assert!(
        wait_for(load_timeout(), || !r.g.renderer_pids(1).is_empty()),
        "gen 1 never got a renderer"
    );
    let pids = r.g.renderer_pids(1);
    r.g.js_signal_builds(false);
    let t0 = Instant::now();
    r.g.restart(2, &page(""));
    events_until(&r.rx, load_timeout(), ready(2));
    let kill_bound = Duration::from_millis(REAPER_KILL_AFTER_MS) + hang_bound();
    assert!(
        wait_for(kill_bound.saturating_sub(t0.elapsed()), || r
            .g
            .reaper_path(1)
            .is_some()),
        "the reaper reported nothing for gen 1"
    );
    let killed_at = t0.elapsed();
    let path = r.g.reaper_path(1).expect("path");
    let killed = match path {
        ReaperPath::Killed { pids } => pids,
        ReaperPath::TornDown { ms } => {
            panic!("the js-signal hang tore down by itself in {ms} ms: this test proves nothing now")
        }
    };
    eprintln!(
        "gen 1 renderers {pids:?}; killed {killed:?} by {killed_at:?} after restart(2); respawned by \
         CEF's reload: {:?}",
        r.g.renderer_pids(1)
    );
    // Never before the bound: the reaper waits for the teardown first.
    assert!(
        killed_at >= Duration::from_millis(REAPER_KILL_AFTER_MS),
        "killed at {killed_at:?}"
    );
    assert!(!killed.is_empty(), "the reaper killed no pid");
    assert_eq!(killed, pids, "the reaper killed other pids than gen 1's");
    assert!(
        wait_for(hang_bound(), || pids.iter().all(|p| !alive(*p))),
        "a gen-1 renderer outlived the kill: {pids:?}"
    );
    // The teardown stays stuck (F-013): counted leaked once, no sooner than its bound (Owed 55).
    let leak_at = Duration::from_millis(REAPER_KILL_AFTER_MS + TEARDOWN_LEAK_AFTER_MS);
    assert!(
        wait_for((leak_at + hang_bound()).saturating_sub(t0.elapsed()), || {
            r.g.health().leaked_teardowns >= 1
        }),
        "the stuck teardown was never counted leaked: {:?}",
        r.g.health()
    );
    let leaked_seen = t0.elapsed();
    eprintln!("leaked counted by {leaked_seen:?}; {:?}", r.g.health());
    assert!(leaked_seen >= leak_at, "counted leaked at {leaked_seen:?}");
    assert_eq!(r.g.health().leaked_teardowns, 1);
    // No zygote dies in this process (only the child tests kill one): a killed renderer, or any
    // other child's exit, is no verdict.
    assert_eq!(
        r.g.health().unrecoverable,
        None,
        "a renderer's death read as a zygote's"
    );
    assert_eq!(r.g.teardowns_in_flight(), 1);
    // The count is published before its line is logged: wait for the line, bounded.
    assert!(
        wait_line(&r, hang_bound(), &["WARN", "1 leaked"]),
        "{:?}",
        lines(&r)
    );
    r.g.stop();
}

// ---------------------------------------------------------------------------------------------
// Owed 17: the ready gate, through the real programme

#[test]
fn the_hidden_page_stays_off_air_and_the_retry_reaches_air() {
    let _s = serial();
    let dir = std::env::temp_dir().join(format!("c11-hidden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let target = dir.join("a.flv");
    let frames = FrameStore::default();
    let mut r = rig_with(frames.clone(), Thresholds::default().cef_rss_cap_mb);
    let (ptx, prx) = crossbeam_channel::bounded(8_192);
    let mut p = GstProgramme::new(frames, ptx);
    let profile = OutputProfile::P720;
    p.start(&profile).expect("programme");
    p.swap_sink(
        1,
        &TargetSpec {
            url: format!("file://{}", target.display()),
            stream_key: String::new(),
        },
    );
    let period = Duration::from_nanos(1_000_000_000 / u64::from(profile.fps));
    let t0 = Instant::now();
    let mut tick = 0u64;
    let mut pace = |p: &mut GstProgramme, n: u64| {
        for _ in 0..n {
            let due = t0 + period * tick as u32;
            if let Some(w) = due.checked_duration_since(Instant::now()) {
                thread::sleep(w);
            }
            p.push(tick, FrameChoice::Slate(SlateKind::Waiting));
            tick += 1;
        }
        tick
    };
    let fails = Thresholds::default().cef_fail_count as u64;
    for gen in 1..=fails {
        r.g.restart(gen, &page("?noready=1"));
        pace(&mut p, 45);
    }
    // C08's mapping of the Failed hide (PR #32 review I2): stop, then hide.
    r.g.stop();
    p.hide_overlay();
    let hidden_from = pace(&mut p, 0);
    pace(&mut p, 45);
    let retry_at = pace(&mut p, 0);
    r.g.restart(fails + 1, &page(""));
    let mut ready_tick = None;
    let mut now = retry_at;
    let ready_ticks = load_timeout().as_millis() as u64 * u64::from(profile.fps) / 1_000;
    while ready_tick.is_none() && now < retry_at + ready_ticks {
        now = pace(&mut p, 1);
        while let Ok(Inbound::Event(e)) = r.rx.try_recv() {
            if e == (Event::CefReady { gen: fails + 1 }) {
                ready_tick = Some(now);
            }
        }
    }
    let ready_tick = ready_tick.expect("the retry never signalled ready");
    pace(&mut p, 45);
    p.drain();
    let drain_bound = Duration::from_millis(Thresholds::default().drain_timeout_ms) + hang_bound();
    events_until(&prx, drain_bound, |e| *e == Event::OutputDrained);
    let blue = decode_blue(&target);
    let _ = std::fs::remove_dir_all(&dir);
    let pushed = pace(&mut p, 0);
    let unready = blue.get(..retry_at as usize).unwrap_or_else(|| {
        panic!(
            "decoded {} frames, fewer than the {retry_at} pushed before the retry ({pushed} in all)",
            blue.len()
        )
    });
    let after_ready = blue.get(ready_tick as usize..).unwrap_or_default();
    eprintln!(
        "frames checked: {} before the retry ({} from the hide), {} after its ready ({} blue)",
        unready.len(),
        retry_at - hidden_from,
        after_ready.len(),
        after_ready.iter().filter(|b| **b).count()
    );
    assert!(!unready.is_empty() && !after_ready.is_empty());
    assert!(
        !unready.iter().any(|b| *b),
        "a blue frame before the retry, at {:?}",
        unready.iter().position(|b| *b)
    );
    assert!(after_ready.iter().any(|b| *b), "the retry never reached air");
    r.g.stop();
}

/// Per decoded programme frame: does (30, 30) carry the test square's blue (66 × 66 at 720p)?
fn decode_blue(path: &Path) -> Vec<bool> {
    let launch = format!(
        "filesrc location={} ! flvdemux name=d d.video ! queue ! h264parse ! avdec_h264 ! videoconvert \
         ! video/x-raw,format=RGB ! appsink name=v sync=false",
        path.display()
    );
    let pipeline = gst::parse::launch(&launch)
        .expect("decode")
        .downcast::<gst::Pipeline>()
        .expect("pipeline");
    let out = Arc::new(Mutex::new(Vec::new()));
    let o = Arc::clone(&out);
    let v = pipeline
        .by_name("v")
        .expect("v")
        .downcast::<gst_app::AppSink>()
        .expect("appsink");
    v.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let info = gst_video::VideoInfo::from_caps(sample.caps().expect("caps")).expect("info");
                let stride = info.stride()[0] as usize;
                let map = sample.buffer().expect("buffer").map_readable().expect("map");
                let px = &map[30 * stride + 30 * 3..30 * stride + 30 * 3 + 3];
                o.lock().unwrap().push(px[2] > 150 && px[0] < 90 && px[1] < 90);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline.set_state(gst::State::Playing).expect("decode playing");
    let msg = pipeline.bus().expect("bus").timed_pop_filtered(
        gst::ClockTime::from_seconds(30),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let _ = pipeline.set_state(gst::State::Null);
    assert!(
        matches!(msg.as_ref().map(|m| m.view()), Some(gst::MessageView::Eos(_))),
        "decoding did not finish: {msg:?}"
    );
    let v = out.lock().unwrap().clone();
    v
}

// ---------------------------------------------------------------------------------------------
// stop() and the slot gate; the hide's order; the bounded drop

/// `g.stop()` on another thread: true if it returned within `limit` (a stop that waited for a
/// held teardown would not).
fn stop_returns_within(g: &mut CefGraphics, limit: Duration) -> bool {
    let (tx, rx) = crossbeam_channel::bounded(1);
    thread::scope(|s| {
        s.spawn(move || {
            g.stop();
            let _ = tx.send(());
        });
        rx.recv_timeout(limit).is_ok()
    })
}

#[test]
fn stop_never_blocks_and_closes_the_slot_on_a_page_hung_after_ready() {
    let _s = serial();
    let mut r = rig();
    // The page hangs 3 s after ready and revives 1.5 s later. A teardown in this mode returns in
    // milliseconds (F-013), so the held teardown stands in for F-001's hung one: the revived page
    // paints while it waits, and the gate must drop it.
    r.g.hold_teardowns(true);
    r.g.restart(1, &page("?crash=1&crash_after_ms=3000&revive_after_ms=1500"));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(
        wait_line(&r, hang_bound(), &["page console: hang starts"]) || r.g.samples(1).written > 0,
        "neither a write nor the hang"
    );
    assert!(
        wait_line(&r, hang_bound(), &["page console: hang starts"]),
        "the page never hung"
    );
    let written = r.g.samples(1).written;
    assert!(
        written > 0,
        "the page hung before any write (a host that starved it for 3 s): this run proves nothing"
    );
    let pids = r.g.renderer_pids(1);
    // The hang itself: no sample for 300 ms (a paint in flight at the line may still land).
    let mut last = r.g.samples(1).delivered;
    let mut quiet_since = Instant::now();
    let quiet = wait_for(hang_bound(), || {
        let now = r.g.samples(1).delivered;
        if now != last {
            last = now;
            quiet_since = Instant::now();
        }
        quiet_since.elapsed() >= Duration::from_millis(300)
    });
    assert!(
        quiet,
        "the page kept painting through its hang: {:?}",
        r.g.samples(1)
    );
    // C08's mapping of a Hang hide (PR #32 review I2): stop, then hide_overlay (the slot cleared).
    assert!(
        stop_returns_within(&mut r.g, hang_bound()),
        "stop() waited for the held teardown"
    );
    assert_eq!(
        r.g.teardowns_in_flight(),
        1,
        "stop() returned once the teardown had"
    );
    r.frames.clear_overlay();
    let at_stop = r.g.samples(1);
    assert!(
        wait_for(hang_bound(), || r.g.samples(1).dropped > at_stop.dropped),
        "the page never painted after stop(): this run proves nothing"
    );
    let later = r.g.samples(1);
    eprintln!(
        "gen 1 wrote {written} before the hang; dropped after stop: {}",
        later.dropped - at_stop.dropped
    );
    assert!(
        r.frames.overlay().is_none(),
        "a gen-1 sample reached the slot after stop()"
    );
    assert_eq!(later.written, at_stop.written);
    assert!(!pids.is_empty());
    r.g.hold_teardowns(false);
    assert!(
        wait_for(hang_bound(), || pids.iter().all(|p| !alive(*p))),
        "gen-1 renderers outlived the teardown: {pids:?}"
    );
}

// PR #32 review I2: a sample being written when the slot is cleared lands after the clear unless the
// stop came first. The write is held (a test hook, inside the gate's lock) so both orders are exact.
#[test]
fn a_hide_before_the_stop_can_leave_a_stale_frame_and_after_it_cannot() {
    let _s = serial();
    let mut r = rig();
    // C08's former order: hide, then stop.
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(wait_for(hang_bound(), || r.g.samples(1).written > 0));
    let hold = r.g.hold_next_write();
    assert!(hold.wait_holding(hang_bound()), "no sample came to be written");
    let written = r.g.samples(1).written;
    r.frames.clear_overlay();
    hold.release();
    assert!(
        wait_for(hang_bound(), || r.g.samples(1).written > written),
        "the held sample was never written"
    );
    r.g.stop();
    let stale = r.frames.overlay();
    eprintln!(
        "hide then stop: a frame in the slot after both: {}",
        stale.is_some()
    );
    assert!(
        stale.is_some(),
        "hide, then stop: the held write did not land after the hide"
    );

    // The order C08 now uses: stop (which waits out the write in hand), then hide. The retired
    // page's teardown is held, so it keeps painting after the stop and the gate must drop it.
    r.frames.clear_overlay();
    r.g.hold_teardowns(true);
    r.g.restart(2, &page(""));
    events_until(&r.rx, load_timeout(), ready(2));
    assert!(wait_for(hang_bound(), || r.g.samples(2).written > 0));
    let hold = r.g.hold_next_write();
    assert!(hold.wait_holding(hang_bound()), "no sample came to be written");
    let (tx, rx) = crossbeam_channel::bounded(1);
    thread::scope(|s| {
        let g = &mut r.g;
        s.spawn(move || {
            g.stop();
            let _ = tx.send(());
        });
        // The stop waits for the held write: it holds the gate.
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "stop() returned while a write was in hand"
        );
        hold.release();
        assert!(rx.recv_timeout(hang_bound()).is_ok(), "stop() never returned");
    });
    r.frames.clear_overlay();
    let at_stop = r.g.samples(2);
    assert!(
        wait_for(hang_bound(), || r.g.samples(2).dropped >= at_stop.dropped + 10),
        "the page painted nothing after the stop: this run proves nothing"
    );
    let later = r.g.samples(2);
    r.g.hold_teardowns(false);
    eprintln!(
        "stop then hide: dropped after both {}",
        later.dropped - at_stop.dropped
    );
    assert!(
        r.frames.overlay().is_none(),
        "stop, then hide: a frame reached the slot"
    );
    assert_eq!(later.written, at_stop.written);
}

#[test]
fn drop_returns_within_500_ms_while_the_page_is_hung() {
    let _s = serial();
    let mut r = rig();
    // F-001's hang, for real: a js-signal page holds start(), so the build's thread is stuck in
    // set_state and its teardown can never run.
    r.g.js_signal_builds(true);
    r.g.restart(1, &page("?noready=1"));
    assert!(wait_for(load_timeout(), || !r.g.renderer_pids(1).is_empty()));
    let Rig { g, .. } = r;
    let t = Instant::now();
    drop(g);
    let took = t.elapsed();
    eprintln!("drop took {took:?}");
    // The brief's bound (amendment 2026-10-19).
    assert!(took <= Duration::from_millis(500), "drop took {took:?}");
}

#[test]
fn a_renderer_born_just_before_a_restart_stays_the_retiring_pages() {
    let _s = serial();
    let mut r = rig();
    let before: Vec<i32> = cef_children()
        .into_iter()
        .filter(|c| c.kind == "renderer")
        .map(|c| c.pid)
        .collect();
    // Gen 1's teardown is held, and the reaper's kill put off, so its renderer is still alive for
    // every claim pass after the restart (a quick teardown would end it before any pass could
    // misattribute it).
    r.g.hold_teardowns(true);
    r.g.set_reaper_kill_after(Duration::from_secs(60));
    r.g.restart(1, &page("?noready=1"));
    // Read /proc here, not through the adapter (whose read claims): restart(2) follows the new
    // renderer within ~20 ms, well inside the 1 s child poll, so only restart's own claim pass can
    // give it to gen 1.
    let mut born = None;
    assert!(
        wait_for(load_timeout(), || {
            born = cef_children()
                .into_iter()
                .find(|c| c.kind == "renderer" && !before.contains(&c.pid))
                .map(|c| c.pid);
            born.is_some()
        }),
        "gen 1 never got a renderer"
    );
    let born = born.expect("pid");
    r.g.restart(2, &page(""));
    // Gen 2's renderer, then two child polls (each claims what it finds for the newest build).
    assert!(
        wait_for(load_timeout(), || cef_children()
            .iter()
            .any(|c| c.kind == "renderer"
                && c.pid != born
                && !before.contains(&c.pid))),
        "gen 2 never got a renderer"
    );
    assert!(two_polls(&r.g), "the child poll stopped");
    assert!(
        alive(born),
        "gen 1's renderer ended before the check: this run proves nothing"
    );
    let gen1 = r.g.renderer_pids(1);
    let gen2 = r.g.renderer_pids(2);
    eprintln!("gen 1's renderer {born}: claimed by gen 1 {gen1:?}, gen 2 {gen2:?}");
    r.g.hold_teardowns(false);
    r.g.set_reaper_kill_after(Duration::from_millis(REAPER_KILL_AFTER_MS));
    assert_eq!(gen1, vec![born], "gen 1's renderer was not gen 1's");
    assert_eq!(gen2.len(), 1, "gen 2's renderers: {gen2:?}");
    assert!(!gen2.contains(&born));
    r.g.stop();
}

// ---------------------------------------------------------------------------------------------
// Guardrails (spec §6; PR #32 review G1)

fn read_i32(path: String) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[test]
fn cef_children_run_at_nice_10_with_the_renderer_first_in_the_oom_order() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(two_polls(&r.g), "the child poll stopped");
    // A child launched after a poll is graded at the next one: read the children listed here only
    // after two more polls, so a poll has seen each (a utility launched late read Chromium's own
    // 200 once, under a host at load 100).
    let kids = cef_children();
    assert!(two_polls(&r.g), "the child poll stopped");
    let kids: Vec<_> = kids.into_iter().filter(|k| alive(k.pid)).collect();
    let own = read_i32("/proc/self/oom_score_adj".to_owned()).expect("own oom_score_adj");
    let mut threads = 0;
    // (kind, oom_score_adj, oom_score): the kernel's own ordering is what the grading is for.
    let mut oom = Vec::new();
    for kid in &kids {
        let adj = read_i32(format!("/proc/{}/oom_score_adj", kid.pid)).expect("oom_score_adj");
        let score = read_i32(format!("/proc/{}/oom_score", kid.pid)).expect("oom_score");
        oom.push((kid.kind.clone(), adj, score));
        for task in std::fs::read_dir(format!("/proc/{}/task", kid.pid))
            .expect("tasks")
            .flatten()
        {
            let stat = std::fs::read_to_string(task.path().join("stat")).unwrap_or_default();
            let Some((_, rest)) = stat.rsplit_once(')') else {
                continue;
            };
            // proc(5): nice is field 19, the 17th after the name.
            let nice: i32 = rest
                .split_whitespace()
                .nth(16)
                .and_then(|n| n.parse().ok())
                .expect("nice");
            assert_eq!(nice, CEF_NICE, "{kid:?} thread {:?}", task.file_name());
            threads += 1;
        }
    }
    eprintln!(
        "CEF children checked: {} ({} renderers), threads checked: {threads}; own adj {own}; \
         (kind, adj, score): {oom:?}",
        kids.len(),
        renderer_count()
    );
    assert!(!kids.is_empty() && threads > 0);
    let of = |k: &str| oom.iter().filter(|o| o.0 == k).collect::<Vec<_>>();
    let (renderers, zygotes) = (of("renderer"), of("zygote"));
    assert!(!renderers.is_empty() && !zygotes.is_empty(), "{oom:?}");
    for (kind, adj, score) in &oom {
        // Every CEF child goes before the compositor itself.
        assert!(*adj > own, "{kind} at {adj}, not above the compositor's {own}");
        for (_, r_adj, r_score) in &renderers {
            if kind != "renderer" {
                assert!(
                    adj < r_adj && score < r_score,
                    "{kind} ({adj}, {score}) ties a renderer"
                );
            }
        }
        for (_, z_adj, z_score) in &zygotes {
            if kind != "zygote" {
                assert!(
                    adj > z_adj && score > z_score,
                    "{kind} ({adj}, {score}) at or below a zygote"
                );
            }
        }
    }
    // Spec §6: the renderer at 1000, the top of the scale.
    assert!(renderers.iter().all(|r| r.1 == 1_000), "{renderers:?}");
    r.g.stop();
}

// Owed 56, owner ruling 2026-10-19 "B": a dead zygote ends CEF for the process, so the adapter
// reports it (C11b restarts in place). Killing a zygote can end CEF, so each case runs in a child
// process of its own: the GPU process's zygote alone (the renderers' zygote alive, so the verdict
// is not "no zygote left"), then the renderers' zygote alone (the GPU one alive).
#[test]
fn a_dead_zygote_either_one_is_reported() {
    let _s = serial();
    let name = "a_dead_zygote_either_one_is_reported";
    if let Some(which) = zygote_child_case() {
        return zygote_case(&which);
    }
    let mut checked = Vec::new();
    for which in ["gpu", "renderer"] {
        let out = rerun_in_child(name, &[("C11_ZYGOTE", which)]);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let mine: Vec<String> = stderr
            .lines()
            .filter(|l| l.starts_with("zygote child:"))
            .map(str::to_owned)
            .collect();
        assert!(out.status.success(), "the {which} child failed:\n{stderr}");
        assert_eq!(mine.len(), 1, "{stderr}");
        checked.extend(mine);
    }
    eprintln!("children checked: {}; {checked:?}", checked.len());
    assert_eq!(checked.len(), 2);
}

// The PR #32 confirm pass's N1, pinned: why the GPU process's zygote's death is the verdict. With
// that zygote dead, the next GPU-process death is Chromium's FATAL "GPU process isn't usable.
// Goodbye." and the whole process dies of SIGTRAP: in production, the compositor, off air. The
// child reports the verdict first, then kills the GPU process.
#[test]
fn a_gpu_process_death_after_its_zygotes_aborts_the_whole_process() {
    use std::os::unix::process::ExitStatusExt;
    let _s = serial();
    let name = "a_gpu_process_death_after_its_zygotes_aborts_the_whole_process";
    if let Some(which) = zygote_child_case() {
        return zygote_case(&which);
    }
    let out = rerun_in_child(name, &[("C11_ZYGOTE", "abort")]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let reached: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("zygote child:"))
        .collect();
    assert_eq!(
        reached.len(),
        1,
        "the child never reached the GPU-process kill ({:?}):\n{stderr}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !stderr.contains("zygote child survived"),
        "the process outlived the GPU process:\n{stderr}"
    );
    assert_eq!(
        out.status.signal(),
        Some(5),
        "not SIGTRAP: {:?}\n{stderr}",
        out.status
    );
    assert!(
        stderr.contains("GPU process isn't usable"),
        "no Chromium FATAL:\n{stderr}"
    );
    eprintln!("abort child: {:?}; {reached:?}", out.status);
}

// Owed 61 (C11b): Chromium aborts the process at the 6th GPU-process death of its life (F-013), so
// the adapter counts each one, from real kills: the GPU process killed `cef_gpu_deaths_to_restart`
// times (two short of the abort), each counted once it is gone, and its relaunch killed next. It
// spends the process's GPU deaths, so it runs in a child of its own.
#[test]
fn gpu_process_deaths_are_counted_from_real_kills() {
    let _s = serial();
    let name = "gpu_process_deaths_are_counted_from_real_kills";
    if std::env::var_os("C11_CHILD").is_some() {
        return gpu_deaths_case();
    }
    let out = rerun_in_child(name, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let reached: Vec<&str> = stderr.lines().filter(|l| l.starts_with("gpu child:")).collect();
    assert!(
        out.status.success(),
        "the child failed ({:?}):\n{stderr}",
        out.status
    );
    assert_eq!(reached.len(), 1, "{stderr}");
    eprintln!("{}", reached[0]);
}

fn gpu_deaths_case() {
    let n = Thresholds::default().cef_gpu_deaths_to_restart;
    assert!(n < CHROMIUM_GPU_DEATHS_ABORT, "the child would abort");
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(two_polls(&r.g), "the child poll stopped");
    assert_eq!(
        r.g.health(),
        CefHealth::default(),
        "healthy before the first kill"
    );
    let mut killed: Vec<i32> = Vec::new();
    for k in 1..=n {
        // The GPU process now: the first one, then each relaunch (a pid not killed yet).
        let mut pid = None;
        let found = wait_for(hang_bound(), || {
            pid = cef_children()
                .iter()
                .find(|c| c.kind == "gpu-process" && !killed.contains(&c.pid))
                .map(|c| c.pid);
            pid.is_some()
        });
        let pid = pid
            .filter(|_| found)
            .unwrap_or_else(|| panic!("death {k}: no GPU process"));
        // A poll has listed it before it dies (a pid no poll saw cannot be counted).
        assert!(two_polls(&r.g), "the child poll stopped");
        let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
        killed.push(pid);
        assert!(
            wait_for(hang_bound(), || r.g.health().gpu_process_deaths >= k),
            "death {k} (pid {pid}) was not counted: {:?}",
            r.g.health()
        );
        assert_eq!(r.g.health().gpu_process_deaths, k, "death {k}: counted once");
    }
    assert!(two_polls(&r.g), "the child poll stopped");
    let h = r.g.health();
    assert_eq!(h.gpu_process_deaths, n, "{h:?}");
    assert_eq!(h.unrecoverable, None, "the zygotes live: {h:?}");
    assert!(
        wait_line(&r, hang_bound(), &[&format!("{n} in this process")]),
        "{:?}",
        lines(&r)
    );
    let warned = lines(&r)
        .iter()
        .filter(|l| l.starts_with("WARN gen=1:") && l.contains("GPU process(es) gone"))
        .count();
    assert_eq!(warned, n, "one line per poll that saw a death: {:?}", lines(&r));
    eprintln!("gpu child: {n} GPU processes killed {killed:?}, {h:?}");
    r.g.stop();
}

/// `C11_ZYGOTE`, in a zygote test's child.
fn zygote_child_case() -> Option<String> {
    std::env::var_os("C11_CHILD")?;
    std::env::var("C11_ZYGOTE").ok().filter(|w| !w.is_empty())
}

/// One zygote case, in a child process: `gpu` and `abort` kill the GPU process's zygote, `renderer`
/// the renderers'; `abort` then kills the GPU process too.
fn zygote_case(which: &str) {
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, load_timeout(), ready(1));
    assert!(wait_for(hang_bound(), || !r.g.renderer_pids(1).is_empty()));
    let kids = cef_children();
    let renderer = kids
        .iter()
        .find(|k| k.kind == "renderer")
        .expect("renderer")
        .clone();
    // The zygotes are this process's own children (a zygote's fresh fork can read `--type=zygote`).
    let zygotes: Vec<i32> = kids
        .iter()
        .filter(|k| k.kind == "zygote" && k.ppid == std::process::id() as i32)
        .map(|k| k.pid)
        .collect();
    assert!(
        zygotes.contains(&renderer.ppid),
        "the renderer's parent is no zygote: {kids:?}"
    );
    let gpu: Vec<i32> = zygotes.iter().copied().filter(|z| *z != renderer.ppid).collect();
    let gpu_processes: Vec<i32> = kids
        .iter()
        .filter(|k| k.kind == "gpu-process" && gpu.contains(&k.ppid))
        .map(|k| k.pid)
        .collect();
    assert!(!gpu.is_empty(), "one zygote only: {kids:?}");
    assert!(
        !gpu_processes.is_empty(),
        "no GPU process under the GPU zygote: {kids:?}"
    );
    assert!(two_polls(&r.g), "the child poll stopped");
    assert_eq!(r.g.health().unrecoverable, None);
    let (victims, survivors) = if which == "renderer" {
        (vec![renderer.ppid], gpu.clone())
    } else {
        (gpu.clone(), vec![renderer.ppid])
    };
    for z in &victims {
        let _ = Command::new("kill").args(["-KILL", &z.to_string()]).status();
    }
    // Reported with the newest generation, within the child poll's next rounds.
    assert!(
        wait_for(hang_bound(), || r.g.health().unrecoverable.is_some()),
        "the {which} zygote's death was not reported: {:?}",
        r.g.health()
    );
    assert_eq!(r.g.health().unrecoverable, Some(1));
    assert!(
        survivors.iter().all(|z| alive(*z)),
        "the other zygote died too: this run proves only \"no zygote left\""
    );
    // The verdict is published before its line is logged: wait for the line, bounded.
    assert!(
        wait_line(&r, hang_bound(), &["ERROR gen=1", "zygote"]),
        "{:?}",
        lines(&r)
    );
    if which != "renderer" {
        // The final C11 review's G4: the dead zygote's GPU process is reparented away and leaves
        // `cef_children()` while it lives on. It is no GPU-process death until it is gone (C11b).
        assert!(two_polls(&r.g), "the child poll stopped");
        let orphans: Vec<i32> = gpu_processes.iter().copied().filter(|p| alive(*p)).collect();
        let tree: Vec<i32> = cef_children().iter().map(|k| k.pid).collect();
        let h = r.g.health();
        eprintln!("zygote orphans: GPU processes {gpu_processes:?}, alive {orphans:?}, {h:?}");
        assert!(
            orphans.iter().all(|p| !tree.contains(p)),
            "G4: {orphans:?} still in {tree:?}"
        );
        assert_eq!(
            h.gpu_process_deaths,
            gpu_processes.len() - orphans.len(),
            "G4: an orphan that lives is no death"
        );
    }
    match which {
        "renderer" => {
            // The next build fails at once (F-013).
            r.g.restart(2, &page(""));
            events_until(&r.rx, load_timeout(), crashed(2));
        }
        "abort" => {
            eprintln!(
                "zygote child: GPU zygotes {gpu:?} killed, {:?}; killing GPU processes {gpu_processes:?}",
                r.g.health()
            );
            for g in &gpu_processes {
                let _ = Command::new("kill").args(["-KILL", &g.to_string()]).status();
            }
            // Probe CG: the abort came within 10 s. Twice that, then say so.
            thread::sleep(load_timeout() * 2);
            eprintln!("zygote child survived the GPU process: {:?}", r.g.health());
            r.g.stop();
            return;
        }
        _ => {}
    }
    eprintln!(
        "zygote child: {which} zygotes {victims:?} killed ({survivors:?} alive), {:?}",
        r.g.health()
    );
    r.g.stop();
}

// ---------------------------------------------------------------------------------------------
// The page token (the dispatch's secret rule)

/// Made up for the tests; no credential.
const TOKEN: &str = "c11madeuptokenNOTREAL0123456789";
const DISK_TOKEN: &str = "c11madeupDISKtokenNOTREAL0123456";

/// A local HTTP server for the harness pages (`python3 -m http.server`), and its port.
struct PageServer {
    child: std::process::Child,
    port: u16,
}

impl PageServer {
    fn start() -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let child = Command::new("python3")
            .args([
                "-m",
                "http.server",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--directory",
            ])
            .arg(pages_dir())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 http.server");
        let up = wait_for(Duration::from_secs(5), || {
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        assert!(up, "the page server never listened on {port}");
        Self { child, port }
    }
}

impl Drop for PageServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// The PR #32 confirm pass's N4: the private profile fails closed. With no directory to be had
// (an unwritable TMPDIR), no build is made, so CEF never starts on its HOME default; each build
// fails as a graphics failure, logged through the adapter's sink, and the domain's retry loads once
// the directory can be made, sweeping a dead process's first (that line through the sink too). It
// runs in a child: the first build that does start is CEF's start, and its profile is CEF's.
#[test]
fn a_profile_directory_that_cannot_be_made_refuses_the_build_and_the_retry_loads() {
    use std::os::unix::fs::PermissionsExt;
    let _s = serial();
    let name = "a_profile_directory_that_cannot_be_made_refuses_the_build_and_the_retry_loads";
    const REFUSED: &str = "no private CEF profile directory";
    // A dead process's profile, there before the child starts.
    const STALE: &str = "seazn-cef-2147483646-1";
    if std::env::var_os("C11_CHILD").is_some() {
        let tmp = std::env::temp_dir();
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let url = page(&format!("?st={TOKEN}"));
        let mut r = rig();
        for gen in [1, 2] {
            r.g.restart(gen, &url);
            let seen = events_until(&r.rx, load_timeout(), crashed(gen));
            assert!(!seen.contains(&Event::CefReady { gen }), "{seen:?}");
            assert!(
                lines(&r)
                    .iter()
                    .any(|l| l.contains(&format!("gen={gen}: {REFUSED}"))),
                "{:?}",
                lines(&r)
            );
        }
        assert_eq!(r.g.profile_dir(), None);
        assert!(cef_children().is_empty(), "CEF started: {:?}", cef_children());
        let (hits, files) = files_holding(&home, TOKEN.as_bytes());
        assert!(
            hits.is_empty(),
            "the token under HOME, of {files} files: {hits:?}"
        );
        // The directory can be made again: the next build loads, in it.
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        r.g.restart(3, &url);
        events_until(&r.rx, load_timeout(), ready(3));
        let dir = r.g.profile_dir().expect("the profile directory");
        assert!(dir.starts_with(&tmp), "{dir:?}");
        assert!(!tmp.join(STALE).exists(), "the dead process's directory was kept");
        assert!(
            lines(&r)
                .iter()
                .any(|l| l.contains("removed 1 CEF profile directories")),
            "{:?}",
            lines(&r)
        );
        assert!(!lines(&r)
            .iter()
            .any(|l| l.contains("gen=3: ") && l.contains(REFUSED)));
        eprintln!("profile child: gens 1 and 2 refused, gen 3 ready in {dir:?}");
        r.g.stop();
        return;
    }
    let ro = std::env::temp_dir().join(format!("c11-ro-tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ro);
    std::fs::create_dir_all(ro.join(STALE)).expect("TMPDIR");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let out = rerun_in_child(name, &[("TMPDIR", ro.to_str().expect("utf-8"))]);
    let _ = std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_dir_all(&ro);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the child failed:\n{stderr}");
    let reached: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("profile child:"))
        .collect();
    assert_eq!(reached.len(), 1, "{stderr}");
    // Through the adapter's sink, never raw stderr: every refusal line, and the sweep's, is the
    // sink's echo.
    let refusals: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains(REFUSED) || l.contains("CEF profile directories"))
        .collect();
    assert!(refusals.len() >= 3, "{stderr}");
    assert!(
        refusals.iter().all(|l| l.starts_with("adapter: ")),
        "a refusal line bypassed the sink: {refusals:?}"
    );
    eprintln!("refusal lines checked: {}; {reached:?}", refusals.len());
}

// PR #32 review I1: CEF writes every URL it loads, token and all, to its profile (History, Top
// Sites, the HTTP cache). Over http (the real page's scheme) and past History's commit, the token
// is on disk only in the adapter's private profile, and gone once the adapter is dropped.
//
// It runs in a child process, with a HOME and a TMPDIR of its own that are read once more after it
// exits: CEF opens History once, in its first profile, and the last adapter's drop (the process's
// exit, in production) removes that directory under it, so a later test's History goes to an
// unlinked file no scan can see.
#[test]
fn a_page_token_rests_on_disk_only_in_the_private_profile_until_the_drop() {
    let _s = serial();
    let name = "a_page_token_rests_on_disk_only_in_the_private_profile_until_the_drop";
    if std::env::var_os("C11_CHILD").is_none() {
        let base = std::env::temp_dir().join(format!("c11-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (home, tmp) = (base.join("home"), base.join("tmp"));
        std::fs::create_dir_all(&home).expect("child HOME");
        std::fs::create_dir_all(&tmp).expect("child TMPDIR");
        let out = run_child(name, &home, &[("TMPDIR", tmp.to_str().expect("utf-8"))]);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let (hits, files): (Vec<PathBuf>, usize) = [&home, &tmp]
            .iter()
            .map(|root| files_holding(root, DISK_TOKEN.as_bytes()))
            .fold((Vec::new(), 0), |(mut h, n), (more, m)| {
                h.extend(more);
                (h, n + m)
            });
        let _ = std::fs::remove_dir_all(&base);
        let checked: Vec<&str> = stderr.lines().filter(|l| l.starts_with("disk child:")).collect();
        eprintln!(
            "child: {}; {checked:?}; after its exit: files checked {files}, hits {hits:?}",
            out.status
        );
        assert!(out.status.success(), "the child failed:\n{stderr}");
        assert_eq!(checked.len(), 1, "{stderr}");
        assert!(files > 0);
        assert!(hits.is_empty(), "the token outlived the child: {hits:?}");
        return;
    }
    let server = PageServer::start();
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let tmp = std::env::temp_dir();
    let mut r = rig();
    let url = format!(
        "http://127.0.0.1:{}/overlay-test.html?host=compositor&sid=SID42&st={DISK_TOKEN}",
        server.port
    );
    r.g.restart(1, &url);
    events_until(&r.rx, load_timeout(), ready(1));
    // Made at the first build (the confirm pass's N4: no directory, no build).
    let profile = r.g.profile_dir();
    // The positive control: the token reaches History's file (which commits on its own clock), so
    // every place CEF writes it has had its chance.
    let mut held: Vec<PathBuf> = Vec::new();
    let reached = wait_for(HISTORY_COMMIT + hang_bound(), || {
        held = [&home, &tmp]
            .iter()
            .flat_map(|root| files_holding(root, DISK_TOKEN.as_bytes()).0)
            .collect();
        held.iter().any(|p| p.file_name().is_some_and(|n| n == "History"))
    });
    eprintln!("profile {profile:?}; token on disk in {held:?}");
    assert!(
        reached,
        "the token never reached History: this run proves nothing"
    );
    let in_home: Vec<&PathBuf> = held.iter().filter(|p| p.starts_with(&home)).collect();
    assert!(
        in_home.is_empty(),
        "CEF's profile under HOME holds the token: {in_home:?}"
    );
    let profile = profile.expect("a private profile directory");
    assert!(
        held.iter().all(|p| p.starts_with(&profile)),
        "the token outside the private profile: {held:?}"
    );
    r.g.stop();
    let Rig { g, .. } = r;
    drop(g);
    drop(server);
    // Past one more commit, in case anything writes after the drop.
    thread::sleep(HISTORY_COMMIT + Duration::from_secs(2));
    let (home_hits, home_files) = files_holding(&home, DISK_TOKEN.as_bytes());
    let (tmp_hits, tmp_files) = files_holding(&tmp, DISK_TOKEN.as_bytes());
    eprintln!(
        "after the drop: files checked {home_files} under HOME, {tmp_files} under {}; hits {:?}",
        tmp.display(),
        [&home_hits, &tmp_hits]
    );
    assert!(home_files + tmp_files > 0);
    assert!(
        home_hits.is_empty() && tmp_hits.is_empty(),
        "the token outlived the drop: {home_hits:?} {tmp_hits:?}"
    );
    // The drop removed the directory, but the live CEF's disk cache writes its index again later
    // (`Cache_Data/index-dir/the-real-index`, hashes and no URL; F-013): what comes back holds no
    // token (above), and the next start's sweep removes it.
    let back = files_holding(&profile, b"").0;
    eprintln!("written again after the drop: {back:?}");
    eprintln!(
        "disk child: History and {} other files held the token, all in {}; none after the drop ({} \
         written again)",
        held.len() - 1,
        profile.display(),
        back.len()
    );
}

#[test]
fn the_page_token_never_reaches_a_log_line_chrome_debug_log_or_an_argv() {
    let _s = serial();
    // The child (below) runs this test under a real GST_DEBUG and prints to its real stderr.
    let child = std::env::var_os("C11_CHILD").is_some();
    let forwarded: Lines = Arc::default();
    gst::init().expect("gstreamer");
    if !child {
        let sink = Arc::clone(&forwarded);
        CefGraphics::forward_gst_lines(Some(Arc::new(move |l: &str| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(l.to_owned());
        })));
        // An operator's GST_DEBUG, in-process: everything cefsrc says, and a busy core category.
        gst::log::set_threshold_for_name("cefsrc", gst::DebugLevel::Trace);
        gst::log::set_threshold_for_name("cefconsole", gst::DebugLevel::Trace);
        gst::log::set_threshold_for_name("GST_STATES", gst::DebugLevel::Debug);
    }
    let mut r = rig();
    let url = page(&format!(
        "?host=compositor&sid=SID42&st={TOKEN}&stale=1&channel=bogus&alive_twin=1&echo_url=1&echo_token=1"
    ));
    r.g.restart(1, &url);
    events_until(&r.rx, load_timeout(), ready(1));
    // The page's own lines, which carry the token: its URL (with a `?` before the token, which
    // `redact` masks by shape too) and the bare token (which only the scrubber, from the URL it
    // learned, can mask: PR #32 review m1).
    assert!(
        wait_line(&r, hang_bound(), &["page console: page url "])
            && wait_line(&r, hang_bound(), &["page console: page token "])
            && wait_line(&r, hang_bound(), &["WARN", "seazn:alive-x"]),
        "the page's lines never reached the adapter's log: {:?}",
        lines(&r)
    );
    assert!(two_polls(&r.g), "the child poll stopped");
    let argvs: Vec<String> = cef_children()
        .iter()
        .map(|c| {
            String::from_utf8_lossy(&std::fs::read(format!("/proc/{}/cmdline", c.pid)).unwrap_or_default())
                .into_owned()
        })
        .collect();
    r.g.stop();
    if !child {
        CefGraphics::forward_gst_lines(None);
        gst::log::set_threshold_for_name("cefsrc", gst::DebugLevel::Warning);
        gst::log::set_threshold_for_name("cefconsole", gst::DebugLevel::Info);
        gst::log::set_threshold_for_name("GST_STATES", gst::DebugLevel::None);
    }
    let own = lines(&r);
    let fwd = forwarded.lock().unwrap_or_else(PoisonError::into_inner).clone();
    // CEF writes chrome_debug.log beside its profile: the private one (I1), and HOME's in case.
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let cef_log: String = [
        r.g.profile_dir().map(|p| p.join("chrome_debug.log")),
        Some(home.join(".config/cef_user_data/chrome_debug.log")),
    ]
    .into_iter()
    .flatten()
    .map(|p| std::fs::read_to_string(p).unwrap_or_default())
    .collect();
    eprintln!(
        "lines checked: {} of the adapter's, {} forwarded, {} of chrome_debug.log; argvs checked: {}",
        own.len(),
        fwd.len(),
        cef_log.lines().count(),
        argvs.len()
    );
    assert!(!own.is_empty() && !argvs.is_empty());
    assert!(child || !fwd.is_empty(), "nothing was forwarded");
    for l in own.iter().chain(&fwd) {
        assert!(
            !l.contains(TOKEN),
            "a log line carries the token: {}",
            l.replace(TOKEN, "<TOKEN>")
        );
    }
    // The bare token's line, masked.
    let token_line = own
        .iter()
        .find(|l| l.contains("page console: page token "))
        .expect("the token line");
    assert!(token_line.contains("page token ****"), "{token_line}");
    // Empty is the expected state (log-severity=disable); at CEF's default severity it holds every
    // console line with its source URL, token included (F-013), which the mutation sweep shows red.
    assert!(!cef_log.contains(TOKEN), "chrome_debug.log carries the token");
    for a in &argvs {
        assert!(!a.contains(TOKEN), "a CEF child's argv carries the token");
    }

    // GStreamer's own handler, under a real GST_DEBUG: this test again, in a child process, whose
    // stderr is read whole.
    if !child {
        let out = rerun_in_child(
            "the_page_token_never_reaches_a_log_line_chrome_debug_log_or_an_argv",
            &[("GST_DEBUG", "4,cefsrc:7,cefconsole:7")],
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        eprintln!(
            "child: {}, stderr lines checked: {}",
            out.status,
            stderr.lines().count()
        );
        assert!(
            out.status.success(),
            "the child failed: {}\n{}",
            stdout.replace(TOKEN, "<TOKEN>"),
            stderr.replace(TOKEN, "<TOKEN>")
        );
        assert!(stderr.lines().count() > 0, "the child printed nothing to stderr");
        // The operator's `cefsrc:7` is honoured (the adapter only ever raises a threshold): cefsrc's
        // LOG lines (`painting, width / height`, once a paint) are printed, through the scrubber.
        let painting = stderr
            .lines()
            .filter(|l| l.contains("cefsrc") && l.contains("painting"))
            .count();
        eprintln!("child cefsrc paint lines: {painting}");
        assert!(painting > 0, "GST_DEBUG=cefsrc:7 printed no cefsrc LOG line");
        assert!(!stderr.contains(TOKEN), "the child's stderr carries the token");
        assert!(!stdout.contains(TOKEN), "the child's stdout carries the token");
    }
}
