//! C10: the input pipeline against real GStreamer. Every test feeds a local SRT **listener** it starts
//! itself (`Listener`: `videotestsrc ! x264enc ! mpegtsmux ! srtsink mode=listener`, plus an
//! `audiotestsrc` branch), and the adapter is the caller, as it is to Cloudflare Input A. The tests
//! share GStreamer and the machine's CPU, so each takes `serial()` first.
//!
//! Expected values come from the brief, `Thresholds::default()`, `OutputProfile`, the frame store's
//! declared audio format and the findings (F-002, F-012), never from the adapter.
use std::net::UdpSocket;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use adapters::frame_store::{FrameStore, AUDIO_CHANNELS, AUDIO_RATE, AUDIO_RING_BYTES};
use adapters::gst_input::GstInput;
use adapters::gst_programme::GstProgramme;
use app::ports::{Inbound, InputSource, InputSpec, ProgrammeOutput, TargetSpec};
use crossbeam_channel::{Receiver, Sender};
use domain::config::{OutputProfile, Thresholds};
use domain::input_link::LossReason;
use domain::programme::FrameChoice;
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;

const P720: OutputProfile = OutputProfile::P720;
const P1080: OutputProfile = OutputProfile::P1080;
/// The listener's frame rate: a phone camera's 30 fps (F-002's source).
const SOURCE_FPS: u32 = 30;
/// The brief's floor for the second after `InputConnected`: at least 25 of the source's 30 frames.
const FRAMES_IN_THE_FIRST_SECOND: usize = 25;
/// Limited-range luma: picture white is 235, black 16 (BT.709 / BT.601 alike). A border pixel reads
/// under this, a white picture pixel over `WHITE_LUMA`.
const BLACK_LUMA: u8 = 40;
const WHITE_LUMA: u8 = 200;
/// SRT's own peer-close report reaches the bus at once on a graceful close (F-012: 47 ms).
const LOSS_BUDGET: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------------------------
// Harness

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn t() -> Thresholds {
    Thresholds::default()
}

fn connect_budget() -> Duration {
    Duration::from_millis(t().input_connect_timeout_ms)
}

/// A UDP port nothing holds now.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .expect("a free udp port")
}

fn spec(port: u16) -> InputSpec {
    InputSpec {
        url: format!("srt://127.0.0.1:{port}"),
        stream_id: String::new(),
        passphrase: String::new(),
    }
}

/// What a listener sends.
#[derive(Clone)]
struct Cfg {
    w: u32,
    h: u32,
    pattern: &'static str,
    colorimetry: &'static str,
    /// `None`: no audio track at all.
    audio: Option<Tone>,
    passphrase: Option<String>,
    /// Pixel aspect ratio of the camera's pixels; `None` is square.
    par: Option<(i32, i32)>,
}

#[derive(Clone, Copy)]
struct Tone {
    freq: f64,
    rate: u32,
    channels: u32,
}

const TONE_440: Tone = Tone {
    freq: 440.0,
    rate: 44_100,
    channels: 1,
};

fn cfg(w: u32, h: u32) -> Cfg {
    Cfg {
        w,
        h,
        pattern: "ball",
        colorimetry: "bt709",
        audio: Some(TONE_440),
        passphrase: None,
        par: None,
    }
}

/// A local SRT listener, as Cloudflare's playback is to the adapter. Stopping it (or dropping it)
/// closes the socket gracefully. `mpegtsmux alignment=7` puts seven 188-byte packets in each buffer,
/// SRT's 1 316-byte live payload: `srtsink` sends a buffer as one message, and a larger one (every
/// keyframe at the default alignment) never arrived, so the caller decoded nothing (F-012).
struct Listener {
    pipeline: gst::Pipeline,
    port: u16,
    /// Stream ids callers presented (`caller-connecting`).
    stream_ids: Arc<Mutex<Vec<String>>>,
    /// Callers whose handshake completed (`caller-added`).
    callers: Arc<AtomicUsize>,
}

impl Listener {
    fn start(port: u16, c: &Cfg) -> Listener {
        gst::init().expect("gstreamer");
        let audio = c
            .audio
            .map(|a| {
                format!(
                    " audiotestsrc is-live=true wave=sine freq={} volume=0.5 ! audio/x-raw,rate={},channels={} \
                     ! audioconvert ! voaacenc ! aacparse ! m.",
                    a.freq, a.rate, a.channels
                )
            })
            .unwrap_or_default();
        let desc = format!(
            "videotestsrc is-live=true pattern={pattern} ! capsfilter name=vcf \
             caps=video/x-raw,format=I420,width={w},height={h},framerate={SOURCE_FPS}/1,colorimetry={col}{par} \
             ! x264enc tune=zerolatency speed-preset=ultrafast key-int-max=30 \
             ! h264parse config-interval=-1 ! mpegtsmux name=m alignment=7 \
             ! srtsink name=out uri=srt://127.0.0.1:{port}?mode=listener wait-for-connection=false{audio}",
            pattern = c.pattern,
            w = c.w,
            h = c.h,
            col = c.colorimetry,
            par = c
                .par
                .map(|(n, d)| format!(",pixel-aspect-ratio={n}/{d}"))
                .unwrap_or_default(),
        );
        let pipeline = gst::parse::launch(&desc)
            .expect("listener pipeline")
            .downcast::<gst::Pipeline>()
            .expect("a pipeline");
        let out = pipeline.by_name("out").expect("srtsink");
        if let Some(p) = &c.passphrase {
            out.set_property("passphrase", p.as_str());
        }
        let stream_ids: Arc<Mutex<Vec<String>>> = Arc::default();
        let ids = Arc::clone(&stream_ids);
        // gst-inspect reports `authentication` as default true, but 1.24.2 never emits
        // `caller-connecting` until the property is set (measured: 0 calls unset, 1 set; F-012).
        out.set_property("authentication", true);
        out.connect("caller-connecting", false, move |args| {
            let id = args.get(2).and_then(|v| v.get::<Option<String>>().ok()).flatten();
            ids.lock().unwrap().push(id.unwrap_or_default());
            Some(true.to_value())
        });
        let callers = Arc::new(AtomicUsize::new(0));
        let added = Arc::clone(&callers);
        out.connect("caller-added", false, move |_| {
            added.fetch_add(1, Ordering::SeqCst);
            None
        });
        pipeline.set_state(gst::State::Playing).expect("listener plays");
        Listener {
            pipeline,
            port,
            stream_ids,
            callers,
        }
    }

    fn new(c: &Cfg) -> Listener {
        Listener::start(free_port(), c)
    }

    /// A listener that completes the handshake and then sends nothing at all (F-002: CF holds a
    /// silent playback socket open).
    fn silent(port: u16) -> Listener {
        gst::init().expect("gstreamer");
        let desc = format!(
            "appsrc is-live=true format=time caps=video/mpegts,systemstream=true,packetsize=188 \
             ! srtsink name=out uri=srt://127.0.0.1:{port}?mode=listener wait-for-connection=false \
             async=false sync=false"
        );
        let pipeline = gst::parse::launch(&desc)
            .expect("silent listener")
            .downcast::<gst::Pipeline>()
            .expect("a pipeline");
        let out = pipeline.by_name("out").expect("srtsink");
        let callers = Arc::new(AtomicUsize::new(0));
        let added = Arc::clone(&callers);
        out.connect("caller-added", false, move |_| {
            added.fetch_add(1, Ordering::SeqCst);
            None
        });
        pipeline.set_state(gst::State::Playing).expect("listener plays");
        Listener {
            pipeline,
            port,
            stream_ids: Arc::default(),
            callers,
        }
    }

    /// The camera's next frames at `w × h`, in the same connection (a phone rotated mid-match).
    fn resize(&self, w: u32, h: u32, colorimetry: &str) {
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "I420")
            .field("width", w as i32)
            .field("height", h as i32)
            .field("framerate", gst::Fraction::new(SOURCE_FPS as i32, 1))
            .field("colorimetry", colorimetry)
            .build();
        self.pipeline
            .by_name("vcf")
            .expect("capsfilter")
            .set_property("caps", caps);
    }

    fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

type Lines = Arc<Mutex<Vec<String>>>;

fn input_with(tx: Sender<Inbound>, out: &OutputProfile) -> (GstInput, FrameStore, Lines) {
    gst::init().expect("gstreamer");
    let frames = FrameStore::default();
    let lines: Lines = Arc::default();
    let sink = Arc::clone(&lines);
    let input = GstInput::new(
        frames.clone(),
        tx,
        t().input_srt_latency_ms,
        out.width,
        out.height,
    )
    .with_log(Arc::new(move |l: &str| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(l.to_owned());
    }));
    (input, frames, lines)
}

fn input(out: &OutputProfile) -> (GstInput, Receiver<Inbound>, FrameStore, Lines) {
    let (tx, rx) = crossbeam_channel::bounded(8_192);
    let (i, f, l) = input_with(tx, out);
    (i, rx, f, l)
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
            Err(_) => panic!(
                "no matching event within {timeout:?}; saw {} events, the last {:?}",
                seen.len(),
                seen.iter().rev().take(5).collect::<Vec<_>>()
            ),
        }
    }
}

/// Every event that arrives within `window`.
fn events_for(rx: &Receiver<Inbound>, window: Duration) -> Vec<Event> {
    let end = Instant::now() + window;
    let mut seen = Vec::new();
    while let Ok(m) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
        if let Inbound::Event(e) = m {
            seen.push(e);
        }
    }
    seen
}

fn frames_in(events: &[Event]) -> usize {
    events.iter().filter(|e| **e == Event::InputFrame).count()
}

fn losses(events: &[Event]) -> Vec<(u64, LossReason)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::InputLost { gen, reason } => Some((*gen, *reason)),
            _ => None,
        })
        .collect()
}

fn connects(events: &[Event]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::InputConnected { gen } => Some(*gen),
            _ => None,
        })
        .collect()
}

/// `rebuild(gen)` and wait for its `InputConnected`; returns the time it took.
fn connect(input: &mut GstInput, rx: &Receiver<Inbound>, gen: u64, s: &InputSpec) -> Duration {
    let t0 = Instant::now();
    input.rebuild(gen, s);
    let seen = events_until(rx, connect_budget(), |e| *e == Event::InputConnected { gen });
    assert!(
        losses(&seen).is_empty(),
        "no loss before gen {gen} connected: {seen:?}"
    );
    t0.elapsed()
}

fn camera_info(frames: &FrameStore) -> gst_video::VideoInfo {
    let sample = frames.camera().expect("a camera frame in the slot");
    gst_video::VideoInfo::from_caps(sample.caps().expect("caps")).expect("video info")
}

/// The camera slot's frame is the programme's shape: I420, `w × h`, square pixels, BT.709.
fn assert_programme_shape(frames: &FrameStore, out: &OutputProfile, phase: &str) {
    let info = camera_info(frames);
    assert_eq!(info.format(), gst_video::VideoFormat::I420, "{phase}");
    assert_eq!((info.width(), info.height()), (out.width, out.height), "{phase}");
    assert_eq!(info.par(), gst::Fraction::new(1, 1), "{phase}");
    assert_eq!(
        info.colorimetry().to_string(),
        "bt709",
        "{phase}: the programme's base-layer colours (F-010)"
    );
}

/// The luma of the camera slot's frame at (`x`, `y`).
fn luma(frames: &FrameStore, x: u32, y: u32) -> u8 {
    let sample = frames.camera().expect("a camera frame");
    let info = gst_video::VideoInfo::from_caps(sample.caps().expect("caps")).expect("info");
    let buffer = sample.buffer_owned().expect("buffer");
    let frame = gst_video::VideoFrame::from_buffer_readable(buffer, &info).expect("frame");
    let stride = frame.plane_stride()[0] as usize;
    frame.plane_data(0).expect("luma plane")[y as usize * stride + x as usize]
}

/// The camera slot's buffer PTS, to tell one frame from the next.
fn camera_pts(frames: &FrameStore) -> Option<gst::ClockTime> {
    frames.camera()?.buffer()?.pts()
}

// ---------------------------------------------------------------------------------------------
// The brief's six

/// Brief test 1, in both modes (720p and 1080p, AGENTS.md §5 "the other mode").
#[test]
fn connects_and_delivers_frames_with_its_generation() {
    let _s = serial();
    let mut checked = 0;
    for out in [P720, P1080] {
        let l = Listener::new(&cfg(1280, 720));
        let (mut input, rx, frames, _lines) = input(&out);
        let took = connect(&mut input, &rx, 1, &spec(l.port));
        let second = events_for(&rx, Duration::from_secs(1));
        let n = frames_in(&second);
        eprintln!(
            "{}x{}: connected in {took:?}; {n} frames in the next second",
            out.width, out.height
        );
        assert!(n >= FRAMES_IN_THE_FIRST_SECOND, "{n} frames");
        assert!(losses(&second).is_empty(), "{second:?}");
        assert!(connects(&second).is_empty(), "InputConnected once: {second:?}");
        assert_programme_shape(&frames, &out, "connected");
        assert_eq!(input.generation_of_last_frame(), Some(1));
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// Brief test 2 (Review Focus 1): 640×360, then the listener restarts at 1920×1080 and the input is
/// rebuilt; every camera frame is the programme's 1280×720 in both phases.
#[test]
fn input_caps_change_does_not_reach_programme() {
    let _s = serial();
    let port = free_port();
    let (mut input, rx, frames, _lines) = input(&P720);
    let mut checked = 0;
    for (gen, (w, h)) in [(1u64, (640, 360)), (2, (1920, 1080))] {
        let l = Listener::start(port, &cfg(w, h));
        connect(&mut input, &rx, gen, &spec(port));
        for _ in 0..10 {
            assert_programme_shape(&frames, &P720, &format!("{w}x{h}"));
            checked += 1;
            thread::sleep(Duration::from_millis(50));
        }
        l.stop();
        events_until(&rx, LOSS_BUDGET, |e| matches!(e, Event::InputLost { .. }));
    }
    eprintln!("camera frames checked: {checked}");
    assert_eq!(checked, 20);
}

/// Brief test 3, as measured (F-012): `srtsrc` 1.24 with `auto-reconnect` off posts an ERROR, never
/// an EOS, when its peer closes the connection. So the loss is `Error`, with the closed generation's
/// number, and it is the generation's only loss. F-002 never saw Cloudflare close a reader (it holds
/// a silent socket open instead), so nothing measured says CF would produce anything else.
#[test]
fn listener_closing_reports_an_error_loss_with_the_right_generation() {
    let _s = serial();
    let port = free_port();
    let (mut input, rx, _frames, _lines) = input(&P720);
    let l = Listener::start(port, &cfg(640, 360));
    connect(&mut input, &rx, 1, &spec(port));
    drop(l);
    let l2 = Listener::start(port, &cfg(640, 360));
    let seen = events_until(&rx, LOSS_BUDGET, |e| matches!(e, Event::InputLost { .. }));
    assert_eq!(losses(&seen), vec![(1, LossReason::Error)]);
    // A second generation on a new listener: its own loss carries its own number.
    connect(&mut input, &rx, 2, &spec(port));
    drop(l2);
    let seen = events_until(&rx, LOSS_BUDGET, |e| matches!(e, Event::InputLost { .. }));
    assert_eq!(losses(&seen), vec![(2, LossReason::Error)]);
    let after = events_for(&rx, Duration::from_secs(2));
    assert!(losses(&after).is_empty(), "one loss a generation: {after:?}");
}

/// Brief test 4: after `rebuild(2)` returns, nothing of generation 1 reaches the camera slot, even
/// while the retired pipeline is still running (its teardown is held 1.5 s) and still delivering.
#[test]
fn rebuild_tears_down_the_old_pipeline() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, _frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    assert_eq!(input.generation_of_last_frame(), Some(1));
    input.delay_teardowns(Duration::from_millis(1_500));
    let dropped_before = input.gate_dropped();
    input.rebuild(2, &spec(l.port));
    let end = Instant::now() + Duration::from_millis(2_500);
    let (mut reads, mut seen_two) = (0, false);
    while Instant::now() < end {
        let g = input.generation_of_last_frame();
        if g == Some(2) {
            seen_two = true;
        }
        assert!(
            !(seen_two && g != Some(2)),
            "generation {g:?} reached the slot after generation 2 did"
        );
        assert_ne!(g, Some(0));
        reads += 1;
        thread::sleep(Duration::from_millis(5));
    }
    let gate = input.gate_dropped() - dropped_before;
    eprintln!("slot reads: {reads}; generation-1 samples dropped at the gate: {gate}");
    assert!(seen_two, "generation 2 delivered");
    assert!(
        gate > 0,
        "the retired pipeline delivered while held, and the gate dropped it"
    );
    assert_eq!(input.generation_of_last_frame(), Some(2));
    assert_eq!(input.builds(), 2);
}

/// Brief test 5: the URI's credentials are percent-encoded, empty ones are left out, and no form of
/// either credential reaches a log line, an error's text or `Debug`.
#[test]
fn srt_uri_encodes_and_never_appears_in_logs() {
    let _s = serial();
    // The expected strings are written out from RFC 3986, not computed by the adapter.
    let s = InputSpec {
        url: "srt://live.example.com:778".to_owned(),
        stream_id: "play=abc&def".to_owned(),
        passphrase: "p&ss=w/rd+1234 5".to_owned(),
    };
    assert_eq!(
        GstInput::srt_uri(&s, 1_000),
        "srt://live.example.com:778?mode=caller&streamid=play%3Dabc%26def\
         &passphrase=p%26ss%3Dw%2Frd%2B1234%205&latency=1000"
    );
    let bare = InputSpec {
        stream_id: String::new(),
        passphrase: String::new(),
        ..s.clone()
    };
    let uri = GstInput::srt_uri(&bare, 250);
    assert_eq!(uri, "srt://live.example.com:778?mode=caller&latency=250");
    assert!(!uri.contains("passphrase") && !uri.contains("streamid"));

    // A failing connect (nothing listens on port 9), with both credentials set.
    let (mut input, rx, _frames, lines) = input(&P720);
    let secret = InputSpec {
        url: "srt://127.0.0.1:9".to_owned(),
        stream_id: "playSECRETSTREAM&id=1".to_owned(),
        passphrase: "SECRETPASS&word=2345".to_owned(),
    };
    input.rebuild(1, &secret);
    let seen = events_until(&rx, Duration::from_secs(8), |e| {
        matches!(e, Event::InputLost { .. })
    });
    assert_eq!(losses(&seen), vec![(1, LossReason::Error)]);
    assert!(connects(&seen).is_empty());
    // An element error whose text echoes the URI and both values (raw and encoded) is scrubbed too.
    let uri = GstInput::srt_uri(&secret, 1_000);
    input.rebuild(2, &secret);
    assert!(input.plant_error(&format!(
        "could not open {uri}: stream {} pass {} ({} {})",
        secret.stream_id, secret.passphrase, "playSECRETSTREAM%26id%3D1", "SECRETPASS%26word%3D2345"
    )));
    events_until(&rx, Duration::from_secs(8), |e| {
        *e == Event::InputLost {
            gen: 2,
            reason: LossReason::Error,
        }
    });
    thread::sleep(Duration::from_millis(200));
    let debug = format!("{input:?}");
    let lines = lines.lock().unwrap().clone();
    let errors = lines.iter().filter(|l| l.contains("error")).count();
    eprintln!("log lines checked: {}, error lines: {errors}", lines.len());
    assert!(errors >= 2, "both errors were logged: {lines:?}");
    // Each build names its source, so the URL reached the log path and only its origin was written.
    assert!(
        lines.iter().any(|l| l == "gen=1: srt://127.0.0.1:9 latency=1000"),
        "{lines:?}"
    );
    let forms = [
        "SECRETSTREAM",
        "SECRETPASS",
        "playSECRETSTREAM%26id%3D1",
        "SECRETPASS%26word%3D2345",
        "id=1",
        "word=2345",
    ];
    for text in lines.iter().chain([&debug]) {
        for f in forms {
            assert!(!text.contains(f), "{f:?} leaked in {text:?}");
        }
    }
}

/// Brief test 6, and the format the frame store declares: after 1 s connected the ring holds real
/// audio, as S16LE 48 kHz stereo. The listener sends a 440 Hz sine at 44.1 kHz mono, so a ring at the
/// wrong rate or channel count reads at another pitch or with channels that differ.
#[test]
fn audio_reaches_the_ring() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    thread::sleep(Duration::from_secs(1));
    let bytes = frames.take_audio(6_400);
    assert!(bytes.iter().any(|&b| b != 0), "not all zeros");
    // A fuller look: 100 ms of the ring, read as interleaved S16LE stereo at 48 kHz.
    thread::sleep(Duration::from_millis(300));
    let n = (AUDIO_RATE / 10 * AUDIO_CHANNELS * 2) as usize;
    let pcm = frames.take_audio(n);
    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let (left, right): (Vec<i16>, Vec<i16>) = samples.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
    assert_eq!(left.len(), (AUDIO_RATE / 10) as usize);
    // `audioconvert` dithers its float-to-S16 step per channel (TPDF, its default), so the two copies
    // of a mono source differ by a few LSB; a channel left silent or swapped differs by the tone.
    let peak = left.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    let spread = left
        .iter()
        .zip(&right)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    eprintln!("peak {peak}, largest left-right difference {spread}");
    assert!(peak > 1_000, "the tone is there: peak {peak}");
    assert!(
        spread <= 4,
        "a mono source is the same on both channels: {spread}"
    );
    let crossings = left.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    let hz = crossings as f64 / 2.0 / 0.1;
    eprintln!(
        "samples checked: {}; zero crossings {crossings}, about {hz:.0} Hz",
        left.len()
    );
    assert!(
        (TONE_440.freq * 0.95..=TONE_440.freq * 1.05).contains(&hz),
        "{hz} Hz against the listener's {} Hz",
        TONE_440.freq
    );
}

// ---------------------------------------------------------------------------------------------
// Sequences (the dispatch): loss and recovery, reconnects, a source that never starts, a caps change
// mid-stream, EOS against error, a full slot

/// One connection, three shapes: landscape, then the phone rotated (portrait, and BT.601 colours from
/// its encoder), then landscape again. Every frame is the programme's box; the portrait one is
/// pillarboxed, its picture centred at its own aspect. Geometry from the rulebook: 360×640 into
/// 1280×720 scales by 720/640 to 405×720, leaving 437 px of black on each side.
#[test]
fn a_rotation_mid_stream_stays_in_the_box_letterboxed() {
    let _s = serial();
    let mut c = cfg(640, 360);
    c.pattern = "white";
    let l = Listener::new(&c);
    let (mut input, rx, frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    let mut checked = 0;
    let wait_for = |frames: &FrameStore, pred: &dyn Fn(&FrameStore) -> bool, what: &str| {
        let end = Instant::now() + Duration::from_secs(5);
        while !pred(frames) {
            assert!(Instant::now() < end, "{what} never showed");
            thread::sleep(Duration::from_millis(20));
        }
    };
    let pillarboxed = |f: &FrameStore| luma(f, 200, 360) < BLACK_LUMA && luma(f, 1_080, 360) < BLACK_LUMA;
    let full = |f: &FrameStore| luma(f, 200, 360) > WHITE_LUMA && luma(f, 1_080, 360) > WHITE_LUMA;

    wait_for(&frames, &full, "the landscape picture");
    assert_programme_shape(&frames, &P720, "landscape");
    checked += 1;

    l.resize(360, 640, "bt601");
    wait_for(&frames, &pillarboxed, "the pillarboxed portrait picture");
    for _ in 0..10 {
        assert_programme_shape(&frames, &P720, "portrait");
        assert!(luma(&frames, 640, 360) > WHITE_LUMA, "the picture is centred");
        assert!(luma(&frames, 0, 0) < BLACK_LUMA, "the border is black");
        checked += 1;
        thread::sleep(Duration::from_millis(30));
    }

    l.resize(640, 360, "bt709");
    wait_for(&frames, &full, "the landscape picture again");
    assert_programme_shape(&frames, &P720, "landscape again");
    checked += 1;
    let seen = events_for(&rx, Duration::from_millis(200));
    assert!(losses(&seen).is_empty(), "a caps change is not a loss: {seen:?}");
    assert_eq!(input.builds(), 1, "and nothing was rebuilt");
    eprintln!("frames checked: {checked}");
    assert_eq!(checked, 12);
}

/// The camera goes away (the listener closes), the generation is reported lost, and once the camera is
/// back a rebuild connects and its frames reach the slot.
#[test]
fn camera_loss_then_recovery() {
    let _s = serial();
    let port = free_port();
    let (mut input, rx, frames, _lines) = input(&P720);
    let l = Listener::start(port, &cfg(640, 360));
    connect(&mut input, &rx, 1, &spec(port));
    let before = camera_pts(&frames);
    l.stop();
    let seen = events_until(&rx, LOSS_BUDGET, |e| matches!(e, Event::InputLost { .. }));
    assert_eq!(losses(&seen), vec![(1, LossReason::Error)]);
    let quiet = events_for(&rx, Duration::from_millis(500));
    assert_eq!(frames_in(&quiet), 0, "no frame after the loss");
    drop(l);
    // The domain backs off, then rebuilds; the camera is back by then.
    let _l2 = Listener::start(port, &cfg(1280, 720));
    connect(&mut input, &rx, 2, &spec(port));
    let after = events_for(&rx, Duration::from_secs(1));
    eprintln!("frames after the recovery: {}", frames_in(&after));
    assert!(frames_in(&after) >= FRAMES_IN_THE_FIRST_SECOND);
    assert_ne!(
        camera_pts(&frames),
        before,
        "the slot holds the new generation's frames"
    );
    assert_eq!(input.generation_of_last_frame(), Some(2));
}

/// Three generations, each lost and rebuilt: each connects once with its own number, each loss names
/// its own, and every retired pipeline is torn down.
#[test]
fn reconnecting_twice_builds_three_generations_and_leaks_none() {
    let _s = serial();
    let port = free_port();
    let (mut input, rx, _frames, _lines) = input(&P720);
    let mut all = Vec::new();
    for gen in 1..=3u64 {
        let l = Listener::start(port, &cfg(640, 360));
        input.rebuild(gen, &spec(port));
        all.extend(events_until(&rx, connect_budget(), |e| {
            *e == Event::InputConnected { gen }
        }));
        all.extend(events_for(&rx, Duration::from_millis(500)));
        l.stop();
        all.extend(events_until(&rx, LOSS_BUDGET, |e| {
            matches!(e, Event::InputLost { .. })
        }));
    }
    all.extend(events_for(&rx, Duration::from_secs(1)));
    eprintln!(
        "events checked: {}; connects {:?}; losses {:?}",
        all.len(),
        connects(&all),
        losses(&all)
    );
    assert_eq!(connects(&all), vec![1, 2, 3]);
    assert_eq!(
        losses(&all),
        vec![
            (1, LossReason::Error),
            (2, LossReason::Error),
            (3, LossReason::Error)
        ]
    );
    assert_eq!(input.builds(), 3);
    input.stop();
    let end = Instant::now() + Duration::from_secs(5);
    while input.teardowns_in_flight() > 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        input.teardowns_in_flight(),
        0,
        "every retired pipeline was torn down"
    );
}

/// Nothing listens: the connect fails inside SRT (its own 3 s connect timeout, F-012) and the
/// generation is lost as `Error`, with no `InputConnected` and no frame.
#[test]
fn a_source_that_never_answers_is_lost_as_an_error() {
    let _s = serial();
    let (mut input, rx, frames, _lines) = input(&P720);
    let t0 = Instant::now();
    input.rebuild(1, &spec(free_port()));
    let seen = events_until(&rx, Duration::from_secs(8), |e| {
        matches!(e, Event::InputLost { .. })
    });
    let took = t0.elapsed();
    eprintln!("lost after {took:?}; events {seen:?}");
    assert_eq!(losses(&seen), vec![(1, LossReason::Error)]);
    assert!(connects(&seen).is_empty() && frames_in(&seen) == 0);
    assert!(frames.camera().is_none());
}

/// A listener that completes the handshake and sends nothing (F-002's silent CF socket): the adapter
/// reports nothing at all. It has no watchdog; the domain's connect timeout owns this case.
#[test]
fn a_connected_but_silent_source_reports_nothing() {
    let _s = serial();
    let port = free_port();
    let l = Listener::silent(port);
    let (mut input, rx, frames, _lines) = input(&P720);
    input.rebuild(1, &spec(port));
    let window = Duration::from_millis(t().input_connect_timeout_ms + 1_000);
    let seen = events_for(&rx, window);
    let callers = l.callers.load(Ordering::SeqCst);
    eprintln!("callers that completed the handshake: {callers}; events in {window:?}: {seen:?}");
    assert!(
        callers >= 1,
        "the handshake completed, so the silence is the source's"
    );
    assert!(seen.is_empty(), "{seen:?}");
    assert!(frames.camera().is_none());
}

/// The EOS and error paths each map to their own reason, and only the first of them counts for a
/// generation: an error after an EOS, or an EOS after an error, is not a second loss.
#[test]
fn eos_and_error_are_told_apart_once_a_generation() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, _frames, _lines) = input(&P720);
    let mut checked = 0;
    for (gen, first) in [(1u64, LossReason::Eos), (2, LossReason::Error)] {
        connect(&mut input, &rx, gen, &spec(l.port));
        let planted = match first {
            LossReason::Eos => input.plant_eos(),
            _ => input.plant_error("planted"),
        };
        assert!(planted);
        let seen = events_until(&rx, LOSS_BUDGET, |e| matches!(e, Event::InputLost { .. }));
        assert_eq!(losses(&seen), vec![(gen, first)]);
        let second = match first {
            LossReason::Eos => input.plant_error("after the eos"),
            _ => input.plant_eos(),
        };
        assert!(second);
        let after = events_for(&rx, Duration::from_secs(1));
        assert!(losses(&after).is_empty(), "gen {gen}: {after:?}");
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// After the adapter reports a generation lost, nothing of it reaches the slot or the loop, though its
/// pipeline (a planted error leaves it running) keeps delivering: the gate drops those samples.
#[test]
fn a_lost_generation_writes_nothing_more() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    events_for(&rx, Duration::from_millis(300));
    assert!(input.plant_error("planted"));
    events_until(&rx, LOSS_BUDGET, |e| matches!(e, Event::InputLost { .. }));
    let pts = camera_pts(&frames);
    let dropped = input.gate_dropped();
    let emptied = frames.take_audio(frames.audio_level()).len();
    let after = events_for(&rx, Duration::from_secs(1));
    let gate = input.gate_dropped() - dropped;
    let ring = frames.audio_level();
    eprintln!(
        "events after the loss: {after:?}; ring {emptied} bytes before, {ring} after; samples dropped \
         at the gate: {gate}"
    );
    assert!(emptied > 0, "audio reached the ring before the loss");
    assert!(after.is_empty(), "{after:?}");
    assert_eq!(
        camera_pts(&frames),
        pts,
        "the slot keeps the last frame before the loss"
    );
    assert_eq!(ring, 0, "no audio after the loss");
    assert!(gate > 0, "the pipeline kept delivering, so the gate was tested");
}

/// The video decoder hangs for 3 s: the audio keeps reaching the ring throughout, because the video
/// queue before the decoder is leaky and drops its oldest buffer rather than holding up the demuxer
/// (a blocking queue full of 1 s of video stops `tsdemux`, and the audio with it, about 1 s in).
#[test]
fn a_hung_video_decoder_never_stalls_the_audio() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    assert!(input.hang_decoder(Duration::from_secs(3)), "the hook");
    let t0 = Instant::now();
    thread::sleep(Duration::from_millis(1_500));
    let pts = camera_pts(&frames);
    let emptied = frames.take_audio(frames.audio_level()).len();
    thread::sleep(Duration::from_millis(1_200));
    let ring = frames.audio_level();
    let still = camera_pts(&frames) == pts;
    eprintln!(
        "{:?} into the hang: ring {ring} bytes since {emptied} were taken; video held: {still}",
        t0.elapsed()
    );
    assert!(still, "the hang held the video, so the test is the one it says");
    assert!(ring > 0, "audio arrived while the video decoder hung");
}

/// The pipeline is built with the bounds the plan and the brief state (written out here from their
/// text): leaky queues of 1 s (video) and 400 ms (audio) that drop the oldest; appsinks of two buffers
/// that drop and never sync; the SRT latency `Thresholds::input_srt_latency_ms`; and no reconnecting
/// inside `srtsrc` (the domain owns it).
#[test]
fn the_pipeline_is_built_with_the_plans_bounds() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, _frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    let p = input.pipeline().expect("a pipeline");
    let el = |n: &str| p.by_name(n).unwrap_or_else(|| panic!("no element {n}"));
    let leaky = |n: &str| {
        let v = el(n).property_value("leaky");
        let (_, value) = glib_enum(&v);
        value
    };
    let mut checked = 0;
    for (queue, ns) in [("vqueue", 1_000_000_000u64), ("aqueue", 400_000_000)] {
        assert_eq!(leaky(queue), "downstream", "{queue}");
        assert_eq!(el(queue).property::<u64>("max-size-time"), ns, "{queue}");
        checked += 1;
    }
    for sink in ["vsink", "asink"] {
        let s = el(sink);
        assert_eq!(s.property::<u32>("max-buffers"), 2, "{sink}");
        assert!(s.property::<bool>("drop"), "{sink}");
        assert!(!s.property::<bool>("sync"), "{sink}");
        checked += 1;
    }
    let src = el("src");
    assert!(!src.property::<bool>("auto-reconnect"));
    assert_eq!(
        u64::from(src.property::<i32>("latency").unsigned_abs()),
        t().input_srt_latency_ms
    );
    checked += 1;
    eprintln!("elements checked: {checked}");
    assert_eq!(checked, 5);
}

fn glib_enum(v: &glib::Value) -> (i32, String) {
    let e = glib::EnumValue::from_value(v).expect("an enum value").1;
    (e.value(), e.nick().to_owned())
}

/// A camera with non-square pixels (PAL 4:3, 720×576 at 16:15) is scaled to square pixels at its
/// display aspect: 4:3 in 1280×720 is 960×720, pillarboxed by 160 px each side.
#[test]
fn non_square_pixels_are_scaled_to_square_and_pillarboxed() {
    let _s = serial();
    let mut c = cfg(720, 576);
    c.pattern = "white";
    c.par = Some((16, 15));
    let l = Listener::new(&c);
    let (mut input, rx, frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    thread::sleep(Duration::from_millis(500));
    let mut checked = 0;
    for _ in 0..5 {
        assert_programme_shape(&frames, &P720, "PAL");
        assert!(luma(&frames, 80, 360) < BLACK_LUMA, "left border");
        assert!(luma(&frames, 1_200, 360) < BLACK_LUMA, "right border");
        assert!(
            luma(&frames, 200, 360) > WHITE_LUMA,
            "the picture starts by x=160"
        );
        assert!(luma(&frames, 640, 360) > WHITE_LUMA, "centre");
        checked += 1;
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(checked, 5);
}

/// `srtsrc` refuses to start with a passphrase under 10 characters (libsrt's floor), in
/// `set_state` itself (F-012): the generation is lost as `Error`, sooner than any connect would fail.
#[test]
fn a_start_srt_refuses_is_lost_as_an_error() {
    let _s = serial();
    let (mut input, rx, _frames, lines) = input(&P720);
    let t0 = Instant::now();
    input.rebuild(
        1,
        &InputSpec {
            passphrase: "short".to_owned(),
            ..spec(free_port())
        },
    );
    let seen = events_until(&rx, Duration::from_secs(8), |e| {
        matches!(e, Event::InputLost { .. })
    });
    let took = t0.elapsed();
    let more = events_for(&rx, Duration::from_millis(500));
    let lines = lines.lock().unwrap().clone();
    eprintln!("lost after {took:?}; lines {lines:?}");
    assert_eq!(losses(&seen), vec![(1, LossReason::Error)]);
    assert!(more.is_empty(), "one loss: {more:?}");
    assert!(
        took < Duration::from_secs(2),
        "{took:?}: before SRT's own 3 s connect timeout"
    );
    assert!(lines.iter().any(|l| l.contains("did not start")), "{lines:?}");
}

/// The loop is stalled and its channel full: frames still replace the camera slot (the newest wins),
/// the audio ring stays at its 400 ms cap, the dropped events are counted, and nothing blocks. Once
/// the channel has room, the generation's `InputConnected` is delivered.
#[test]
fn a_full_inbound_channel_never_holds_the_camera_slot() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (tx, rx) = crossbeam_channel::bounded(1);
    tx.try_send(Inbound::Event(Event::Tick))
        .expect("fills the channel");
    let (mut input, frames, lines) = input_with(tx, &P720);
    input.rebuild(1, &spec(l.port));
    let end = Instant::now() + connect_budget();
    while frames.camera().is_none() {
        assert!(Instant::now() < end, "no frame reached the slot");
        thread::sleep(Duration::from_millis(20));
    }
    let mut stamps = Vec::new();
    for _ in 0..5 {
        thread::sleep(Duration::from_millis(200));
        stamps.push(camera_pts(&frames));
    }
    eprintln!(
        "slot stamps {stamps:?}; events dropped {}; ring {} bytes",
        input.frames_dropped(),
        frames.audio_level()
    );
    assert!(
        stamps.windows(2).all(|w| w[1] > w[0]),
        "the slot keeps taking the newest frame"
    );
    assert!(input.frames_dropped() > 0);
    assert_eq!(
        frames.audio_level(),
        AUDIO_RING_BYTES,
        "the ring is full and bounded"
    );
    let logged = lines
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains("channel is full"))
        .count();
    assert_eq!(logged, 1, "one line for the run of drops");
    // Room again: the generation's connect is delivered, once.
    assert_eq!(rx.recv().ok(), Some(Inbound::Event(Event::Tick)));
    let seen = events_until(&rx, Duration::from_secs(2), |e| {
        *e == Event::InputConnected { gen: 1 }
    });
    let more = events_for(&rx, Duration::from_millis(500));
    assert!(connects(&more).is_empty(), "{more:?}");
    assert!(!seen.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The caller's thread never waits

/// `rebuild` returns at once while the start is held (a resolver that hangs: F-012 measured 10 s on
/// `set_state` with an unanswered DNS), and the generation connects once the start goes ahead.
#[test]
fn rebuild_never_waits_on_the_start() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, _frames, _lines) = input(&P720);
    input.delay_starts(Duration::from_secs(2));
    let t0 = Instant::now();
    input.rebuild(1, &spec(l.port));
    let took = t0.elapsed();
    eprintln!("rebuild returned in {took:?}");
    assert!(took < Duration::from_millis(200), "{took:?}");
    events_until(&rx, connect_budget() + Duration::from_secs(2), |e| {
        *e == Event::InputConnected { gen: 1 }
    });
}

/// C08 amendment 2026-10-19: the adapter drops within 500 ms while the pipeline it owns is hung, and
/// a rebuild over a hung pipeline returns as fast.
#[test]
fn drop_returns_within_500_ms_while_the_pipeline_hangs() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, _frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    let hold = Duration::from_secs(3);
    assert!(input.hang_decoder(hold), "the hook");
    thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    input.rebuild(2, &spec(l.port));
    let rebuilt = t0.elapsed();
    assert!(rebuilt < Duration::from_millis(500), "rebuild {rebuilt:?}");
    events_until(&rx, connect_budget(), |e| *e == Event::InputConnected { gen: 2 });
    assert!(input.hang_decoder(hold), "the hook");
    thread::sleep(Duration::from_millis(300));
    let teardowns = input.teardown_counter();
    let t0 = Instant::now();
    drop(input);
    let took = t0.elapsed();
    let in_flight = teardowns.load(Ordering::SeqCst);
    eprintln!("rebuild took {rebuilt:?}, drop took {took:?}; teardowns in flight after it: {in_flight}");
    assert!(took < Duration::from_millis(500), "{took:?}");
    assert!(
        in_flight >= 1,
        "a teardown is still held by the hang, so the hang was real"
    );
    let end = Instant::now() + Duration::from_secs(10);
    while teardowns.load(Ordering::SeqCst) > 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        teardowns.load(Ordering::SeqCst),
        0,
        "the reapers finish once the hang ends"
    );
}

// ---------------------------------------------------------------------------------------------
// Refusals and the end of the session

/// `stop` ends the input for good: no event or frame after it, a second `stop` changes nothing, and a
/// later `rebuild` is refused and counted, with no pipeline and no event.
#[test]
fn stop_closes_the_gate_and_refuses_later_rebuilds() {
    let _s = serial();
    let l = Listener::new(&cfg(640, 360));
    let (mut input, rx, frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    // The stopped pipeline keeps delivering for 1.5 s (its teardown is held), so the gate is tested.
    input.delay_teardowns(Duration::from_millis(1_500));
    let dropped = input.gate_dropped();
    input.stop();
    // Whatever the channel holds was sent before `stop` returned: the gate closes under the lock
    // each send is made under.
    let before = rx.try_iter().count();
    let pts = camera_pts(&frames);
    input.stop();
    input.rebuild(2, &spec(l.port));
    let after = events_for(&rx, Duration::from_secs(1));
    let gate = input.gate_dropped() - dropped;
    eprintln!("events queued before stop returned: {before}; after it: {after:?}; gate drops {gate}");
    assert!(after.is_empty(), "{after:?}");
    assert!(
        gate > 0,
        "the stopped pipeline delivered while held, and the gate dropped it"
    );
    assert_eq!(camera_pts(&frames), pts);
    assert_eq!(input.refused_rebuilds(), 1);
    assert_eq!(input.builds(), 1);
}

/// An input URL that is not `srt://host:port` (here, one carrying its own query) is lost at once as
/// `Error`, with no pipeline built and nothing of the query logged.
#[test]
fn a_malformed_url_is_a_loss_without_a_pipeline() {
    let _s = serial();
    let (mut input, rx, _frames, lines) = input(&P720);
    let mut checked = 0;
    for (gen, url) in [
        (1u64, "srt://127.0.0.1:9?streamid=SECRETSTREAM9"),
        (2, "srt://127.0.0.1"),
        (3, "rtmp://127.0.0.1:9"),
    ] {
        let s = InputSpec {
            url: url.to_owned(),
            ..spec(9)
        };
        input.rebuild(gen, &s);
        let seen = events_until(&rx, Duration::from_millis(200), |e| {
            matches!(e, Event::InputLost { .. })
        });
        assert_eq!(losses(&seen), vec![(gen, LossReason::Error)]);
        checked += 1;
    }
    assert_eq!(input.builds(), 0);
    let lines = lines.lock().unwrap().clone();
    assert!(lines.iter().all(|l| !l.contains("SECRETSTREAM9")), "{lines:?}");
    assert_eq!(
        lines.iter().filter(|l| l.contains("refused")).count(),
        3,
        "{lines:?}"
    );
    assert_eq!(checked, 3);
}

// ---------------------------------------------------------------------------------------------
// Credentials end to end, and the real consumer

/// An encrypted listener, reached through the encoded URI: the passphrase and the stream id carry `&`,
/// `=`, `/`, `+` and a space, so a URI that did not encode them would cut both at the first `&` and the
/// handshake would fail. The listener records the stream id it was presented, which must be the
/// adapter's, byte for byte.
#[test]
fn an_encrypted_listener_is_reached_through_the_encoded_uri() {
    let _s = serial();
    let passphrase = "pa&ss=w/rd+12 34".to_owned();
    let mut c = cfg(640, 360);
    c.passphrase = Some(passphrase.clone());
    let l = Listener::new(&c);
    let (mut input, rx, _frames, _lines) = input(&P720);
    let s = InputSpec {
        url: format!("srt://127.0.0.1:{}", l.port),
        stream_id: "play=uid&x/y+z 1".to_owned(),
        passphrase,
    };
    connect(&mut input, &rx, 1, &s);
    let ids = l.stream_ids.lock().unwrap().clone();
    eprintln!("stream ids presented: {}", ids.len());
    assert_eq!(ids, vec![s.stream_id.clone()]);
}

/// The seam end to end: the input's camera, read by the real programme pipeline from the shared frame
/// store, is what goes on air. The listener sends solid blue; the programme's file shows blue.
#[test]
fn the_programme_puts_the_input_on_air() {
    let _s = serial();
    let mut c = cfg(640, 480);
    c.pattern = "blue";
    let l = Listener::new(&c);
    let (mut input, rx, frames, _lines) = input(&P720);
    connect(&mut input, &rx, 1, &spec(l.port));
    let (ptx, prx) = crossbeam_channel::bounded(8_192);
    let mut p = GstProgramme::new(frames.clone(), ptx);
    p.start(&P720).expect("programme");
    let dir = std::env::temp_dir().join(format!("c10-on-air-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("p.flv");
    p.swap_sink(
        1,
        &TargetSpec {
            url: format!("file://{}", file.display()),
            stream_key: String::new(),
        },
    );
    let step = Duration::from_nanos(1_000_000_000 / u64::from(P720.fps));
    let t0 = Instant::now();
    for i in 0..60u64 {
        if let Some(wait) = (t0 + step * i as u32).checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        p.push(i, FrameChoice::Emit);
    }
    p.drain();
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        match prx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(Inbound::Event(Event::OutputDrained)) => break,
            Ok(_) => {}
            Err(_) => panic!("the programme never drained"),
        }
    }
    let missing = p.camera_missing();
    drop(p);
    let centres = decode_centres(&file);
    let _ = std::fs::remove_dir_all(&dir);
    let blue = centres
        .iter()
        .filter(|c| c[2] > 180 && c[0] < 60 && c[1] < 60)
        .count();
    eprintln!(
        "programme frames decoded: {}; blue at the centre: {blue}; camera missing: {missing}",
        centres.len()
    );
    assert!(centres.len() >= 50, "{} frames", centres.len());
    assert_eq!(missing, 0, "every Emit found a camera frame");
    assert_eq!(blue, centres.len(), "every frame on air is the camera's");
}

/// The centre pixel (RGB) of every frame in a programme file.
fn decode_centres(file: &Path) -> Vec<[u8; 3]> {
    let desc = format!(
        "filesrc location={} ! flvdemux name=d d.video ! queue ! h264parse ! avdec_h264 ! videoconvert \
         ! video/x-raw,format=RGB ! appsink name=s sync=false",
        file.display()
    );
    let pipeline = gst::parse::launch(&desc)
        .expect("decoder")
        .downcast::<gst::Pipeline>()
        .expect("a pipeline");
    let sink = pipeline
        .by_name("s")
        .expect("appsink")
        .downcast::<gst_app::AppSink>()
        .expect("an appsink");
    let out: Arc<Mutex<Vec<[u8; 3]>>> = Arc::default();
    let seen = Arc::clone(&out);
    let count = Arc::new(AtomicU64::new(0));
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let info = gst_video::VideoInfo::from_caps(sample.caps().ok_or(gst::FlowError::Error)?)
                    .map_err(|_| gst::FlowError::Error)?;
                let buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
                let frame = gst_video::VideoFrame::from_buffer_readable(buffer, &info)
                    .map_err(|_| gst::FlowError::Error)?;
                let stride = frame.plane_stride()[0] as usize;
                let (x, y) = (info.width() as usize / 2, info.height() as usize / 2);
                let data = frame.plane_data(0).map_err(|_| gst::FlowError::Error)?;
                let i = y * stride + x * 3;
                seen.lock().unwrap().push([data[i], data[i + 1], data[i + 2]]);
                count.fetch_add(1, Ordering::Relaxed);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline.set_state(gst::State::Playing).expect("decode");
    let bus = pipeline.bus().expect("bus");
    let _ = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(20),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let _ = pipeline.set_state(gst::State::Null);
    let v = out.lock().unwrap().clone();
    v
}
