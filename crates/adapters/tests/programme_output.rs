//! C09: the programme pipeline (never rebuilt) and the swappable output pipeline, against real
//! GStreamer. Every test writes to a `file://` target in its own temp dir and reads the result back
//! through `filesrc ! flvdemux ! …` (`decode`). The tests share GStreamer and the machine's CPU, so
//! each takes `serial()` first: they run one at a time whatever the harness's thread count, and the
//! real-pace timing tests never compete with another test's encoder.
//!
//! Expected values come from the brief, `Thresholds::default()`, `OutputProfile` and the findings
//! (F-004, F-005), never from the adapter.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use adapters::frame_store::{FrameStore, AUDIO_RING_BYTES};
use adapters::gst_output::{
    ENCODED_RING_MAX_ENTRIES, ENCODED_RING_SPAN, OUTPUT_PROGRESS_POLL_MS, OUTPUT_QUEUE_BYTES,
};
use adapters::gst_programme::{GstProgramme, OVERLAY_SIZE};
use adapters::slate::{logo_box, Slate, LOGO_PNG, SLATE_BACKDROP_RGB};
use adapters::stats::StatsSnapshot;
use app::ports::{Inbound, ProgrammeOutput, TargetSpec};
use crossbeam_channel::Receiver;
use domain::config::{OutputProfile, Thresholds, ENCODER_SPEED_REALTIME_MIN};
use domain::ladder::Rung;
use domain::output_link::OutputLink;
use domain::programme::{FrameChoice, SlateKind};
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use sha2::{Digest, Sha256};

const P720: OutputProfile = OutputProfile::P720;
/// The house backdrop, from the rulebook rather than the adapter: seazn.club's `--mk-night`,
/// `#150b36` (`apps/web/src/app/globals.css:535`; the overlay templates spec §4.2).
const HOUSE_NIGHT: [u8; 3] = [0x15, 0x0b, 0x36];
/// The slate on air, decoded from the programme, against `HOUSE_NIGHT`: measured 1 level off it
/// ([22, 11, 54]; x264 codes the flat backdrop), so 2 leaves one level of slack. Left to
/// negotiation the compositor mixes in RGB and the backdrop comes out [18, 7, 51] (F-010), which 2
/// tells apart and the looser 8 did not (mutant G24).
const ON_AIR_TOL: u8 = 2;
/// The brief's pin of the vendored logo (C09 amendment 2026-10-15).
const LOGO_SHA256: &str = "bee2648f76fa2590ac81d2c1e14050fe4c62687b8630437b0d6e6b080c2f90e8";
/// Owed 15, option (a): poll the bytes-sent total at most every 250 ms, keeping the PDF's 5 s.
const OWED_15_POLL_MAX_MS: u64 = 250;
/// F-004: AAC at 48 kHz carries 1 024 samples a frame, 46.875 frames/s, and the verified encode is
/// 128 kbps.
const AAC_FRAMES_PER_S: f64 = 48_000.0 / 1_024.0;
const AAC_KBPS: f64 = 128.0;
/// The brief's A/V tolerance (PDF rank 4): ±40 ms.
const AV_TOLERANCE_MS: u64 = 40;
/// One tick of S16LE 48 kHz stereo at 30 fps: 1 600 frames of 4 bytes.
const TICK_AUDIO_BYTES: usize = 48_000 / 30 * 4;

// ---------------------------------------------------------------------------------------------
// Harness

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A temp dir that removes itself.
struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("c09-{}-{name}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Tmp(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file_target(path: &Path) -> TargetSpec {
    TargetSpec {
        url: format!("file://{}", path.display()),
        stream_key: String::new(),
    }
}

type Lines = Arc<Mutex<Vec<String>>>;

/// A started 720p programme, its inbound receiver, its frame store and its captured log.
fn programme_with(profile: &OutputProfile) -> (GstProgramme, Receiver<Inbound>, FrameStore, Lines) {
    gst::init().expect("gstreamer");
    let frames = FrameStore::default();
    let (tx, rx) = crossbeam_channel::bounded(8_192);
    let lines: Lines = Arc::default();
    let sink = Arc::clone(&lines);
    let mut p = GstProgramme::new(frames.clone(), tx).with_log(Arc::new(move |l: &str| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(l.to_owned());
    }));
    p.start(profile).expect("start");
    (p, rx, frames, lines)
}

fn programme() -> (GstProgramme, Receiver<Inbound>, FrameStore, Lines) {
    programme_with(&P720)
}

fn period(profile: &OutputProfile) -> Duration {
    Duration::from_nanos(1_000_000_000 / u64::from(profile.fps))
}

/// Pushes `range` at the profile's real frame rate, as C08's loop does, calling `before(i)` first.
fn pace_with(
    p: &mut GstProgramme,
    range: std::ops::Range<u64>,
    mut choice: impl FnMut(u64) -> FrameChoice,
    mut before: impl FnMut(u64),
) {
    let step = period(&P720);
    let t0 = Instant::now();
    for (k, i) in range.enumerate() {
        let due = t0 + step * k as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        before(i);
        p.push(i, choice(i));
    }
}

fn pace(p: &mut GstProgramme, range: std::ops::Range<u64>, choice: FrameChoice) {
    pace_with(p, range, |_| choice, |_| {});
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

fn drain_and_wait(p: &mut GstProgramme, rx: &Receiver<Inbound>) -> Vec<Event> {
    p.drain();
    events_until(rx, Duration::from_secs(15), |e| *e == Event::OutputDrained)
}

// ---------------------------------------------------------------------------------------------
// Pictures

type Rgb = [u8; 3];

fn close(a: Rgb, b: Rgb, tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

fn is_blue(p: Rgb) -> bool {
    p[2] > 150 && p[0] < 90 && p[1] < 90
}

fn is_red(p: Rgb) -> bool {
    p[0] > 150 && p[1] < 90 && p[2] < 90
}

fn is_green(p: Rgb) -> bool {
    p[1] > 150 && p[0] < 90 && p[2] < 90
}

/// A `w` × `h` sample of `format`, every pixel `rgba`, converted from RGBA by GStreamer.
fn solid(format: gst_video::VideoFormat, w: u32, h: u32, rgba: [u8; 4]) -> gst::Sample {
    picture(format, w, h, |_, _| rgba)
}

/// A `w` × `h` sample of `format` drawn by `px(x, y) -> rgba`.
fn picture(format: gst_video::VideoFormat, w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 4]) -> gst::Sample {
    let rgba = gst_video::VideoInfo::builder(gst_video::VideoFormat::Rgba, w, h)
        .build()
        .expect("rgba info");
    let stride = rgba.stride()[0] as usize;
    let mut data = vec![0u8; rgba.size()];
    for y in 0..h {
        for x in 0..w {
            let o = y as usize * stride + x as usize * 4;
            data[o..o + 4].copy_from_slice(&px(x, y));
        }
    }
    let src = gst::Buffer::from_mut_slice(data);
    let out_info = gst_video::VideoInfo::builder(format, w, h)
        .build()
        .expect("out info");
    let buffer = if format == gst_video::VideoFormat::Rgba {
        src
    } else {
        let conv = gst_video::VideoConverter::new(&rgba, &out_info, None).expect("converter");
        let in_frame = gst_video::VideoFrame::from_buffer_readable(src, &rgba).expect("in frame");
        let out = gst::Buffer::with_size(out_info.size()).expect("out buffer");
        let mut out_frame = gst_video::VideoFrame::from_buffer_writable(out, &out_info).expect("out frame");
        conv.frame(&in_frame, &mut out_frame);
        out_frame.into_buffer()
    };
    gst::Sample::builder()
        .buffer(&buffer)
        .caps(&out_info.to_caps().expect("caps"))
        .build()
}

/// CEF's overlay: BGRA 1920 × 1080, opaque blue in the top-left 100 × 100, transparent elsewhere.
fn overlay_blue_corner() -> gst::Sample {
    let (w, h) = OVERLAY_SIZE;
    picture(gst_video::VideoFormat::Bgra, w, h, |x, y| {
        if x < 100 && y < 100 {
            [0, 0, 255, 255]
        } else {
            [0, 0, 0, 0]
        }
    })
}

/// BT.709's luma weights (ITU-R BT.709-6, item 3.2), from which the decode matrix is derived.
const BT709_KR: f64 = 0.2126;
const BT709_KB: f64 = 0.0722;

/// An I420 picture, decoded to RGB by the test itself: BT.709 limited range, in floating point and
/// rounded. GStreamer's own 8-bit converter truncates (#150b36 comes back as [16, 7, 49] after one
/// RGB → I420 → RGB trip, F-010), so a reading through it would measure the converter, not the
/// adapter.
struct Pic {
    w: u32,
    h: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Pic {
    fn of(sample: &gst::Sample) -> Pic {
        let info = gst_video::VideoInfo::from_caps(sample.caps().expect("caps")).expect("info");
        assert_eq!(
            info.format(),
            gst_video::VideoFormat::I420,
            "the test decodes I420 only"
        );
        let c = info.colorimetry();
        assert_eq!(
            (c.matrix(), c.range()),
            (
                gst_video::VideoColorMatrix::Bt709,
                gst_video::VideoColorRange::Range16_235
            ),
            "the test decodes BT.709 limited range; the caps say {c}"
        );
        let (w, h) = (info.width(), info.height());
        let frame =
            gst_video::VideoFrame::from_buffer_readable(sample.buffer_owned().expect("buffer"), &info)
                .expect("frame");
        let plane = |n: u32, pw: u32, ph: u32| {
            let stride = frame.plane_stride()[n as usize] as usize;
            let data = frame.plane_data(n).expect("plane");
            let mut packed = Vec::with_capacity((pw * ph) as usize);
            for row in 0..ph as usize {
                packed.extend_from_slice(&data[row * stride..row * stride + pw as usize]);
            }
            packed
        };
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        Pic {
            w,
            h,
            y: plane(0, w, h),
            u: plane(1, cw, ch),
            v: plane(2, cw, ch),
        }
    }

    fn at(&self, x: u32, y: u32) -> Rgb {
        let c = (y / 2 * self.w.div_ceil(2) + x / 2) as usize;
        let luma = (f64::from(self.y[(y * self.w + x) as usize]) - 16.0) * 255.0 / 219.0;
        let (cb, cr) = (
            (f64::from(self.u[c]) - 128.0) * 255.0 / 224.0,
            (f64::from(self.v[c]) - 128.0) * 255.0 / 224.0,
        );
        let kg = 1.0 - BT709_KR - BT709_KB;
        let r = luma + 2.0 * (1.0 - BT709_KR) * cr;
        let b = luma + 2.0 * (1.0 - BT709_KB) * cb;
        let g =
            luma - 2.0 * BT709_KB * (1.0 - BT709_KB) / kg * cb - 2.0 * BT709_KR * (1.0 - BT709_KR) / kg * cr;
        [r, g, b].map(|v| v.round().clamp(0.0, 255.0) as u8)
    }

    /// The share of the logo box's pixels more than 40 away from the backdrop on any channel.
    fn logo_share(&self) -> f64 {
        let (x0, y0, lw, lh) = logo_box(self.w, self.h);
        let mut off = 0usize;
        for y in y0..y0 + lh {
            for x in x0..x0 + lw {
                if !close(self.at(x, y), HOUSE_NIGHT, 40) {
                    off += 1;
                }
            }
        }
        off as f64 / f64::from(lw * lh)
    }
}

// ---------------------------------------------------------------------------------------------
// Reading a target back

/// One decoded frame: its size and the pixels the tests look at.
#[derive(Debug, Clone)]
struct Frame {
    w: u32,
    h: u32,
    centre: Rgb,
    /// (10, 10) and (W − 10, H − 10): the slate's corners.
    corner_tl: Rgb,
    corner_br: Rgb,
    /// (30, 30): inside the overlay's blue 100 × 100 at 1080, 66 × 66 at 720.
    spot: Rgb,
    /// (20, H / 2): the pillarbox border of a 4:3 camera.
    side: Rgb,
    logo_share: f64,
}

#[derive(Debug, Default)]
struct Decoded {
    video_pts_ms: Vec<u64>,
    keys: Vec<bool>,
    /// What h264parse passed on, before the decoder: (PTS ms, keyframe).
    parsed: Vec<(u64, bool)>,
    frames: Vec<Frame>,
    /// The raw AAC payload (flvdemux's access units) and their count.
    aac: Vec<u8>,
    aac_frames: usize,
    audio_pts_ms: Vec<u64>,
    pcm: Vec<i16>,
}

impl Decoded {
    fn first_is_key(&self) -> bool {
        self.keys.first().copied().unwrap_or(false)
    }

    fn audio_ms(&self) -> u64 {
        (self.aac_frames as u64 * 1_024 * 1_000) / 48_000
    }

    fn zero_share(&self) -> f64 {
        self.aac.iter().filter(|&&b| b == 0).count() as f64 / self.aac.len().max(1) as f64
    }

    fn aac_kbps(&self) -> f64 {
        self.aac.len() as f64 * 8.0 / 1_000.0 / (self.audio_ms().max(1) as f64 / 1_000.0)
    }

    /// RMS in dBFS of the decoded stereo PCM between `from_ms` and `to_ms`.
    fn rms_dbfs(&self, from_ms: u64, to_ms: u64) -> f64 {
        let a = (from_ms * 48 * 2) as usize;
        let b = ((to_ms * 48 * 2) as usize).min(self.pcm.len());
        let s = &self.pcm[a.min(b)..b];
        let rms = (s.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / s.len().max(1) as f64).sqrt();
        20.0 * (rms.max(1e-9) / f64::from(i16::MAX)).log10()
    }
}

fn decode(path: &Path) -> Decoded {
    let launch = format!(
        "filesrc location={} ! flvdemux name=d \
         d.video ! queue ! h264parse name=hp ! avdec_h264 ! video/x-raw,format=I420 ! appsink name=v sync=false \
         d.audio ! queue ! identity name=araw ! avdec_aac ! audioconvert ! audioresample ! \
         audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved ! appsink name=a sync=false",
        path.display()
    );
    let pipeline = gst::parse::launch(&launch)
        .expect("decode pipeline")
        .downcast::<gst::Pipeline>()
        .expect("pipeline");
    let out = Arc::new(Mutex::new(Decoded::default()));

    let hp = pipeline.by_name("hp").expect("hp");
    let o = Arc::clone(&out);
    hp.static_pad("src")
        .expect("hp src")
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(b) = info.buffer() {
                let key = !b.flags().contains(gst::BufferFlags::DELTA_UNIT);
                let mut d = o.lock().unwrap();
                d.keys.push(key);
                d.parsed.push((b.pts().map_or(u64::MAX, |p| p.mseconds()), key));
            }
            gst::PadProbeReturn::Ok
        });
    let araw = pipeline.by_name("araw").expect("araw");
    let o = Arc::clone(&out);
    araw.static_pad("src")
        .expect("araw src")
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(b) = info.buffer() {
                if let Ok(m) = b.map_readable() {
                    let mut d = o.lock().unwrap();
                    d.aac.extend_from_slice(&m);
                    d.aac_frames += 1;
                    d.audio_pts_ms.push(b.pts().map_or(u64::MAX, |p| p.mseconds()));
                }
            }
            gst::PadProbeReturn::Ok
        });
    let v = pipeline
        .by_name("v")
        .expect("v")
        .downcast::<gst_app::AppSink>()
        .expect("appsink");
    let o = Arc::clone(&out);
    v.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let pic = Pic::of(&sample);
                let (w, h) = (pic.w, pic.h);
                let pts = sample.buffer().and_then(|b| b.pts()).map_or(0, |p| p.mseconds());
                let f = Frame {
                    w,
                    h,
                    centre: pic.at(w / 2, h / 2),
                    corner_tl: pic.at(10, 10),
                    corner_br: pic.at(w - 10, h - 10),
                    spot: pic.at(30, 30),
                    side: pic.at(20, h / 2),
                    logo_share: pic.logo_share(),
                };
                let mut d = o.lock().unwrap();
                d.video_pts_ms.push(pts);
                d.frames.push(f);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let a = pipeline
        .by_name("a")
        .expect("a")
        .downcast::<gst_app::AppSink>()
        .expect("appsink");
    let o = Arc::clone(&out);
    a.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(b) = sample.buffer() {
                    if let Ok(m) = b.map_readable() {
                        let mut d = o.lock().unwrap();
                        d.pcm
                            .extend(m.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])));
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline.set_state(gst::State::Playing).expect("decode playing");
    let bus = pipeline.bus().expect("bus");
    let msg = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(30),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let _ = pipeline.set_state(gst::State::Null);
    match msg.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => {}
        other => panic!("decoding {} did not finish: {other:?}", path.display()),
    }
    drop(pipeline);
    let d = std::mem::take(&mut *out.lock().unwrap());
    d
}

// ---------------------------------------------------------------------------------------------
// The brief's tests (Step 3, amended)

#[test]
fn ninety_slate_ticks_give_ninety_frames_at_constant_spacing() {
    let _s = serial();
    let dir = Tmp::new("ninety");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..90, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 90, "frames");
    let expected = (1_000.0 / f64::from(P720.fps)).round() as u64;
    let deltas: Vec<u64> = d.video_pts_ms.windows(2).map(|w| w[1] - w[0]).collect();
    eprintln!("pts deltas checked: {}", deltas.len());
    assert_eq!(deltas.len(), 89);
    for (i, &dt) in deltas.iter().enumerate() {
        assert!(
            dt.abs_diff(expected) <= 1,
            "delta {i}: {dt} ms, want {expected} ± 1"
        );
    }
    assert!(
        d.audio_ms().abs_diff(3_000) <= AV_TOLERANCE_MS,
        "audio {} ms against 3 000 ± {AV_TOLERANCE_MS}",
        d.audio_ms()
    );
    // The track starts with the picture: the first AAC frame is the keyframe's, within one frame
    // (1 024 samples, 21 ms) of it.
    let first_audio = *d.audio_pts_ms.first().expect("audio");
    eprintln!("first video pts 0 ms, first audio pts {first_audio} ms");
    assert!(
        first_audio <= 1_024 * 1_000 / 48_000,
        "audio starts {first_audio} ms after the picture"
    );
}

#[test]
fn emit_uses_the_latest_camera_frame() {
    let _s = serial();
    let dir = Tmp::new("emit");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    frames.set_camera(solid(gst_video::VideoFormat::Bgra, 1_280, 720, [0, 255, 0, 255]));
    pace(&mut p, 0..10, FrameChoice::Emit);
    // The latest frame wins: red replaces green.
    frames.set_camera(solid(gst_video::VideoFormat::I420, 1_280, 720, [255, 0, 0, 255]));
    pace(&mut p, 10..20, FrameChoice::Emit);
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 20);
    assert!(is_green(d.frames[9].centre), "{:?}", d.frames[9].centre);
    assert!(is_red(d.frames[19].centre), "{:?}", d.frames[19].centre);
    assert_eq!(p.camera_missing(), 0);
}

#[test]
fn camera_of_a_different_size_is_letterboxed_not_renegotiated() {
    let _s = serial();
    let dir = Tmp::new("letterbox");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    let i420 = gst_video::VideoFormat::I420;
    frames.set_camera(solid(i420, 640, 360, [255, 0, 0, 255]));
    pace(&mut p, 0..10, FrameChoice::Emit);
    frames.set_camera(solid(i420, 1_920, 1_080, [0, 255, 0, 255]));
    pace(&mut p, 10..20, FrameChoice::Repeat);
    // A 4:3 camera (a phone rotated, or a transcode change) is pillarboxed: 960 × 720, centred.
    frames.set_camera(solid(i420, 640, 480, [0, 0, 255, 255]));
    pace(&mut p, 20..30, FrameChoice::Emit);
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 30);
    let checked = d.frames.iter().filter(|f| (f.w, f.h) == (1_280, 720)).count();
    eprintln!("frames at 1280x720: {checked}");
    assert_eq!(checked, 30, "every frame keeps the programme's caps");
    assert!(is_red(d.frames[9].centre));
    assert!(is_green(d.frames[19].centre));
    assert!(is_blue(d.frames[29].centre));
    assert!(
        close(d.frames[29].side, [0, 0, 0], 24),
        "pillarbox border {:?}",
        d.frames[29].side
    );
    assert!(
        close(d.frames[9].side, [255, 0, 0], 90) || is_red(d.frames[9].side),
        "16:9 fills the frame"
    );
    assert_eq!(p.builds(), 1);
}

#[test]
fn overlay_is_composited_on_top_and_hide_clears_it() {
    let _s = serial();
    let dir = Tmp::new("overlay");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    frames.set_overlay(overlay_blue_corner());
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    p.hide_overlay();
    pace(&mut p, 15..30, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 30);
    assert!(is_blue(d.frames[14].spot), "{:?}", d.frames[14].spot);
    assert!(!is_blue(d.frames[29].spot), "{:?}", d.frames[29].spot);
    assert!(
        close(d.frames[29].spot, HOUSE_NIGHT, ON_AIR_TOL),
        "{:?}",
        d.frames[29].spot
    );
}

#[test]
fn half_rate_rung_halves_overlay_pushes() {
    let _s = serial();
    let (mut p, _rx, _f, _l) = programme();
    p.step_down(Rung::OverlayHalfRate);
    for i in 0..60 {
        p.push(i, FrameChoice::Slate(SlateKind::Waiting));
    }
    assert_eq!(p.overlay_pushes(), 30);
}

#[test]
fn swap_sink_starts_new_file_at_keyframe_without_rebuilding_programme() {
    let _s = serial();
    let dir = Tmp::new("swap");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..60, FrameChoice::Slate(SlateKind::Waiting));
    p.swap_sink(2, &file_target(&dir.file("b.flv")));
    pace(&mut p, 60..120, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let b = decode(&dir.file("b.flv"));
    assert!(b.frames.len() >= 50, "b frames {}", b.frames.len());
    assert!(b.first_is_key(), "b starts at a keyframe");
    assert_eq!(b.video_pts_ms.first(), Some(&0), "b's PTS is rebased to 0");
    let a = decode(&dir.file("a.flv"));
    assert!(a.frames.len() >= 50, "a frames {}", a.frames.len());
    assert!(a.first_is_key());
    assert_eq!(p.builds(), 1);
}

#[test]
fn unreachable_rtmp_target_reports_output_error_with_its_generation() {
    let _s = serial();
    let (mut p, rx, _f, _l) = programme();
    let target = TargetSpec {
        url: "rtmp://127.0.0.1:9/live".to_owned(),
        stream_key: "SECRETKEY-unreachable".to_owned(),
    };
    p.swap_sink(3, &target);
    let seen = events_until(&rx, Duration::from_secs(6), |e| {
        matches!(e, Event::OutputError { .. })
    });
    assert_eq!(seen.last(), Some(&Event::OutputError { gen: 3 }));
    // Once per generation: the domain's swap is the answer, and the dead sink says nothing more.
    let later = events_for(&rx, Duration::from_millis(1_000));
    let errors = later
        .iter()
        .filter(|e| matches!(e, Event::OutputError { .. }))
        .count();
    assert_eq!(errors, 0, "{later:?}");
    let t = Instant::now();
    let mut worst = Duration::ZERO;
    for i in 0..30 {
        let one = Instant::now();
        p.push(i, FrameChoice::Slate(SlateKind::Reconnecting));
        worst = worst.max(one.elapsed());
    }
    let took = t.elapsed();
    eprintln!("30 pushes after the error: {took:?}, worst {worst:?}");
    assert!(took < Duration::from_millis(1_500), "{took:?}");
    assert!(worst < period(&P720), "a push blocked for {worst:?}");
}

/// A failed output's own EOS is not a drain. Each appsrc whose push fails sends an EOS downstream,
/// so a sink that dies mid-stream posts its error and then an EOS (F-011's live legs: `OutputError`,
/// then `OutputDrained` 34 ms later with no drain asked for). That EOS neither samples progress after
/// the error nor spends the once-only `OutputDrained`, so the session's real drain, on the next
/// sink, still reports.
#[test]
fn a_failed_output_neither_reports_drained_nor_spends_the_drain() {
    let _s = serial();
    let dir = Tmp::new("failed-then-drain");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    assert!(p.fail_sink_writes());
    pace(&mut p, 15..45, FrameChoice::Slate(SlateKind::Waiting));
    let after = events_for(&rx, Duration::from_millis(1_500));
    let error_at = after
        .iter()
        .position(|e| *e == Event::OutputError { gen: 1 })
        .unwrap_or_else(|| panic!("no OutputError: {after:?}"));
    let drained = after.iter().filter(|e| **e == Event::OutputDrained).count();
    let progress = after[error_at..]
        .iter()
        .filter(|e| matches!(e, Event::OutputProgress { gen: 1, .. }))
        .count();
    eprintln!(
        "after the sink failed: {} events, {drained} drained, {progress} gen-1 progress after the error",
        after.len()
    );
    assert_eq!(drained, 0, "{after:?}");
    assert_eq!(progress, 0, "{after:?}");

    p.swap_sink(2, &file_target(&dir.file("b.flv")));
    pace(&mut p, 45..75, FrameChoice::Slate(SlateKind::Waiting));
    let seen = drain_and_wait(&mut p, &rx);
    let mut drained = seen.iter().filter(|e| **e == Event::OutputDrained).count();
    drained += events_for(&rx, Duration::from_millis(500))
        .iter()
        .filter(|e| **e == Event::OutputDrained)
        .count();
    eprintln!("the real drain: {drained} drained");
    assert_eq!(drained, 1);
    let d = decode(&dir.file("b.flv"));
    eprintln!("frames on the second sink: {}", d.frames.len());
    assert!(
        d.first_is_key() && !d.frames.is_empty(),
        "{} frames",
        d.frames.len()
    );
}

/// Both shapes of key: one after `/live/` (the pattern rule catches it) and one after another path,
/// which only the scrub by value catches.
#[test]
fn errors_and_logs_never_contain_the_stream_key() {
    let _s = serial();
    let (mut p, rx, _f, lines) = programme();
    let keys = ["SECRETKEY-alpha-1234", "SECRETKEY/bravo+5678"];
    p.swap_sink(
        1,
        &TargetSpec {
            url: "rtmp://127.0.0.1:9/live/".to_owned(),
            stream_key: keys[0].to_owned(),
        },
    );
    events_until(&rx, Duration::from_secs(6), |e| {
        *e == Event::OutputError { gen: 1 }
    });
    p.swap_sink(
        2,
        &TargetSpec {
            url: "rtmp://127.0.0.1:9/app".to_owned(),
            stream_key: keys[1].to_owned(),
        },
    );
    events_until(&rx, Duration::from_secs(6), |e| {
        *e == Event::OutputError { gen: 2 }
    });
    // A refused scheme logs its target too.
    p.swap_sink(
        3,
        &TargetSpec {
            url: format!("srt://127.0.0.1:9?streamid={}", keys[0]),
            stream_key: keys[1].to_owned(),
        },
    );
    events_until(&rx, Duration::from_secs(6), |e| {
        *e == Event::OutputError { gen: 3 }
    });
    let debug = format!("{p:?}");
    let lines = lines.lock().unwrap().clone();
    let errors = lines.iter().filter(|l| l.contains("error")).count();
    eprintln!("log lines checked: {}, error lines: {errors}", lines.len());
    assert!(errors >= 2, "both failed sinks logged their error: {lines:?}");
    // Each generation names its sink, so the key reached the log path and the scrub is what hid it.
    for (gen, shown) in [
        (1, "rtmp://127.0.0.1:9/live/****"),
        (2, "rtmp://127.0.0.1:9/app/****"),
    ] {
        let named = format!("output gen={gen}: sink {shown}");
        assert!(lines.contains(&named), "{named:?} not in {lines:?}");
    }
    let forms: Vec<String> = keys
        .iter()
        .flat_map(|k| {
            [
                k.to_string(),
                k.replace('/', "%2F").replace('+', "%2B"),
                "SECRETKEY".to_owned(),
            ]
        })
        .collect();
    for text in lines.iter().chain([&debug]) {
        for f in &forms {
            assert!(!text.contains(f.as_str()), "{f:?} leaked in {text:?}");
        }
    }
}

/// Owed 13 replaced `OutputConnected`/`OutputAck` with `OutputProgress`: a file target reports the
/// growing total with its generation.
#[test]
fn file_target_reports_progress_with_its_generation() {
    let _s = serial();
    let dir = Tmp::new("progress-gen");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(7, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..45, FrameChoice::Slate(SlateKind::Waiting));
    let seen = events_for(&rx, Duration::from_millis(600));
    let progress: Vec<(u64, u64)> = seen
        .iter()
        .filter_map(|e| match e {
            Event::OutputProgress { gen, total_bytes } => Some((*gen, *total_bytes)),
            _ => None,
        })
        .collect();
    eprintln!("progress events read: {}", progress.len());
    assert!(progress.len() >= 4, "{progress:?}");
    assert!(progress.iter().all(|&(g, _)| g == 7), "{progress:?}");
    assert!(progress.iter().any(|&(_, t)| t > 0), "{progress:?}");
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::OutputError { .. } | Event::OutputDrained)),
        "{seen:?}"
    );
}

#[test]
fn stats_are_absent_before_anything_is_measured() {
    let _s = serial();
    gst::init().expect("gstreamer");
    let (tx, _rx) = crossbeam_channel::bounded(16);
    let fresh = GstProgramme::new(FrameStore::default(), tx);
    assert_eq!(fresh.stats().snapshot(), StatsSnapshot::default());
    // Started, but nothing pushed and no sink: still nothing measured.
    let (p, _rx, _f, _l) = programme();
    assert_eq!(p.stats().snapshot(), StatsSnapshot::default());
}

#[test]
fn stats_fps_counts_pushes_per_second() {
    let _s = serial();
    let (mut p, _rx, _f, _l) = programme();
    pace(&mut p, 0..66, FrameChoice::Slate(SlateKind::Waiting));
    let fps = p.stats().snapshot().fps.expect("a complete window");
    let want = f64::from(P720.fps);
    assert!((fps - want).abs() <= 1.0, "fps {fps}, want {want} ± 1");
}

#[test]
fn stats_egress_and_bitrate_follow_bytes_sent() {
    let _s = serial();
    let dir = Tmp::new("egress");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..90, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let snap = p.stats().snapshot();
    let size = std::fs::metadata(dir.file("a.flv")).expect("file").len();
    assert_eq!(snap.egress_bytes, Some(size), "egress is the file's final size");
    let kbps = f64::from(snap.bitrate_kbps.expect("a complete window"));
    let want = f64::from(P720.video_kbps);
    assert!(
        (kbps - want).abs() <= want * 0.5,
        "bitrate {kbps} kbps against {want} ± 50 %"
    );
}

/// The encoder's speed over 2 s of real-pace pushes from `from` (PR #28 review item 5). One 1 s window
/// moves by a whole frame (1/30 = 0.033) when a frame lands on either side of its edge, the size of
/// the 0.95 floor's margin, so one window flaked. Two consecutive windows, read exactly 1 s apart
/// on the tick grid (the 2nd and 3rd seconds, after a whole window of real-time output), halve it.
fn paced_speed_over_two_windows(p: &mut GstProgramme, from: u64) -> f64 {
    let stats = p.stats();
    let fps = u64::from(P720.fps);
    let mut read = Vec::new();
    pace_with(
        p,
        from..from + 3 * fps + 2,
        |_| FrameChoice::Slate(SlateKind::Waiting),
        |i| {
            if i == from + 2 * fps || i == from + 3 * fps {
                read.push(stats.snapshot().encoder_speed.expect("a window"));
            }
        },
    );
    assert_eq!(read.len(), 2, "{read:?}");
    eprintln!("encoder speed windows from tick {from}: {read:?}");
    (read[0] + read[1]) / 2.0
}

#[test]
fn stats_encoder_speed_tracks_real_time_and_drops_when_starved() {
    let _s = serial();
    let (mut p, _rx, _f, _l) = programme();
    let band = ENCODER_SPEED_REALTIME_MIN..=1.1;
    let first = paced_speed_over_two_windows(&mut p, 0);
    thread::sleep(Duration::from_millis(1_500));
    let starved = p.stats().snapshot().encoder_speed.expect("a window");
    let resumed = paced_speed_over_two_windows(&mut p, 92);
    eprintln!("encoder speed: {first:.3} → starved {starved:.3} → resumed {resumed:.3}");
    assert!(band.contains(&first), "real time {first}");
    assert!(starved < 0.5, "starved {starved}");
    assert!(band.contains(&resumed), "resumed {resumed}");
}

#[test]
fn audio_ring_is_bounded_at_400ms_and_pads_silence() {
    let f = FrameStore::default();
    let second: Vec<u8> = (0..192_000u32).map(|i| (i % 251 + 1) as u8).collect();
    f.push_audio(&second);
    assert_eq!(f.audio_level(), AUDIO_RING_BYTES);
    let got = f.take_audio(192_000);
    assert_eq!(got.len(), 192_000);
    let pad = 192_000 - AUDIO_RING_BYTES;
    assert_eq!(pad, 115_200, "600 ms of 48 kHz stereo S16LE");
    assert!(got[..pad].iter().all(|&b| b == 0), "the first 600 ms is silence");
    assert_eq!(
        &got[pad..],
        &second[192_000 - AUDIO_RING_BYTES..],
        "the newest 400 ms survive"
    );
    assert_eq!(f.audio_level(), 0);
    // An empty ring gives n bytes of padding; a push that is not whole frames is refused.
    assert_eq!(f.take_audio(6_400), vec![0u8; 6_400]);
    f.push_audio(&[1, 2, 3]);
    assert_eq!((f.audio_level(), f.audio_refused()), (0, 1));
}

// ---------------------------------------------------------------------------------------------
// Amendment 2026-10-15: the textless slate

#[test]
fn slate_is_textless_house_backdrop_and_logo_for_every_kind() {
    let _s = serial();
    gst::init().expect("gstreamer");
    let mut modes = 0;
    // Both modes (ADR 0005): the logo box is 480 × 168 at 720p and 720 × 252 at 1080p.
    for profile in [OutputProfile::P720, OutputProfile::P1080] {
        let mut kinds = 0;
        let (w, h) = (profile.width, profile.height);
        let slate = Slate::render(w, h).expect("slate");
        let first = slate.slate_for(SlateKind::ALL[0]);
        for kind in SlateKind::ALL {
            let s = slate.slate_for(kind);
            assert!(Arc::ptr_eq(&s, &first), "{kind:?} shares the one buffer");
            let pic = Pic::of(&s);
            assert_eq!((pic.w, pic.h), (w, h));
            for (x, y) in [(10, 10), (w - 10, h - 10)] {
                let px = pic.at(x, y);
                assert!(close(px, HOUSE_NIGHT, 4), "{kind:?} {w}x{h} ({x},{y}) = {px:?}");
            }
            let share = pic.logo_share();
            assert!(
                share >= 0.10,
                "{kind:?} {w}x{h}: logo covers {share:.3} of its box"
            );
            kinds += 1;
        }
        assert!(
            !slate.factories().contains(&"textoverlay"),
            "{:?}",
            slate.factories()
        );
        assert!(slate.factories().contains(&"pngdec"), "{:?}", slate.factories());
        eprintln!("slate kinds checked: {kinds} ({w}x{h})");
        assert_eq!(
            SLATE_BACKDROP_RGB, HOUSE_NIGHT,
            "the adapter's backdrop is the house colour"
        );
        assert_eq!(kinds, SlateKind::ALL.len());
        assert!(kinds > 0);
        modes += 1;
    }
    assert_eq!(modes, 2);
    assert_eq!(logo_box(1_280, 720), (400, 276, 480, 168));
    assert_eq!(logo_box(1_920, 1_080), (600, 414, 720, 252));
}

#[test]
fn vendored_logo_matches_its_pinned_sha256() {
    let got = hex::encode(Sha256::digest(LOGO_PNG));
    assert_eq!(got, LOGO_SHA256);
}

#[test]
fn an_opaque_overlay_covers_the_slate_and_hiding_it_reveals_the_slate() {
    let _s = serial();
    let dir = Tmp::new("cover");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    let (w, h) = OVERLAY_SIZE;
    frames.set_overlay(solid(gst_video::VideoFormat::Bgra, w, h, [0, 255, 0, 255]));
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Reconnecting));
    p.hide_overlay();
    pace(&mut p, 15..30, FrameChoice::Slate(SlateKind::Reconnecting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 30);
    let covered = &d.frames[14];
    assert!(
        is_green(covered.centre) && is_green(covered.corner_tl),
        "{covered:?}"
    );
    assert!(
        covered.logo_share > 0.9,
        "the overlay covers the logo: {covered:?}"
    );
    let revealed = &d.frames[29];
    assert!(close(revealed.corner_tl, HOUSE_NIGHT, ON_AIR_TOL), "{revealed:?}");
    assert!(close(revealed.corner_br, HOUSE_NIGHT, ON_AIR_TOL), "{revealed:?}");
    assert!(revealed.logo_share >= 0.10, "the logo is back: {revealed:?}");
}

// ---------------------------------------------------------------------------------------------
// Amendment 2026-10-18: no rung clears the overlay; the owed checks

#[test]
fn the_bottom_rung_keeps_the_overlay_on_air() {
    let _s = serial();
    let dir = Tmp::new("bottom-rung");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    frames.set_overlay(overlay_blue_corner());
    p.step_down(Rung::OverlayHalfRate);
    pace(&mut p, 0..60, FrameChoice::Slate(SlateKind::Waiting));
    assert_eq!(p.overlay_pushes(), 30);
    // `Full` (never emitted by the domain; the match is total) restores every-tick pushes.
    p.step_down(Rung::Full);
    pace(&mut p, 60..90, FrameChoice::Slate(SlateKind::Waiting));
    assert_eq!(p.overlay_pushes(), 60);
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 90);
    let (mut even, mut odd) = (0, 0);
    for (i, f) in d.frames.iter().enumerate() {
        assert!(is_blue(f.spot), "frame {i}: {:?}", f.spot);
        if i % 2 == 0 {
            even += 1;
        } else {
            odd += 1;
        }
    }
    eprintln!("blue frames checked: even {even}, odd {odd}");
    assert!(even > 0 && odd > 0);
}

#[test]
fn hide_then_set_overlay_composites_again() {
    let _s = serial();
    let dir = Tmp::new("hide-set");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    frames.set_overlay(overlay_blue_corner());
    p.hide_overlay();
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    frames.set_overlay(overlay_blue_corner());
    pace(&mut p, 15..30, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 30);
    assert!(!is_blue(d.frames[14].spot), "hidden: {:?}", d.frames[14].spot);
    assert!(
        is_blue(d.frames[29].spot),
        "set again, on air: {:?}",
        d.frames[29].spot
    );
}

fn progress_of(events: &[Event], want: u64) -> Vec<u64> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OutputProgress { gen, total_bytes } if *gen == want => Some(*total_bytes),
            _ => None,
        })
        .collect()
}

#[test]
fn progress_total_grows_while_pushing_and_freezes_when_the_sink_stops() {
    let _s = serial();
    let dir = Tmp::new("grow-freeze");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    let mut while_pushing = Vec::new();
    pace_with(
        &mut p,
        0..90,
        |_| FrameChoice::Slate(SlateKind::Waiting),
        |_| {
            while let Ok(Inbound::Event(e)) = rx.try_recv() {
                while_pushing.push(e);
            }
        },
    );
    let grown = progress_of(&while_pushing, 1);
    let first = grown.iter().position(|&t| t > 0).expect("bytes were sent");
    let grown = &grown[first..];
    eprintln!("progress events read while pushing: {}", grown.len());
    assert!(grown.len() >= 8, "{grown:?}");
    for w in grown.windows(2) {
        assert!(
            w[1] > w[0],
            "the total grows on every poll while pushing: {grown:?}"
        );
    }
    let drained = drain_and_wait(&mut p, &rx);
    let last = *progress_of(&drained, 1).last().expect("the final sample");
    let after = progress_of(&events_for(&rx, Duration::from_millis(1_000)), 1);
    eprintln!("progress events read after the drain: {}", after.len());
    assert!(!after.is_empty(), "the poll keeps reading after the drain");
    assert!(
        after.iter().all(|&t| t <= last),
        "after {after:?} past the last {last}"
    );
    assert_eq!(last, std::fs::metadata(dir.file("a.flv")).expect("file").len());
}

/// Owed 15 (a): the poll is at most 250 ms (the choice, `_FINDINGS.md` F-011).
#[test]
fn the_progress_poll_keeps_the_pdf_stall_bound() {
    // Read at run time, so the check is a test that reds rather than a constant folded away.
    let poll = std::hint::black_box(OUTPUT_PROGRESS_POLL_MS);
    assert!(poll <= OWED_15_POLL_MAX_MS, "{poll} ms");
    assert!(poll > 0);
}

/// Owed 15: a sink whose writes stop is swapped within `output_stall_after_ms` + the poll of its
/// true last byte, by the domain's own `OutputLink` in the loop (one tick of quantisation added: the
/// domain sees time only at ticks). Owed 43 (after PR #29): the sink must first earn Connected
/// (`output_connected_after_ms` of growth), so the writes stop 1 s after `is_connected()` first
/// reads true, never at a fixed time, and the deadline follows the cut.
#[test]
fn stall_is_detected_within_budget_of_the_last_byte() {
    let _s = serial();
    let dir = Tmp::new("stall");
    let (mut p, rx, _f, _l) = programme();
    let t = Thresholds::default();
    let mut link = OutputLink::new();
    let t0 = Instant::now();
    let ms = |at: Instant| at.duration_since(t0).as_millis() as u64;
    let path = |gen: u64| dir.file(&format!("g{gen}.flv"));

    // The true last write, watched on disk every 5 ms, independently of the adapter's poll.
    let last_growth: Arc<Mutex<(u64, Instant)>> = Arc::new(Mutex::new((0, Instant::now())));
    let watching = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let watcher = {
        let (last_growth, watching, file) = (Arc::clone(&last_growth), Arc::clone(&watching), path(1));
        thread::spawn(move || {
            while watching.load(Ordering::SeqCst) {
                let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
                let mut g = last_growth.lock().unwrap();
                if size > g.0 {
                    *g = (size, Instant::now());
                }
                drop(g);
                thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let step = period(&P720);
    let mut held = false;
    let mut connected_at: Option<Instant> = None;
    let mut swapped_at: Option<Instant> = None;
    // The last growth as it stood when the swap was decided. The retired sink's teardown flushes
    // what `filesink` still buffered (bytes it took before the cut), so growth seen after the swap
    // is not a write the stall rule could have waited for.
    let mut at_swap: Option<(u64, Instant)> = None;
    let mut i = 0u64;
    // Until the cut, the sink has its connect bound and 4 s more to earn Connected; then the
    // stall bound and 4 s more to be swapped.
    let mut deadline = t0 + Duration::from_millis(t.output_connected_after_ms + 4_000);
    while Instant::now() < deadline {
        let due = t0 + step * i as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let now = Instant::now();
        while let Ok(Inbound::Event(e)) = rx.try_recv() {
            match e {
                Event::OutputProgress { gen, total_bytes } => link.on_progress(ms(now), gen, total_bytes, &t),
                Event::OutputError { gen } => link.on_error(ms(now), gen, &t),
                _ => {}
            }
        }
        if link.on_tick(ms(now), &t) {
            let gen = link.generation();
            if gen >= 2 {
                swapped_at = Some(now);
                at_swap = Some(*last_growth.lock().unwrap());
                p.swap_sink(gen, &file_target(&path(gen)));
                // Watch on past the swap, so the teardown's flush lands while the watcher runs
                // and the snapshot above, not the watcher's final reading, is what is judged.
                thread::sleep(Duration::from_millis(500));
                break;
            }
            p.swap_sink(gen, &file_target(&path(gen)));
        }
        if connected_at.is_none() && link.is_connected() {
            connected_at = Some(now);
        }
        // One healthy second after the sink earns Connected, it stops taking writes.
        if !held && connected_at.is_some_and(|c| now.duration_since(c) >= Duration::from_secs(1)) {
            assert!(link.is_connected(), "the sink is still connected at the cut");
            held = p.hold_sink_writes();
            assert!(held, "the hook found the sink");
            deadline = now + Duration::from_millis(t.output_stall_after_ms + 4_000);
        }
        p.push(i, FrameChoice::Slate(SlateKind::Waiting));
        i += 1;
    }
    watching.store(false, Ordering::SeqCst);
    watcher.join().expect("watcher");
    let connected_at = connected_at.expect("the sink earned Connected within its bound and 4 s");
    assert!(held, "the writes were cut");
    let swapped_at = swapped_at.expect("the stall swapped the sink within its bound and 4 s of the cut");
    let (bytes, last) = at_swap.expect("the snapshot is taken with the swap");
    let flushed = last_growth.lock().unwrap().0 - bytes;
    let gap = swapped_at.duration_since(last).as_millis() as u64;
    let bound = t.output_stall_after_ms + OWED_15_POLL_MAX_MS + period(&P720).as_millis() as u64 + 1;
    eprintln!(
        "connected {} ms after t0; last byte (total {bytes}) to SwapSink: {gap} ms; bound {bound} ms; \
         flushed by the teardown: {flushed} bytes",
        ms(connected_at)
    );
    assert!(bytes > 0);
    assert!(
        gap >= t.output_stall_after_ms,
        "swapped before the stall rule could fire: {gap} ms"
    );
    assert!(gap <= bound, "{gap} ms past the last byte, bound {bound} ms");
}

/// C08 amendment 2026-10-19: the adapter drops within 500 ms while what it owns is hung.
#[test]
fn drop_returns_within_500_ms_while_the_pipelines_hang() {
    let _s = serial();
    let dir = Tmp::new("drop");
    let (mut p, _rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    let hold = Duration::from_secs(3);
    assert!(p.hang_sink(hold), "the sink hook");
    assert!(p.hang_encoder(hold), "the encoder hook");
    // Two more ticks reach both probes, which then sleep holding their streaming locks.
    pace(&mut p, 15..20, FrameChoice::Slate(SlateKind::Waiting));
    thread::sleep(Duration::from_millis(300));
    let teardowns = p.teardown_counter();
    let t = Instant::now();
    drop(p);
    let took = t.elapsed();
    let in_flight = teardowns.load(Ordering::SeqCst);
    eprintln!("drop took {took:?}; teardowns in flight after it: {in_flight}");
    assert!(took < Duration::from_millis(500), "{took:?}");
    assert_eq!(
        in_flight, 2,
        "both teardowns are still held by the hang, so the hang was real"
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
// The audio section (F-004 verdict): real audio through, noise when there is none

/// A 440 Hz tone at −20 dBFS peak, one tick of it, continuing from sample `n`.
fn tone(n: &mut u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(TICK_AUDIO_BYTES);
    for _ in 0..TICK_AUDIO_BYTES / 4 {
        let v = (0.1
            * f64::from(i16::MAX)
            * (2.0 * std::f64::consts::PI * 440.0 * *n as f64 / 48_000.0).sin()) as i16;
        *n += 1;
        let [lo, hi] = v.to_le_bytes();
        out.extend_from_slice(&[lo, hi, lo, hi]);
    }
    out
}

fn report_audio(case: &str, d: &Decoded) -> (f64, f64, f64) {
    let fps = d.aac_frames as f64 / (d.audio_ms() as f64 / 1_000.0);
    let (kbps, zeros) = (d.aac_kbps(), d.zero_share());
    eprintln!(
        "audio {case}: {} AAC frames, {fps:.2} frames/s, {kbps:.1} kbps, zero-byte share {:.3}",
        d.aac_frames, zeros
    );
    (fps, kbps, zeros)
}

#[test]
fn camera_lost_audio_is_coded_noise_never_digital_silence() {
    let _s = serial();
    let dir = Tmp::new("noise");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    // No input audio at all: the slate, and nothing in the ring.
    pace(&mut p, 0..90, FrameChoice::Slate(SlateKind::Reconnecting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    let (fps, kbps, zeros) = report_audio("camera lost", &d);
    assert!(d.aac_frames > 100, "{}", d.aac_frames);
    assert!((fps - AAC_FRAMES_PER_S).abs() < 1.5, "{fps}");
    assert!((kbps - AAC_KBPS).abs() < AAC_KBPS * 0.2, "{kbps}");
    assert!(
        zeros <= 0.5,
        "zero-byte share {zeros:.3}: digital silence (F-004)"
    );
    let quiet = d.rms_dbfs(200, 2_800);
    assert!((-80.0..=-45.0).contains(&quiet), "filler level {quiet:.1} dBFS");
}

#[test]
fn live_audio_passes_through_and_an_underrun_neither_silences_nor_gaps_the_track() {
    let _s = serial();
    let dir = Tmp::new("live-audio");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    let mut n = 0u64;
    // 1 s of input audio, 1 s of none (the camera lost), 1 s of audio again.
    pace_with(
        &mut p,
        0..90,
        |_| FrameChoice::Slate(SlateKind::Waiting),
        |i| {
            if !(30..60).contains(&i) {
                frames.push_audio(&tone(&mut n));
            }
        },
    );
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    // The zero-byte share is reported, not held: a steady tone is coded with CBR fill bytes, which
    // are zeros (F-004's measure tells silence from noise, not a tone from either). The levels below
    // are the check.
    report_audio("live, underrun, live", &d);
    assert!(
        d.audio_ms().abs_diff(3_000) <= AV_TOLERANCE_MS,
        "no gap in the track: {} ms",
        d.audio_ms()
    );
    let (live1, lost, live2) = (
        d.rms_dbfs(200, 900),
        d.rms_dbfs(1_200, 1_900),
        d.rms_dbfs(2_200, 2_900),
    );
    eprintln!("segment levels: {live1:.1} / {lost:.1} / {live2:.1} dBFS");
    // A −20 dBFS-peak sine has an RMS of −23 dBFS.
    assert!((-27.0..=-19.0).contains(&live1), "{live1}");
    assert!((-80.0..=-45.0).contains(&lost), "{lost}");
    assert!((-27.0..=-19.0).contains(&live2), "{live2}");
}

// ---------------------------------------------------------------------------------------------
// Sequences: a second call, an empty input, after an interruption, the other mode

#[test]
fn swapping_twice_starts_each_sink_at_a_keyframe_and_drains_once() {
    let _s = serial();
    let dir = Tmp::new("swap-twice");
    let (mut p, rx, _f, _l) = programme();
    let mut drained = 0;
    for (gen, name) in [(1u64, "a.flv"), (2, "b.flv"), (3, "c.flv")] {
        p.swap_sink(gen, &file_target(&dir.file(name)));
        let from = (gen - 1) * 30;
        pace(&mut p, from..from + 30, FrameChoice::Slate(SlateKind::Waiting));
    }
    let mut seen = drain_and_wait(&mut p, &rx);
    seen.extend(events_for(&rx, Duration::from_millis(700)));
    drained += seen.iter().filter(|e| **e == Event::OutputDrained).count();
    assert_eq!(drained, 1, "{seen:?}");
    assert!(
        !seen.iter().any(|e| matches!(e, Event::OutputError { .. })),
        "{seen:?}"
    );
    let mut sinks = 0;
    for name in ["a.flv", "b.flv", "c.flv"] {
        let d = decode(&dir.file(name));
        assert!(d.frames.len() >= 20, "{name}: {}", d.frames.len());
        assert!(d.first_is_key(), "{name}");
        assert_eq!(d.video_pts_ms.first(), Some(&0), "{name}");
        let first_audio = *d.audio_pts_ms.first().expect("audio");
        assert!(
            first_audio <= 1_024 * 1_000 / 48_000,
            "{name}: audio starts at {first_audio} ms"
        );
        sinks += 1;
    }
    eprintln!("sinks checked: {sinks}");
    assert_eq!(sinks, 3);
    assert_eq!(p.builds(), 1);
}

#[test]
fn a_swap_during_the_drain_still_drains_once() {
    let _s = serial();
    let dir = Tmp::new("swap-drain");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..30, FrameChoice::Slate(SlateKind::Waiting));
    p.drain();
    p.swap_sink(2, &file_target(&dir.file("b.flv")));
    let mut seen = events_until(&rx, Duration::from_secs(15), |e| *e == Event::OutputDrained);
    seen.extend(events_for(&rx, Duration::from_millis(1_000)));
    let drained = seen.iter().filter(|e| **e == Event::OutputDrained).count();
    assert_eq!(drained, 1, "{seen:?}");
    assert!(
        !seen.iter().any(|e| matches!(e, Event::OutputError { .. })),
        "{seen:?}"
    );
    // A push after the drain is refused, never a panic.
    p.push(30, FrameChoice::Slate(SlateKind::Waiting));
    assert_eq!(p.refused_pushes(), 1);
}

#[test]
fn a_late_first_sink_starts_at_a_keyframe_inside_the_ring() {
    let _s = serial();
    let dir = Tmp::new("late-sink");
    let (mut p, rx, _f, _l) = programme();
    // 2.5 s with no sink: the 2 s ring has dropped the first keyframe, so its oldest frame is a delta.
    pace(&mut p, 0..75, FrameChoice::Slate(SlateKind::Waiting));
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 75..105, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert!(d.first_is_key(), "keys {:?}", &d.keys[..d.keys.len().min(5)]);
    assert_eq!(d.video_pts_ms.first(), Some(&0));
    // It starts at frame 60, the 2 s keyframe still in the ring: 45 frames, not the 30 since the swap.
    assert!(d.frames.len() >= 44, "{}", d.frames.len());
}

#[test]
fn a_slate_then_back_to_camera_and_back_again() {
    let _s = serial();
    let dir = Tmp::new("slate-camera");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    frames.set_camera(solid(gst_video::VideoFormat::I420, 1_280, 720, [255, 0, 0, 255]));
    pace_with(
        &mut p,
        0..60,
        |i| {
            if (20..40).contains(&i) {
                FrameChoice::Emit
            } else {
                FrameChoice::Slate(SlateKind::Reconnecting)
            }
        },
        |_| {},
    );
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 60);
    let mut checked = 0;
    for (i, f) in d.frames.iter().enumerate() {
        if (20..40).contains(&i) {
            assert!(is_red(f.corner_tl) && is_red(f.centre), "frame {i}: camera {f:?}");
        } else {
            assert!(
                close(f.corner_tl, HOUSE_NIGHT, ON_AIR_TOL),
                "frame {i}: slate {f:?}"
            );
        }
        checked += 1;
    }
    eprintln!("frames checked: {checked}");
    assert_eq!(checked, 60);
}

#[test]
fn emit_with_no_camera_frame_pushes_the_slate_and_counts_it() {
    let _s = serial();
    let dir = Tmp::new("no-camera");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..10, FrameChoice::Emit);
    drain_and_wait(&mut p, &rx);
    assert_eq!(p.camera_missing(), 10);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 10);
    assert!(close(d.frames[9].corner_tl, HOUSE_NIGHT, ON_AIR_TOL));
}

#[test]
fn a_second_start_is_refused_and_nothing_is_rebuilt() {
    let _s = serial();
    let (mut p, _rx, _f, _l) = programme();
    assert_eq!(p.start(&P720), Err("programme already started".to_owned()));
    assert_eq!(
        p.start(&OutputProfile::P1080),
        Err("programme already started".to_owned())
    );
    assert_eq!(p.builds(), 1);
}

#[test]
fn a_profile_whose_ticks_do_not_divide_the_audio_is_refused() {
    let _s = serial();
    gst::init().expect("gstreamer");
    for fps in [0, 7] {
        let (tx, _rx) = crossbeam_channel::bounded(16);
        let mut p = GstProgramme::new(FrameStore::default(), tx);
        let profile = OutputProfile { fps, ..P720 };
        assert!(p.start(&profile).is_err(), "fps {fps}");
        assert_eq!(p.builds(), 0);
    }
}

#[test]
fn calls_before_start_are_refused_without_panic() {
    let _s = serial();
    gst::init().expect("gstreamer");
    let (tx, rx) = crossbeam_channel::bounded(16);
    let mut p = GstProgramme::new(FrameStore::default(), tx);
    p.push(0, FrameChoice::Emit);
    assert_eq!(p.refused_pushes(), 1);
    p.swap_sink(4, &file_target(Path::new("/nonexistent/x.flv")));
    assert_eq!(
        events_until(&rx, Duration::from_secs(1), |_| true),
        vec![Event::OutputError { gen: 4 }]
    );
    p.drain();
    p.drain();
    let seen = events_for(&rx, Duration::from_millis(300));
    assert_eq!(
        seen,
        vec![Event::OutputDrained],
        "drained once, with nothing to drain"
    );
}

#[test]
fn a_drain_with_no_sink_reports_drained_once() {
    let _s = serial();
    let (mut p, rx, _f, _l) = programme();
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    p.drain();
    p.drain();
    let mut seen = events_until(&rx, Duration::from_secs(10), |e| *e == Event::OutputDrained);
    seen.extend(events_for(&rx, Duration::from_millis(500)));
    assert_eq!(
        seen.iter().filter(|e| **e == Event::OutputDrained).count(),
        1,
        "{seen:?}"
    );
}

#[test]
fn an_index_that_does_not_advance_is_refused() {
    let _s = serial();
    let (mut p, _rx, _f, _l) = programme();
    p.push(5, FrameChoice::Slate(SlateKind::Waiting));
    p.push(5, FrameChoice::Slate(SlateKind::Waiting));
    p.push(4, FrameChoice::Slate(SlateKind::Waiting));
    p.push(9, FrameChoice::Slate(SlateKind::Waiting));
    assert_eq!(p.refused_pushes(), 2);
    assert_eq!(p.overlay_pushes(), 2);
}

#[test]
fn a_target_with_an_unknown_scheme_or_no_key_is_an_output_error() {
    let _s = serial();
    let (mut p, rx, _f, lines) = programme();
    p.swap_sink(
        1,
        &TargetSpec {
            url: "srt://127.0.0.1:9".to_owned(),
            stream_key: "SECRETKEY-srt-0001".to_owned(),
        },
    );
    p.swap_sink(
        2,
        &TargetSpec {
            url: "rtmps://127.0.0.1:9/live/".to_owned(),
            stream_key: String::new(),
        },
    );
    let seen = events_for(&rx, Duration::from_millis(500));
    let errors: Vec<&Event> = seen
        .iter()
        .filter(|e| matches!(e, Event::OutputError { .. }))
        .collect();
    assert_eq!(
        errors,
        vec![&Event::OutputError { gen: 1 }, &Event::OutputError { gen: 2 }]
    );
    // Both are refused before any sink is built, each with its own reason.
    let lines = lines.lock().unwrap().clone();
    for (gen, reason) in [
        (1, "output target scheme is not rtmp, rtmps or file"),
        (2, "output target has no stream key"),
    ] {
        let found = lines
            .iter()
            .any(|l| l.starts_with(&format!("swap_sink gen={gen}: {reason}")));
        assert!(found, "gen {gen}: {reason:?} not in {lines:?}");
    }
    assert!(
        !lines.iter().any(|l| l.contains(": sink ")),
        "a sink was built: {lines:?}"
    );
}

#[test]
fn the_1080p_programme_keeps_its_own_caps() {
    let _s = serial();
    let dir = Tmp::new("p1080");
    let profile = OutputProfile::P1080;
    let (mut p, rx, frames, _l) = programme_with(&profile);
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    frames.set_overlay(overlay_blue_corner());
    frames.set_camera(solid(gst_video::VideoFormat::I420, 1_280, 720, [255, 0, 0, 255]));
    pace_with(
        &mut p,
        0..30,
        |i| {
            if i < 15 {
                FrameChoice::Slate(SlateKind::Final)
            } else {
                FrameChoice::Emit
            }
        },
        |_| {},
    );
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 30);
    assert!(d.frames.iter().all(|f| (f.w, f.h) == (1_920, 1_080)));
    assert!(
        close(d.frames[14].corner_br, HOUSE_NIGHT, ON_AIR_TOL),
        "{:?}",
        d.frames[14]
    );
    assert!(is_blue(d.frames[14].spot) && is_blue(d.frames[29].spot));
    assert!(is_red(d.frames[29].centre), "{:?}", d.frames[29]);
}

// ---------------------------------------------------------------------------------------------
// The adapter's own guards (each mutated once: the PR lists them)

#[test]
fn a_programme_error_is_fatal_once_and_its_text_is_scrubbed() {
    let _s = serial();
    let dir = Tmp::new("fatal");
    let (mut p, rx, _f, lines) = programme();
    let key = "SECRETKEY-fatal-0001";
    p.swap_sink(
        1,
        &TargetSpec {
            url: format!("file://{}", dir.file("a.flv").display()),
            stream_key: key.to_owned(),
        },
    );
    for _ in 0..2 {
        assert!(p.plant_programme_error(&format!("the element said {key}")));
    }
    let seen = events_for(&rx, Duration::from_millis(800));
    let fatals: Vec<&Event> = seen.iter().filter(|e| matches!(e, Event::Fatal { .. })).collect();
    assert_eq!(fatals.len(), 1, "{seen:?}");
    let Event::Fatal { detail: Some(detail) } = fatals[0] else {
        panic!("a fatal error with no detail: {fatals:?}");
    };
    assert!(detail.as_str().contains("planted programme error"), "{detail:?}");
    assert!(
        !detail.as_str().contains(key) && detail.as_str().contains("****"),
        "{detail:?}"
    );
    let lines = lines.lock().unwrap().clone();
    let logged = lines.iter().filter(|l| l.starts_with("programme error")).count();
    eprintln!("programme error lines: {logged}");
    assert_eq!(logged, 2, "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains(key)), "{lines:?}");
}

#[test]
fn an_encoder_that_stalls_reports_encoder_overrun_and_the_programme_carries_on() {
    let _s = serial();
    let dir = Tmp::new("overrun");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    assert!(p.hang_encoder(Duration::from_millis(2_000)), "the encoder hook");
    pace(&mut p, 15..120, FrameChoice::Slate(SlateKind::Waiting));
    let seen = drain_and_wait(&mut p, &rx);
    let overruns = seen.iter().filter(|e| **e == Event::EncoderOverrun).count();
    eprintln!("encoder overruns read: {overruns}");
    assert!(overruns >= 1, "{seen:?}");
    let d = decode(&dir.file("a.flv"));
    eprintln!("frames after a 2 s encoder stall: {} of 120", d.frames.len());
    // The leaky queue dropped what the stalled encoder could not take; the programme went on.
    assert!(
        d.frames.len() < 120,
        "nothing was dropped, so the stall was not real"
    );
    assert!(d.frames.len() >= 60, "{}", d.frames.len());
    assert_eq!(p.builds(), 1);
}

/// The bound a held sink leaves on the output's two appsrcs: the feeder's own bound plus the one
/// entry it may push past it.
fn queue_bound() -> u64 {
    2 * OUTPUT_QUEUE_BYTES
}

/// The entries 2 s of encoded output holds: 30 video frames a second at 30 fps and 48 000 / 1 024
/// AAC frames a second, with a tenth of a second of slack.
fn ring_bound() -> usize {
    let per_second = f64::from(P720.fps) + 48_000.0 / 1_024.0;
    (per_second * (ENCODED_RING_SPAN.as_secs_f64() + 0.1)).ceil() as usize
}

#[test]
fn a_held_sink_backs_up_into_the_bounded_ring_not_the_output() {
    let _s = serial();
    let dir = Tmp::new("held");
    let (mut p, _rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    assert!(p.hold_sink_writes(), "the hook found the sink");
    let (mut queued, mut ring, mut samples) = (0u64, 0usize, 0);
    for second in 0..5u64 {
        let from = 15 + second * 30;
        pace(&mut p, from..from + 30, FrameChoice::Slate(SlateKind::Waiting));
        let (v, a) = p.output_queued_bytes().expect("an output");
        queued = queued.max(v.max(a));
        ring = ring.max(p.ring_entries());
        samples += 1;
        assert!(
            v <= queue_bound() && a <= queue_bound(),
            "second {second}: queued {v} / {a}"
        );
        assert!(
            p.ring_entries() <= ring_bound().min(ENCODED_RING_MAX_ENTRIES),
            "{}",
            p.ring_entries()
        );
    }
    eprintln!(
        "held-sink samples: {samples}; most queued {queued} B (bound {}), ring at most {ring} (bound {})",
        queue_bound(),
        ring_bound()
    );
    assert_eq!(samples, 5);
    assert!(
        queued >= OUTPUT_QUEUE_BYTES,
        "the output never filled to the feeder's bound"
    );
    assert!(ring * 10 >= ring_bound() * 8, "the ring never filled: {ring}");
}

#[test]
fn a_sink_that_stalls_past_the_ring_resumes_at_a_keyframe() {
    let _s = serial();
    let dir = Tmp::new("stall-recover");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..30, FrameChoice::Slate(SlateKind::Waiting));
    // 5 s: the output fills (about 1 s), then the ring's 2 s fill and drop their oldest.
    assert!(p.hang_sink(Duration::from_secs(5)), "the hook found the sink");
    pace(&mut p, 30..300, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    let step = (1_000.0 / f64::from(P720.fps)).round() as u64;
    let mut gaps = 0;
    for w in d.parsed.windows(2) {
        let ((before, _), (after, key)) = (w[0], w[1]);
        if after > before + step + 1 {
            gaps += 1;
            assert!(
                key,
                "the frame at {after} ms after a gap from {before} ms is not a keyframe"
            );
        }
    }
    eprintln!("gaps checked: {gaps}; frames parsed: {}", d.parsed.len());
    assert!(gaps >= 1, "the stall never outlasted the ring");
    assert!(d.first_is_key());
}

#[test]
fn a_camera_frame_that_cannot_be_fitted_shows_the_slate_and_is_logged_once() {
    let _s = serial();
    let dir = Tmp::new("unfit");
    let (mut p, rx, frames, lines) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    // Caps that promise a 640 × 360 I420 frame, and a buffer far too small to be one.
    let caps = gst_video::VideoInfo::builder(gst_video::VideoFormat::I420, 640, 360)
        .build()
        .expect("info")
        .to_caps()
        .expect("caps");
    let short = gst::Sample::builder()
        .buffer(&gst::Buffer::from_mut_slice(vec![0u8; 16]))
        .caps(&caps)
        .build();
    frames.set_camera(short);
    pace(&mut p, 0..10, FrameChoice::Emit);
    // A good frame afterwards is fitted as usual.
    frames.set_camera(solid(gst_video::VideoFormat::I420, 640, 360, [255, 0, 0, 255]));
    pace(&mut p, 10..20, FrameChoice::Emit);
    drain_and_wait(&mut p, &rx);
    assert_eq!(p.camera_missing(), 10);
    let lines = lines.lock().unwrap().clone();
    let logged = lines.iter().filter(|l| l.contains("cannot be fitted")).count();
    eprintln!("unfit camera ticks: 10, lines logged: {logged}");
    assert_eq!(logged, 1, "{lines:?}");
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 20);
    assert!(
        close(d.frames[9].corner_tl, HOUSE_NIGHT, ON_AIR_TOL),
        "{:?}",
        d.frames[9]
    );
    assert!(is_red(d.frames[19].centre), "{:?}", d.frames[19]);
}

#[test]
fn an_output_error_is_reported_once_and_its_progress_stops() {
    let _s = serial();
    let dir = Tmp::new("output-error");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(5, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    for _ in 0..2 {
        assert!(p.plant_output_error(), "the hook found the output");
    }
    let seen = events_until(&rx, Duration::from_secs(2), |e| {
        matches!(e, Event::OutputError { .. })
    });
    assert_eq!(seen.last(), Some(&Event::OutputError { gen: 5 }));
    pace(&mut p, 15..45, FrameChoice::Slate(SlateKind::Waiting));
    let after = events_for(&rx, Duration::from_millis(500));
    let errors = after
        .iter()
        .filter(|e| matches!(e, Event::OutputError { .. }))
        .count();
    let progress = progress_of(&after, 5).len();
    eprintln!("after the error: {errors} more errors, {progress} progress events");
    assert_eq!((errors, progress), (0, 0), "{after:?}");
}

#[test]
fn a_drain_waits_for_the_sink_to_take_everything() {
    let _s = serial();
    let dir = Tmp::new("drain-held");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..30, FrameChoice::Slate(SlateKind::Waiting));
    assert!(p.hold_sink_writes(), "the hook found the sink");
    p.drain();
    // The encoders reach EOS at once; the sink, holding its writes, never does. Drained is the
    // sink's to say (the domain's drain_timeout_ms is the bound on waiting for it).
    let seen = events_for(&rx, Duration::from_millis(1_500));
    let progress = progress_of(&seen, 1).len();
    eprintln!(
        "events while the held sink drains: {}, progress among them: {progress}",
        seen.len()
    );
    assert!(progress > 0, "the watch was running");
    assert!(!seen.contains(&Event::OutputDrained), "{seen:?}");
}

// ---------------------------------------------------------------------------------------------
// PR #28 review, round 1

/// Review item 1: a swap that lands just before the retired output's next poll. The retired watch's
/// last reading lands after the new sink has started counting; it must change nothing. Egress never
/// exceeds what is on disk (each sink's total is a file's size at some earlier instant), and once the
/// new sink has one complete window its bitrate is a real rate, not the 0 a stale higher total left.
#[test]
fn egress_and_bitrate_survive_a_swap_just_before_the_old_poll() {
    let _s = serial();
    let dir = Tmp::new("swap-before-poll");
    let (mut p, _rx, _f, _l) = programme();
    let stats = p.stats();
    let step = period(&P720);
    let poll = Duration::from_millis(OUTPUT_PROGRESS_POLL_MS);
    let t0 = Instant::now();
    let mut i = 0u64;
    let push_until = |p: &mut GstProgramme, until: Instant, i: &mut u64| {
        while Instant::now() < until {
            let due = t0 + step * (*i as u32);
            if let Some(w) = due.checked_duration_since(Instant::now()) {
                thread::sleep(w.min(until.saturating_duration_since(Instant::now())));
            }
            if Instant::now() >= due {
                p.push(*i, FrameChoice::Slate(SlateKind::Waiting));
                *i += 1;
            }
        }
    };
    let on_disk = |n: u64| -> u64 {
        (1..=n)
            .map(|g| {
                std::fs::metadata(dir.file(&format!("g{g}.flv")))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum()
    };
    p.swap_sink(1, &file_target(&dir.file("g1.flv")));
    let mut built = Instant::now();
    // The first sink lives 4 s, so a stale reading of it is large; the swap lands 25 ms before its
    // watch's 16th poll.
    push_until(&mut p, built + poll * 16 - Duration::from_millis(25), &mut i);
    let mut checked = 0;
    for gen in 2..=7u64 {
        p.swap_sink(gen, &file_target(&dir.file(&format!("g{gen}.flv"))));
        built = Instant::now();
        // 120 ms on, the retired watch's poll has landed (it was due 25 ms after the swap).
        push_until(&mut p, built + Duration::from_millis(120), &mut i);
        let egress = stats.snapshot().egress_bytes.unwrap_or(0);
        let disk = on_disk(gen);
        // 1.6 s on, the new sink's first window (from its first poll at 250 ms) is complete.
        push_until(&mut p, built + Duration::from_millis(1_600), &mut i);
        let later = stats.snapshot();
        let disk_later = on_disk(gen);
        eprintln!(
            "gen {gen}: +120 ms egress {egress} on disk {disk}; +1.6 s egress {:?} on disk {disk_later}, bitrate {:?}",
            later.egress_bytes, later.bitrate_kbps
        );
        assert!(
            egress <= disk,
            "gen {gen}: egress {egress} over the {disk} bytes on disk"
        );
        assert!(
            later.egress_bytes.unwrap_or(0) <= disk_later,
            "gen {gen}: {later:?} over {disk_later}"
        );
        // A healthy 720p sink sends at least a quarter of its video rate.
        let floor = P720.video_kbps / 4;
        assert!(
            later.bitrate_kbps.is_some_and(|b| b >= floor),
            "gen {gen}: bitrate {:?}, floor {floor}",
            later.bitrate_kbps
        );
        checked += 1;
        // The next swap lands 25 ms before this sink's watch's 7th poll.
        push_until(&mut p, built + poll * 7 - Duration::from_millis(25), &mut i);
    }
    eprintln!("swaps timed before a poll checked: {checked}");
    assert_eq!(checked, 6);
}

/// Review item 2 (ruling: `try_send` and a drop counter): the driver's thread is the inbound
/// channel's only reader, so a call on it that waits for room never returns. A refused swap (before
/// `start`, or a scheme the adapter refuses) and a drain with nothing started each try once and
/// count the drop. The lost `OutputError` costs nothing lasting: the domain's link times the connect
/// out and swaps again.
#[test]
fn refused_calls_never_block_on_a_full_inbound_channel() {
    let _s = serial();
    gst::init().expect("gstreamer");
    let (tx, rx) = crossbeam_channel::bounded(1);
    tx.send(Inbound::Event(Event::Tick)).expect("fill the channel");
    let lines: Lines = Arc::default();
    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    let worker = thread::spawn({
        let lines = Arc::clone(&lines);
        move || {
            let log = move |l: &str| {
                lines
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(l.to_owned())
            };
            let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(log);
            let refused = TargetSpec {
                url: "srt://127.0.0.1:9".to_owned(),
                stream_key: "SECRETKEY-full-channel".to_owned(),
            };
            // Never started: a swap and a drain.
            let mut idle = GstProgramme::new(FrameStore::default(), tx.clone()).with_log(Arc::clone(&log));
            idle.swap_sink(1, &file_target(Path::new("/tmp/never.flv")));
            idle.drain();
            // Started: a scheme the adapter refuses.
            let mut started = GstProgramme::new(FrameStore::default(), tx).with_log(log);
            started.start(&P720).expect("start");
            started.swap_sink(2, &refused);
            let _ = done_tx.send((idle.events_dropped(), started.events_dropped()));
        }
    });
    let returned = done_rx.recv_timeout(Duration::from_secs(3));
    // Unblock a blocked worker either way, so the test process can end.
    let mut seen = Vec::new();
    while let Ok(m) = rx.recv_timeout(Duration::from_millis(200)) {
        if let Inbound::Event(e) = m {
            seen.push(e);
        }
    }
    let _ = worker.join();
    let (idle, started) = returned.expect("the calls returned within 3 s with the channel full");
    let lines = lines.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let logged = lines.iter().filter(|l| l.contains("not delivered")).count();
    eprintln!("dropped: idle {idle}, started {started}; lines naming the drop {logged}; delivered {seen:?}");
    assert_eq!((idle, started), (2, 1));
    assert_eq!(logged, 3, "{lines:?}");
    assert_eq!(seen, vec![Event::Tick], "only what was there before");
    assert!(lines.iter().all(|l| !l.contains("SECRETKEY")), "{lines:?}");
}

/// Review item 3: a sink that dies mid-stream and is not swapped (the domain is in its back-off, or
/// in the hold), then the session drains. Nothing more can reach the dead sink, so the drain ends
/// when the encoders do, well inside the domain's `drain_timeout_ms`.
#[test]
fn a_failed_sink_left_in_place_still_drains() {
    let _s = serial();
    let dir = Tmp::new("failed-in-place");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..15, FrameChoice::Slate(SlateKind::Waiting));
    assert!(p.fail_sink_writes());
    pace(&mut p, 15..30, FrameChoice::Slate(SlateKind::Waiting));
    events_until(&rx, Duration::from_secs(2), |e| {
        *e == Event::OutputError { gen: 1 }
    });
    let t = Instant::now();
    p.drain();
    let seen = events_for(&rx, Duration::from_millis(2_000));
    let drained = seen.iter().filter(|e| **e == Event::OutputDrained).count();
    let bound = Thresholds::default().drain_timeout_ms;
    eprintln!(
        "drained events {drained} within 2 s of the drain (took {:?}); the domain waits {bound} ms",
        t.elapsed()
    );
    assert_eq!(drained, 1, "{seen:?}");
}

/// Review item 3, the other order: the sink stops taking writes during the drain (the encoders have
/// reached EOS, and the sink never will), then errors, as rtmp2sink does when its write times out.
/// The error ends the drain: nothing more can reach that sink.
#[test]
fn a_sink_that_fails_during_the_drain_ends_it() {
    let _s = serial();
    let dir = Tmp::new("fail-in-drain");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..30, FrameChoice::Slate(SlateKind::Waiting));
    assert!(p.hold_sink_writes());
    p.drain();
    let before = events_for(&rx, Duration::from_millis(1_000));
    assert!(!before.contains(&Event::OutputDrained), "{before:?}");
    assert!(p.plant_output_error());
    let after = events_for(&rx, Duration::from_millis(1_000));
    let drained = after.iter().filter(|e| **e == Event::OutputDrained).count();
    eprintln!("held drain, then the sink's error: {after:?}");
    assert!(after.contains(&Event::OutputError { gen: 1 }), "{after:?}");
    assert_eq!(drained, 1, "{after:?}");
}

/// The ring's gap rule, deterministically (the reviewer's M4 survived a quiet machine: whether the
/// stall test's natural overflow leaves a delta frame at the ring's front is timing). While the sink
/// hangs and the feeder holds off, the ring loses its oldest entries up to a delta frame; when the
/// sink resumes, the output's next picture after the gap is a keyframe, never that delta.
#[test]
fn after_the_ring_drops_entries_the_output_resumes_at_a_keyframe() {
    let _s = serial();
    let dir = Tmp::new("ring-gap");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..30, FrameChoice::Slate(SlateKind::Waiting));
    assert!(p.hang_sink(Duration::from_secs(4)), "the hook found the sink");
    // The output's queue fills (`OUTPUT_QUEUE_BYTES`, about 1.4 s) and the feeder holds off; the
    // ring then holds about a second.
    pace(&mut p, 30..105, FrameChoice::Slate(SlateKind::Waiting));
    let held = p.ring_entries();
    let dropped = p.drop_ring_to_delta();
    pace(&mut p, 105..240, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    let step = (1_000.0 / f64::from(P720.fps)).round() as u64;
    let gaps: Vec<(u64, u64, bool)> = d
        .parsed
        .windows(2)
        .filter(|w| w[1].0 > w[0].0 + step + 1)
        .map(|w| (w[0].0, w[1].0, w[1].1))
        .collect();
    eprintln!("ring held {held}, dropped {dropped} up to a delta; gaps (before ms, after ms, key) {gaps:?}; frames parsed {}", d.parsed.len());
    assert!(dropped >= 1 && held > dropped, "held {held}, dropped {dropped}");
    assert!(!gaps.is_empty(), "no gap reached the file");
    assert!(gaps.iter().all(|g| g.2), "{gaps:?}");
    assert!(d.first_is_key());
}

/// Review item 4 (ruling: keep the rebase, witnessed on the appsrc side): flvmux starts its own
/// timestamps at 0, so the file cannot tell a rebased output from one that is not. What the feeder
/// sends into the output's appsrcs can: a second sink, built 2 s into the programme, starts its video
/// at exactly 0 and its audio within one AAC frame of it.
#[test]
fn each_output_rebases_its_first_timestamps_to_zero() {
    let _s = serial();
    let dir = Tmp::new("rebase");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..60, FrameChoice::Slate(SlateKind::Waiting));
    p.swap_sink(2, &file_target(&dir.file("b.flv")));
    pace(&mut p, 60..90, FrameChoice::Slate(SlateKind::Waiting));
    let (video, audio) = p.output_first_pts().expect("an output");
    drain_and_wait(&mut p, &rx);
    eprintln!("second sink's first PTS: video {video:?}, audio {audio:?} (built at 2 s of programme)");
    assert_eq!(video, Some(gst::ClockTime::ZERO));
    let aac_frame = gst::ClockTime::from_nseconds(1_024 * 1_000_000_000 / 48_000);
    assert!(audio.is_some_and(|a| a <= aac_frame), "{audio:?}");
}

/// Review item 6 (ruling: the fitter holds the input buffer it cached by): a camera's buffer pool
/// recycles a buffer as soon as nothing holds it, so the next frame can arrive in the very same
/// `GstBuffer` with new pixels. Keyed by address alone, the fitter served the old frame's conversion.
#[test]
fn a_recycled_camera_buffer_is_converted_again() {
    let _s = serial();
    let dir = Tmp::new("pool-recycle");
    let (mut p, rx, frames, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    let i420 = gst_video::VideoFormat::I420;
    // A shape the programme does not take (640×360), so every frame goes through the fitter.
    let info = gst_video::VideoInfo::builder(i420, 640, 360)
        .build()
        .expect("info");
    let caps = info.to_caps().expect("caps");
    let pool = gst::BufferPool::new();
    let mut config = pool.config();
    config.set_params(Some(&caps), info.size() as u32, 1, 2);
    pool.set_config(config).expect("pool config");
    pool.set_active(true).expect("pool active");
    let from_pool = |rgba: [u8; 4]| -> (gst::Sample, usize) {
        let pixels = solid(i420, 640, 360, rgba);
        let bytes = pixels
            .buffer()
            .expect("buffer")
            .map_readable()
            .expect("map")
            .to_vec();
        let mut b = pool.acquire_buffer(None).expect("a pool buffer");
        b.get_mut()
            .expect("writable")
            .copy_from_slice(0, &bytes)
            .expect("fill");
        let at = b.as_ptr() as usize;
        (gst::Sample::builder().buffer(&b).caps(&caps).build(), at)
    };
    // The programme's own shape skips the fitter; it releases the pool buffer from the store.
    let native = solid(i420, P720.width, P720.height, [0, 255, 0, 255]);
    let colours = [[255u8, 0, 0, 255], [0, 0, 255, 255]];
    let mut addresses = Vec::new();
    let mut want = Vec::new();
    let step = period(&P720);
    let t0 = Instant::now();
    for i in 0..20u64 {
        if let Some(w) = (t0 + step * i as u32).checked_duration_since(Instant::now()) {
            thread::sleep(w);
        }
        frames.set_camera(native.clone());
        let c = colours[(i % 2) as usize];
        let (sample, at) = from_pool(c);
        addresses.push(at);
        frames.set_camera(sample);
        want.push(i % 2);
        p.push(i, FrameChoice::Emit);
    }
    drain_and_wait(&mut p, &rx);
    pool.set_active(false).expect("pool inactive");
    let d = decode(&dir.file("a.flv"));
    assert_eq!(d.frames.len(), 20);
    let wrong: Vec<(usize, u64)> = d
        .frames
        .iter()
        .enumerate()
        .filter(|(i, f)| {
            if want[*i] == 0 {
                !is_red(f.centre)
            } else {
                !is_blue(f.centre)
            }
        })
        .map(|(i, _)| (i, want[i]))
        .collect();
    let reused = addresses.windows(2).filter(|w| w[0] == w[1]).count();
    eprintln!(
        "pool frames checked: {}, consecutive frames in a reused buffer: {reused}, wrong: {wrong:?}",
        d.frames.len()
    );
    assert!(wrong.is_empty(), "{wrong:?}");
}

/// Review item 7: progress the poll could not deliver is counted and said, once when the channel
/// fills and once, with the count, when it drains.
#[test]
fn progress_dropped_on_a_full_channel_is_counted_and_logged() {
    let _s = serial();
    let dir = Tmp::new("progress-dropped");
    gst::init().expect("gstreamer");
    let (tx, rx) = crossbeam_channel::bounded(1);
    tx.send(Inbound::Event(Event::Tick)).expect("fill the channel");
    let lines: Lines = Arc::default();
    let sink = Arc::clone(&lines);
    let mut p = GstProgramme::new(FrameStore::default(), tx).with_log(Arc::new(move |l: &str| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(l.to_owned());
    }));
    p.start(&P720).expect("start");
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..30, FrameChoice::Slate(SlateKind::Waiting));
    let dropped = p.progress_dropped();
    // Room again: the next poll is delivered, and says how many were not.
    let _ = rx.recv();
    let first = events_until(&rx, Duration::from_secs(2), |e| {
        matches!(e, Event::OutputProgress { .. })
    });
    // The count is logged just after that event is delivered, on the watch's thread: wait for it.
    let end = Instant::now() + Duration::from_secs(1);
    let lines_now = loop {
        let now = lines.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if now.iter().any(|l| l.contains("progress events were dropped")) || Instant::now() > end {
            break now;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let full = lines_now
        .iter()
        .filter(|l| l.contains("inbound channel is full"))
        .count();
    let summed: Vec<&String> = lines_now
        .iter()
        .filter(|l| l.contains("progress events were dropped"))
        .collect();
    eprintln!("dropped {dropped} over 1 s; lines: full {full}, summed {summed:?}; first after {first:?}");
    assert!(dropped >= 3, "{dropped}");
    assert_eq!(full, 1, "{lines_now:?}");
    assert_eq!(summed.len(), 1, "{lines_now:?}");
    // The line's count is the streak's: at least what was dropped before the channel drained, and
    // no more than the counter now.
    let said: u64 = summed[0]
        .split(": ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|n| n.parse().ok())
        .expect("a count in the line");
    assert!(
        (dropped..=p.progress_dropped()).contains(&said),
        "{said} against {dropped}..={}",
        p.progress_dropped()
    );
    drain_and_wait(&mut p, &rx);
}

/// Review item 8 (brief Step 6: "push_buffer errors are logged, redacted and counted"): a layer whose
/// appsrc refuses buffers counts every refusal and says so once a run: once when it starts failing,
/// and once more only after it has recovered and fails again.
#[test]
fn a_refused_layer_push_is_counted_and_logged_once_a_run() {
    let _s = serial();
    let dir = Tmp::new("push-errors");
    let (mut p, _rx, _f, lines) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..10, FrameChoice::Slate(SlateKind::Waiting));
    assert_eq!(p.push_errors(), 0);
    let said = |lines: &Lines| -> Vec<String> {
        let l = lines.lock().unwrap_or_else(PoisonError::into_inner);
        l.iter().filter(|l| l.contains("push failed")).cloned().collect()
    };
    assert!(p.stop_base_layer());
    pace(&mut p, 10..20, FrameChoice::Slate(SlateKind::Waiting));
    let first_run = (p.push_errors(), said(&lines).len());
    assert!(p.restart_base_layer());
    pace(&mut p, 20..30, FrameChoice::Slate(SlateKind::Waiting));
    let recovered = (p.push_errors(), said(&lines).len());
    assert!(p.stop_base_layer());
    pace(&mut p, 30..35, FrameChoice::Slate(SlateKind::Waiting));
    let second_run = (p.push_errors(), said(&lines));
    eprintln!("(errors, lines): first run {first_run:?}, recovered {recovered:?}, second run {second_run:?}");
    assert_eq!(first_run, (10, 1));
    assert_eq!(recovered, (10, 1), "the restarted layer pushes again");
    assert_eq!(second_run.0, 15);
    assert_eq!(second_run.1.len(), 2, "{:?}", second_run.1);
    assert!(
        second_run.1.iter().all(|l| l.contains("base")),
        "{:?}",
        second_run.1
    );
}

/// Owed 40's bound: the ring holds 400 ms of audio, so a 1 s gap fills its last 400 ms (12 periods at
/// 30 fps) and keeps the rest as one hole, never a burst of a whole second of filler.
#[test]
fn a_long_gap_fills_only_the_rings_span_of_audio() {
    let _s = serial();
    let dir = Tmp::new("long-gap");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    pace(&mut p, 0..30, FrameChoice::Slate(SlateKind::Waiting));
    thread::sleep(Duration::from_secs(1));
    pace(&mut p, 60..90, FrameChoice::Slate(SlateKind::Waiting));
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    let jumps: Vec<u64> = d
        .audio_pts_ms
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|&dt| dt > 22)
        .collect();
    // 30 periods missed, 12 filled (`AUDIO_RING_BYTES` over one period): an 18-period hole, 600 ms,
    // seen as one jump of the hole plus one AAC frame.
    let period_ms = 1_000 / u64::from(P720.fps);
    let fill = (AUDIO_RING_BYTES / (48_000 / P720.fps as usize * 4)) as u64;
    let hole_ms = (30 - fill) * 1_000 / u64::from(P720.fps);
    eprintln!("audio frames checked {}, jumps {jumps:?}; filled {fill} periods, hole {hole_ms} ms ({period_ms} ms a period)", d.audio_pts_ms.len());
    assert_eq!(fill, 12);
    assert_eq!(jumps.len(), 1, "{jumps:?}");
    assert!(
        jumps[0].abs_diff(hole_ms + 21) <= 22,
        "{jumps:?} against {hole_ms} + one frame"
    );
}

/// Owed 40 (owner, 2026-10-19: "1 ok"): a skipped tick leaves no hole in the audio. The compositor
/// repeats the last picture across the gap; the audio of every missed period is pushed with the next
/// tick, from the ring and then the filler, so the track has no jump and its full length.
#[test]
fn skipped_ticks_leave_no_hole_in_the_audio() {
    let _s = serial();
    let dir = Tmp::new("skipped-ticks");
    let (mut p, rx, _f, _l) = programme();
    p.swap_sink(1, &file_target(&dir.file("a.flv")));
    let step = period(&P720);
    let t0 = Instant::now();
    let mut skipped = 0;
    for i in 0..150u64 {
        if let Some(w) = (t0 + step * i as u32).checked_duration_since(Instant::now()) {
            thread::sleep(w);
        }
        if i % 10 == 9 {
            skipped += 1;
            continue;
        }
        p.push(i, FrameChoice::Slate(SlateKind::Waiting));
    }
    drain_and_wait(&mut p, &rx);
    let d = decode(&dir.file("a.flv"));
    // One AAC frame is 21.3 ms; a jump past 22 ms is a hole.
    let jumps: Vec<u64> = d
        .audio_pts_ms
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|&dt| dt > 22)
        .collect();
    // Index 148 is the last pushed, so the audio covers 149 periods.
    let want_ms = 149 * 1_000 / u64::from(P720.fps);
    eprintln!(
        "ticks skipped {skipped}; video frames {}; audio frames checked {}, jumps {jumps:?}; audio {} ms against {want_ms}",
        d.frames.len(),
        d.audio_pts_ms.len(),
        d.audio_ms()
    );
    assert_eq!(skipped, 15, "9, 19, … 149");
    assert!(d.audio_pts_ms.len() > 200, "{}", d.audio_pts_ms.len());
    assert!(jumps.is_empty(), "{jumps:?}");
    assert!(
        d.audio_ms().abs_diff(want_ms) <= AV_TOLERANCE_MS,
        "audio {} ms",
        d.audio_ms()
    );
}
