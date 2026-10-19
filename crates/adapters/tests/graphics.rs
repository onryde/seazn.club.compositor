//! C11: the overlay page in CEF, against the real gstcefsrc (F-001's build). These tests need cefsrc
//! and an X display, so they run in the C12 image (or the C00 spike image with a Rust toolchain)
//! only: `Xvfb :99 & DISPLAY=:99 cargo test -p adapters --features cef --test graphics --
//! --test-threads=1`. CEF is initialised once per process and outlives every adapter, so the tests
//! take `serial()` and run one at a time.
//!
//! Expected values come from the page (`harness/pages/overlay-test.html`, its CSS and its lines),
//! the brief, `Thresholds::default()` and the findings (F-001, F-013), never from the adapter.
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use adapters::cef_graphics::{
    cef_children, CefGraphics, ReaperPath, CEF_NICE, CEF_OOM_SCORE_ADJ, REAPER_KILL_AFTER_MS,
    SPARE_RENDERER_OFF,
};
use adapters::frame_store::FrameStore;
use adapters::gst_programme::GstProgramme;
use app::ports::{GraphicsRenderer, Inbound, ProgrammeOutput, TargetSpec};
use crossbeam_channel::Receiver;
use domain::config::{OutputProfile, Thresholds};
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
/// The page beats every `cef_alive_every_ms` after ready (R2005; the page's own setInterval).
fn beat() -> Duration {
    Duration::from_millis(Thresholds::default().cef_alive_every_ms)
}

// ---------------------------------------------------------------------------------------------
// Harness

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn page(query: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness/pages/overlay-test.html")
        .canonicalize()
        .expect("the harness page");
    format!("file://{}{query}", p.display())
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
    // The token test's child prints the adapter's lines too: they join the stderr it checks.
    let echo = std::env::var_os("C11_TOKEN_CHILD").is_some();
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

/// Every event until `pred` matches one (included), or panics after `timeout`.
fn events_until(rx: &Receiver<Inbound>, timeout: Duration, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let end = Instant::now() + timeout;
    let mut seen = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Inbound::Event(e)) => {
                let hit = pred(&e);
                seen.push(e);
                if hit {
                    return seen;
                }
            }
            Ok(Inbound::Session(_)) => {}
            Err(_) => panic!("no matching event within {timeout:?}; saw {seen:?}"),
        }
    }
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

fn ready(gen: u64) -> impl Fn(&Event) -> bool {
    move |e| *e == Event::CefReady { gen }
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

// ---------------------------------------------------------------------------------------------
// The brief's tests (Step 1, amended)

#[test]
fn ready_signal_arrives_with_its_generation() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    assert!(
        wait_for(Duration::from_secs(2), || r.frames.overlay().is_some()),
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
    let seen = events_for(&r.rx, Duration::from_secs(8));
    let samples = r.g.samples(1);
    eprintln!(
        "noready: {} samples delivered, events {seen:?}",
        samples.delivered
    );
    assert!(samples.delivered > 0, "the page painted no frame at all");
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
    // The first generation, on an empty slot.
    r.g.restart(1, &page("?noready=1"));
    thread::sleep(Duration::from_secs(3));
    let first = r.g.samples(1);
    assert!(first.delivered > 0, "gen 1 painted nothing: {first:?}");
    assert!(
        r.frames.overlay().is_none(),
        "a frame of an unready page reached the slot"
    );
    // A previous generation's frame, held through an unready one.
    r.g.restart(2, &page(""));
    events_until(&r.rx, Duration::from_secs(5), ready(2));
    assert!(wait_for(Duration::from_secs(2), || r.frames.overlay().is_some()));
    r.g.restart(3, &page("?noready=1"));
    let held = bytes(&r.frames.overlay().expect("gen 2's frame"));
    thread::sleep(Duration::from_secs(3));
    let third = r.g.samples(3);
    eprintln!("unready samples: gen 1 {first:?}; gen 3 {third:?}");
    assert!(third.delivered > 0, "gen 3 painted nothing: {third:?}");
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
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    assert!(wait_for(Duration::from_secs(2), || r.frames.overlay().is_some()));
    r.g.restart(2, &page("?ready_after_ms=4000"));
    let held = r.frames.overlay().expect("gen 1's last frame");
    assert!(
        wait_for(Duration::from_secs(3), || r.g.samples(2).delivered >= 3),
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
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    assert!(wait_for(Duration::from_secs(2), || !r
        .g
        .renderer_pids(1)
        .is_empty()));
    let pids = r.g.renderer_pids(1);
    // F-013: no spare renderer, so the build has exactly one.
    assert_eq!(pids.len(), 1, "gen 1's renderers: {pids:?}");
    assert!(wait_for(Duration::from_secs(1), || r.frames.overlay().is_some()));
    let killed = Command::new("kill")
        .args(["-KILL", &pids[0].to_string()])
        .status()
        .expect("kill");
    assert!(killed.success());
    events_until(&r.rx, Duration::from_secs(2), |e| {
        *e == Event::CefCrashed { gen: 1 }
    });
    let held = bytes(&r.frames.overlay().expect("held"));
    // cefsrc reloads the page, and the reloaded page signals ready again: the crashed build's gate
    // stays shut (the reload's frames never reach the slot).
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    let at_reload = r.g.samples(1);
    thread::sleep(Duration::from_secs(1));
    let later = r.g.samples(1);
    eprintln!(
        "killed renderer {}; after the reload: {at_reload:?} → {later:?}",
        pids[0]
    );
    assert!(
        later.dropped > at_reload.dropped,
        "the reloaded page painted nothing"
    );
    assert_eq!(bytes(&r.frames.overlay().expect("held")), held);
    r.g.stop();
}

#[test]
fn rss_over_cap_reports_crash() {
    let _s = serial();
    let mut r = rig_with(FrameStore::default(), 1);
    r.g.restart(1, &page(""));
    events_until(&r.rx, Duration::from_secs(3), |e| {
        *e == Event::CefCrashed { gen: 1 }
    });
    r.g.stop();
}

#[test]
fn a_reported_crash_closes_its_generations_gate() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    assert!(wait_for(Duration::from_secs(3), || r.g.samples(1).written >= 5));
    r.g.set_rss_cap_mb(1);
    events_until(&r.rx, Duration::from_secs(3), |e| {
        *e == Event::CefCrashed { gen: 1 }
    });
    let held = bytes(&r.frames.overlay().expect("the last frame"));
    let at_report = r.g.samples(1);
    thread::sleep(Duration::from_millis(1_500));
    let later = r.g.samples(1);
    eprintln!("at the report {at_report:?}; 1.5 s later {later:?}");
    assert_eq!(
        later.written, at_report.written,
        "a sample reached the slot after the report"
    );
    assert!(
        later.dropped > at_report.dropped,
        "the page painted nothing after the report"
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
    // Gen 1's teardown is held (a stand-in for a slow one), so its page signals ready, stale and
    // its channel AFTER it was retired; gen 2 signals much later.
    r.g.delay_teardowns(Duration::from_millis(2_800));
    r.g.restart(1, &page("?ready_after_ms=1200&stale=1&channel=web"));
    thread::sleep(Duration::from_millis(300));
    r.g.restart(2, &page("?ready_after_ms=3500"));
    let seen = events_for(&r.rx, Duration::from_secs(6));
    r.g.delay_teardowns(Duration::ZERO);
    let readies: Vec<u64> = seen
        .iter()
        .filter_map(|(_, e)| match e {
            Event::CefReady { gen } => Some(*gen),
            _ => None,
        })
        .collect();
    eprintln!("readies in order: {readies:?}; all events {seen:?}");
    assert_eq!(readies, vec![1, 2], "the first page's ready must carry gen 1");
    assert!(
        !seen
            .iter()
            .any(|(_, e)| matches!(e, Event::ScoreStale(_) | Event::PhaseChannel(_))),
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
    let seen = events_until(&r.rx, Duration::from_secs(7), |e| *e == Event::ScoreStale(false));
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
        events_until(&r.rx, Duration::from_secs(5), ready(gen));
        let got: Vec<PhaseChannel> = events_for(&r.rx, Duration::from_secs(3))
            .into_iter()
            .filter_map(|(_, e)| match e {
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
    let t0 = Instant::now();
    // `alive_twin=1`: the page logs `seazn:alive-x` right after ready, a near miss.
    r.g.restart(1, &page("?alive_twin=1"));
    let seen = events_for(&r.rx, Duration::from_secs(7));
    let at_ready = seen
        .iter()
        .position(|(_, e)| *e == Event::CefReady { gen: 1 })
        .expect("CefReady{1}");
    let ready_at = seen[at_ready].0;
    assert!(
        !seen[..at_ready]
            .iter()
            .any(|(_, e)| matches!(e, Event::CefAlive { .. })),
        "a beat before ready: {seen:?}"
    );
    let beats: Vec<Duration> = seen[at_ready..]
        .iter()
        .filter(|(_, e)| *e == Event::CefAlive { gen: 1 })
        .map(|(t, _)| t.duration_since(ready_at))
        .collect();
    eprintln!(
        "ready {:?} after restart; beats after ready: {beats:?}",
        ready_at - t0
    );
    assert!(beats.len() >= 2, "{seen:?}");
    // The first real beat is one beat after ready; the twin came at once and is no beat.
    assert!(beats[0] > beat() * 3 / 4, "{beats:?}");
    assert!(
        seen.iter()
            .all(|(_, e)| !matches!(e, Event::CefAlive { gen } if *gen != 1)),
        "{seen:?}"
    );
    assert!(lines(&r)
        .iter()
        .any(|l| l.contains("WARN") && l.contains("seazn:alive-x")));
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
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    let ready_at = Instant::now();
    let before = events_for(&r.rx, hang_after - Duration::from_millis(100));
    let pids = r.g.renderer_pids(1);
    assert_eq!(pids.len(), 1, "{pids:?}");
    thread::sleep(
        (ready_at + hang_after + Duration::from_millis(100)).saturating_duration_since(Instant::now()),
    );
    let during = events_for(&r.rx, Duration::from_secs(3));
    let beats_before = before
        .iter()
        .filter(|(_, e)| *e == Event::CefAlive { gen: 1 })
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
        wait_for(Duration::from_secs(5), || !r.g.renderer_pids(1).is_empty()),
        "gen 1 never got a renderer"
    );
    thread::sleep(Duration::from_millis(500));
    let pids = r.g.renderer_pids(1);
    r.g.js_signal_builds(false);
    let t0 = Instant::now();
    r.g.restart(2, &page(""));
    let budget = Duration::from_millis(REAPER_KILL_AFTER_MS + 2_000);
    events_until(&r.rx, budget, ready(2));
    assert!(
        wait_for(budget.saturating_sub(t0.elapsed()), || r
            .g
            .reaper_path(1)
            .is_some()),
        "the reaper reported nothing for gen 1"
    );
    let path = r.g.reaper_path(1).expect("path");
    let killed = match path {
        ReaperPath::Killed { pids } => pids,
        ReaperPath::TornDown { ms } => {
            panic!("the js-signal hang tore down by itself in {ms} ms: this test proves nothing now")
        }
    };
    eprintln!(
        "gen 1 renderers {pids:?}; killed {killed:?} {:?} after restart(2); respawned by CEF's reload: {:?}",
        t0.elapsed(),
        r.g.renderer_pids(1)
    );
    assert!(!killed.is_empty(), "the reaper killed no pid");
    assert_eq!(killed, pids, "the reaper killed other pids than gen 1's");
    assert!(
        wait_for(budget.saturating_sub(t0.elapsed()), || pids
            .iter()
            .all(|p| !alive(*p))),
        "a gen-1 renderer outlived the kill: {pids:?}"
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
    // C08's mapping of the Failed hide.
    p.hide_overlay();
    r.g.stop();
    let hidden_from = pace(&mut p, 0);
    pace(&mut p, 45);
    let retry_at = pace(&mut p, 0);
    r.g.restart(fails + 1, &page(""));
    let mut ready_tick = None;
    let mut now = retry_at;
    while ready_tick.is_none() && now < retry_at + 30 * 6 {
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
    events_until(&prx, Duration::from_secs(15), |e| *e == Event::OutputDrained);
    let blue = decode_blue(&target);
    let _ = std::fs::remove_dir_all(&dir);
    let unready: Vec<bool> = blue[..retry_at as usize].to_vec();
    let after_ready: Vec<bool> = blue[ready_tick as usize..].to_vec();
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
// stop() and the slot gate; the bounded drop

#[test]
fn stop_never_blocks_and_closes_the_slot_on_a_page_hung_after_ready() {
    let _s = serial();
    let mut r = rig();
    // The page hangs 500 ms after ready and revives 1 500 ms later. A teardown in this mode returns
    // in milliseconds (F-013), so the held teardown stands in for F-001's hung one: the revived
    // page paints while it waits, and the gate must drop it.
    r.g.delay_teardowns(Duration::from_millis(2_500));
    r.g.restart(1, &page("?crash=1&crash_after_ms=500&revive_after_ms=1500"));
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    let ready_at = Instant::now();
    let first_write = wait_for(Duration::from_millis(450), || r.g.samples(1).written > 0);
    assert!(
        first_write,
        "no write {:?} after ready: {:?}",
        ready_at.elapsed(),
        r.g.samples(1)
    );
    let pids = r.g.renderer_pids(1);
    // Wait for the hang itself, seen as no sample for 300 ms (the page paints at 30 fps until it
    // starts; a loaded host delays the page's timer), so the hide and the stop land inside it.
    let mut last = r.g.samples(1).delivered;
    let mut quiet_since = Instant::now();
    let hung = wait_for(Duration::from_secs(3), || {
        let now = r.g.samples(1).delivered;
        if now != last {
            last = now;
            quiet_since = Instant::now();
        }
        quiet_since.elapsed() >= Duration::from_millis(300)
    });
    assert!(hung, "the page never hung: {:?}", r.g.samples(1));
    let written = r.g.samples(1).written;
    // C08's mapping of a Hang hide: hide_overlay (the programme clears the slot), then stop.
    r.frames.clear_overlay();
    let t = Instant::now();
    r.g.stop();
    let took = t.elapsed();
    let at_stop = r.g.samples(1);
    thread::sleep(Duration::from_millis(2_000));
    let later = r.g.samples(1);
    eprintln!(
        "gen 1 wrote {written} before the hang; stop() took {took:?}; dropped after stop: {}",
        later.dropped - at_stop.dropped
    );
    assert!(written > 0);
    assert!(took < Duration::from_millis(200), "stop() took {took:?}");
    assert!(
        r.frames.overlay().is_none(),
        "a gen-1 sample reached the slot after stop()"
    );
    assert_eq!(later.written, at_stop.written);
    assert!(
        later.dropped > at_stop.dropped,
        "the page never painted after stop(): this run proves nothing"
    );
    assert!(!pids.is_empty());
    assert!(
        wait_for(Duration::from_millis(REAPER_KILL_AFTER_MS + 2_000), || pids
            .iter()
            .all(|p| !alive(*p))),
        "gen-1 renderers outlived the teardown: {pids:?}"
    );
    r.g.delay_teardowns(Duration::ZERO);
}

#[test]
fn drop_returns_within_500_ms_while_the_page_is_hung() {
    let _s = serial();
    let mut r = rig();
    // F-001's hang, for real: a js-signal page holds start(), so the build's thread is stuck in
    // set_state and its teardown can never run.
    r.g.js_signal_builds(true);
    r.g.restart(1, &page("?noready=1"));
    assert!(wait_for(Duration::from_secs(5), || !r
        .g
        .renderer_pids(1)
        .is_empty()));
    thread::sleep(Duration::from_millis(500));
    let Rig { g, .. } = r;
    let t = Instant::now();
    drop(g);
    let took = t.elapsed();
    eprintln!("drop took {took:?}");
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
    // Gen 1's teardown is held, so its renderer is still alive for every claim pass after the
    // restart (a quick teardown would end it before any pass could misattribute it).
    r.g.delay_teardowns(Duration::from_millis(2_500));
    r.g.restart(1, &page("?noready=1"));
    // Read /proc here, not through the adapter (whose read claims): restart(2) follows the new
    // renderer within ~20 ms, well inside the 1 s child poll, so only restart's own claim pass can
    // give it to gen 1.
    let mut born = None;
    assert!(
        wait_for(Duration::from_secs(5), || {
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
    events_until(&r.rx, Duration::from_secs(5), ready(2));
    // Past one child poll, while gen 1's teardown is still held.
    thread::sleep(Duration::from_millis(1_200));
    assert!(
        alive(born),
        "gen 1's renderer ended before the check: this run proves nothing"
    );
    let gen1 = r.g.renderer_pids(1);
    let gen2 = r.g.renderer_pids(2);
    eprintln!("gen 1's renderer {born}: claimed by gen 1 {gen1:?}, gen 2 {gen2:?}");
    r.g.delay_teardowns(Duration::ZERO);
    assert_eq!(gen1, vec![born], "gen 1's renderer was not gen 1's");
    assert_eq!(gen2.len(), 1, "gen 2's renderers: {gen2:?}");
    assert!(!gen2.contains(&born));
    r.g.stop();
}

// ---------------------------------------------------------------------------------------------
// Guardrails and secrets (spec §6; the dispatch's secret rule)

#[test]
fn cef_children_run_at_nice_10_with_oom_score_adj_1000() {
    let _s = serial();
    let mut r = rig();
    r.g.restart(1, &page(""));
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    // Two polls.
    thread::sleep(Duration::from_millis(2_500));
    let kids = cef_children();
    let mut threads = 0;
    for kid in &kids {
        let oom = std::fs::read_to_string(format!("/proc/{}/oom_score_adj", kid.pid)).unwrap_or_default();
        assert_eq!(oom.trim(), CEF_OOM_SCORE_ADJ.to_string(), "{kid:?}");
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
        "CEF children checked: {} ({} renderers), threads checked: {threads}",
        kids.len(),
        renderer_count()
    );
    assert!(!kids.is_empty() && threads > 0);
    r.g.stop();
}

/// Made up for the test; no credential.
const TOKEN: &str = "c11madeuptokenNOTREAL0123456789";

#[test]
fn the_page_token_never_reaches_a_log_a_file_or_an_argv() {
    let _s = serial();
    // The child (below) runs this test under a real GST_DEBUG and prints to its real stderr.
    let child = std::env::var_os("C11_TOKEN_CHILD").is_some();
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
        "?host=compositor&sid=SID42&st={TOKEN}&stale=1&channel=bogus&alive_twin=1&echo_url=1"
    ));
    r.g.restart(1, &url);
    events_until(&r.rx, Duration::from_secs(5), ready(1));
    thread::sleep(Duration::from_millis(2_500));
    let argvs: Vec<String> = cef_children()
        .iter()
        .map(|c| {
            String::from_utf8_lossy(&std::fs::read(format!("/proc/{}/cmdline", c.pid)).unwrap_or_default())
                .into_owned()
        })
        .collect();
    r.g.stop();
    thread::sleep(Duration::from_millis(300));
    if !child {
        CefGraphics::forward_gst_lines(None);
        gst::log::set_threshold_for_name("cefsrc", gst::DebugLevel::Warning);
        gst::log::set_threshold_for_name("cefconsole", gst::DebugLevel::Info);
        gst::log::set_threshold_for_name("GST_STATES", gst::DebugLevel::None);
    }
    let own = lines(&r);
    let fwd = forwarded.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let home = std::env::var("HOME").unwrap_or_default();
    let cef_log = std::fs::read_to_string(Path::new(&home).join(".config/cef_user_data/chrome_debug.log"))
        .unwrap_or_default();
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
    // Empty is the expected state (log-severity=disable); at CEF's default severity it holds every
    // console line with its source URL, token included (F-013), which the mutation sweep shows red.
    assert!(!cef_log.contains(TOKEN), "chrome_debug.log carries the token");
    for a in &argvs {
        assert!(!a.contains(TOKEN), "a CEF child's argv carries the token");
    }
    // The page's own lines did arrive (scrubbed): the WARN for the twin.
    assert!(own.iter().any(|l| l.contains("seazn:alive-x")));
    // ...and the page's echo of its own URL, which carries the token: logged, scrubbed.
    assert!(
        own.iter().any(|l| l.contains("page console: page url ")),
        "the page's URL echo never reached the adapter's log: {own:?}"
    );

    // GStreamer's own handler, under a real GST_DEBUG: this test again, in a child process, whose
    // stderr is read whole.
    if !child {
        // A HOME of its own: this process's CEF holds the profile in `~/.config/cef_user_data` for
        // good, and a second CEF on the same profile refuses to start.
        let home = std::env::temp_dir().join(format!("c11-token-child-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("child HOME");
        let out = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "the_page_token_never_reaches_a_log_a_file_or_an_argv",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("C11_TOKEN_CHILD", "1")
            .env("HOME", &home)
            .env("GST_DEBUG", "4,cefsrc:7,cefconsole:7")
            .output()
            .expect("child");
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
        let child_cef_log =
            std::fs::read_to_string(home.join(".config/cef_user_data/chrome_debug.log")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&home);
        assert!(stderr.lines().count() > 0, "the child printed nothing to stderr");
        // The operator's `cefsrc:7` is honoured (the adapter only ever raises a threshold): cefsrc's
        // LOG lines (`painting, width / height`, once a paint) are printed, through the scrubber.
        let painting = stderr
            .lines()
            .filter(|l| l.contains("cefsrc") && l.contains("painting"))
            .count();
        eprintln!("child cefsrc paint lines: {painting}");
        assert!(painting > 0, "GST_DEBUG=cefsrc:7 printed no cefsrc LOG line");
        assert!(
            !child_cef_log.contains(TOKEN),
            "the child's chrome_debug.log carries the token"
        );
        assert!(!stderr.contains(TOKEN), "the child's stderr carries the token");
        assert!(!stdout.contains(TOKEN), "the child's stdout carries the token");
    }
}
