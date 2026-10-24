//! Everything of the harness that runs GStreamer (feature `media`): the checker's decode of the
//! recorded segments, and fake-camera's pipeline.
//!
//! **The decode.** `filesrc ! decodebin`, and each decoded stream to an appsink: video as I420, read
//! for the camera strip ([`strip::read`]) and the score strip ([`score::read`]) frame by frame;
//! audio as 48 kHz mono S16LE, fed to the tone scan ([`audio::Scan`]). A segment that errors part
//! way keeps the frames before the error and records it (`decode_error`).
//!
//! **The camera.** `appsrc (I420) ! x264enc ! h264parse ! mpegtsmux` with
//! `appsrc (S16LE) ! audioconvert ! voaacenc ! aacparse` beside it, into `srtsink` (caller). The
//! buffers carry the camera's own PTS from 0 under appsrc's time segment (F-002's trap was raw
//! encoder PTS under a default segment); `mpegtsmux alignment=7` keeps each SRT payload at 1316
//! bytes (C10). The stream id and passphrase are element properties, never in the URI, so no
//! credential is in a string GStreamer prints.
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use crate::audio::{self, Scan};
use crate::camera::{self, Layout, Pattern};
use crate::checker::{FrameRec, SegmentScan};
use crate::strip::Plane;
use crate::{clock, score, strip};

/// The longest a segment's decode may take (a 10 s segment decodes in about a second).
pub const SEGMENT_DECODE_LIMIT_S: u64 = 120;
/// The camera's frame duration, in ns.
pub const FRAME_NS: u64 = 1_000_000_000 / camera::FPS;

pub fn init() -> Result<(), String> {
    gst::init().map_err(|e| format!("gstreamer: {e}"))
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|e| format!("no {factory} element: {e}"))
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn appsink(caps: &gst::Caps) -> gst_app::AppSink {
    let sink = gst_app::AppSink::builder().caps(caps).sync(false).build();
    sink.set_property("async", false);
    sink
}

/// The I420 caps of a `w`×`h` 30 fps picture.
pub fn i420_caps(w: u32, h: u32) -> Result<gst::Caps, String> {
    gst_video::VideoInfo::builder(gst_video::VideoFormat::I420, w, h)
        .fps(gst::Fraction::new(camera::FPS as i32, 1))
        .build()
        .and_then(|i| i.to_caps())
        .map_err(|e| format!("{w}x{h}: {e}"))
}

/// Checks `camera::Layout` against GStreamer's for the size (the camera draws into the one and
/// GStreamer reads the other).
pub fn check_layout(l: &Layout) -> Result<(), String> {
    let info = gst_video::VideoInfo::builder(gst_video::VideoFormat::I420, l.width, l.height)
        .build()
        .map_err(|e| format!("{}x{}: {e}", l.width, l.height))?;
    let theirs = (
        info.stride()[0] as usize,
        info.stride()[1] as usize,
        info.offset()[1],
        info.offset()[2],
        info.size(),
    );
    let ours = (l.y_stride, l.c_stride, l.u_offset, l.v_offset, l.size);
    if theirs != ours {
        return Err(format!(
            "{}x{}: the I420 layout {ours:?} is not GStreamer's {theirs:?}",
            l.width, l.height
        ));
    }
    Ok(())
}

/// The audio caps the camera sends and the checker reads: 48 kHz mono S16LE.
pub fn audio_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("rate", audio::RATE as i32)
        .field("channels", 1i32)
        .build()
}

/// I420 at whatever size the recording has.
fn i420_caps_any() -> gst::Caps {
    gst::Caps::builder("video/x-raw").field("format", "I420").build()
}

/// A decoded I420 sample's luma plane, read for both strips.
fn read_video(sample: &gst::Sample) -> Option<FrameRec> {
    let buffer = sample.buffer()?;
    let pts_ns = buffer.pts()?.nseconds();
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let map = buffer.map_readable().ok()?;
    let start = info.offset()[0];
    let stride = usize::try_from(info.stride()[0]).ok()?;
    let rows = stride * info.height() as usize;
    let y = Plane {
        data: map.as_slice().get(start..start + rows)?,
        stride,
        width: info.width(),
        height: info.height(),
    };
    Some(FrameRec {
        pts_ns,
        counter: strip::read(&y),
        score: score::read(&y),
    })
}

#[derive(Default)]
struct Decoded {
    frames: Vec<FrameRec>,
    /// Video samples without a PTS or a readable layout (reported, never read as frames).
    unreadable: u64,
    scan: Scan,
    link_errors: Vec<String>,
}

/// Links one of decodebin's pads: video and audio to their appsinks, anything else to a fakesink.
fn link_decoded(pipeline: &gst::Pipeline, pad: &gst::Pad, out: &Arc<Mutex<Decoded>>) -> Result<(), String> {
    let caps = pad.current_caps().ok_or("a decoded pad without caps")?;
    let name = caps
        .structure(0)
        .map(|s| s.name().to_string())
        .unwrap_or_default();
    let elements: Vec<gst::Element> = if name.starts_with("video/") {
        let sink = appsink(&i420_caps_any());
        let o = Arc::clone(out);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let mut d = locked(&o);
                    match read_video(&sample) {
                        Some(f) => d.frames.push(f),
                        None => d.unreadable += 1,
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        vec![make("videoconvert")?, sink.upcast()]
    } else if name.starts_with("audio/") {
        let sink = appsink(&audio_caps());
        let o = Arc::clone(out);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let Some(buffer) = sample.buffer() else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let (Some(pts), Ok(map)) = (buffer.pts(), buffer.map_readable()) else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let samples: Vec<i16> = map
                        .as_slice()
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    locked(&o).scan.feed(pts.nseconds(), &samples);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        vec![make("audioconvert")?, make("audioresample")?, sink.upcast()]
    } else {
        vec![make("fakesink")?]
    };
    pipeline.add_many(&elements).map_err(|e| format!("{name}: {e}"))?;
    gst::Element::link_many(&elements).map_err(|e| format!("{name}: {e}"))?;
    for e in &elements {
        e.sync_state_with_parent().map_err(|e| format!("{name}: {e}"))?;
    }
    let sink = elements[0].static_pad("sink").ok_or("no sink pad")?;
    pad.link(&sink).map(|_| ()).map_err(|e| format!("{name}: {e:?}"))
}

/// Decodes one recorded segment. `file` is its name relative to the recordings directory; its wall
/// start comes from its file name (`clock::segment_start_us`), which the caller has read.
pub fn scan_segment(path: &Path, file: &str, start_wall_us: i64) -> SegmentScan {
    let out = Arc::new(Mutex::new(Decoded::default()));
    let error = decode(path, &out).err();
    let d = std::mem::take(&mut *locked(&out));
    let mut errors: Vec<String> = d.link_errors;
    if d.unreadable > 0 {
        errors.push(format!(
            "{} video samples had no PTS or no readable picture",
            d.unreadable
        ));
    }
    errors.extend(error);
    SegmentScan {
        file: file.to_owned(),
        start_wall_us,
        frames: d.frames,
        audio: d.scan.finish(),
        decode_error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

fn decode(path: &Path, out: &Arc<Mutex<Decoded>>) -> Result<(), String> {
    let pipeline = gst::Pipeline::new();
    let src = make("filesrc")?;
    src.set_property("location", path.to_str().ok_or("a non-UTF-8 path")?);
    let bin = make("decodebin")?;
    pipeline.add_many([&src, &bin]).map_err(|e| e.to_string())?;
    src.link(&bin).map_err(|e| e.to_string())?;
    let weak = pipeline.downgrade();
    let o = Arc::clone(out);
    bin.connect_pad_added(move |_, pad| {
        let Some(pipeline) = weak.upgrade() else { return };
        if let Err(e) = link_decoded(&pipeline, pad, &o) {
            locked(&o).link_errors.push(e);
        }
    });
    let result = run_to_eos(&pipeline);
    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn error_text(e: &gst::message::Error) -> String {
    format!(
        "{}: {}",
        e.src().map(|s| s.name().to_string()).unwrap_or_default(),
        e.error()
    )
}

/// Plays `pipeline` until EOS (`Ok`), an error, or [`SEGMENT_DECODE_LIMIT_S`].
fn run_to_eos(pipeline: &gst::Pipeline) -> Result<(), String> {
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("cannot play: {e}"))?;
    let bus = pipeline.bus().ok_or("no bus")?;
    let msg = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(SEGMENT_DECODE_LIMIT_S),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    match msg.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => Ok(()),
        Some(gst::MessageView::Error(e)) => Err(error_text(e)),
        _ => Err(format!("no EOS within {SEGMENT_DECODE_LIMIT_S} s")),
    }
}

/// Every `.mp4` under `dir`, as (path, name relative to `dir`), in name order.
fn mp4s(dir: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        for entry in entries {
            let p = entry.map_err(|e| format!("{}: {e}", d.display()))?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "mp4") {
                let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().into_owned();
                out.push((p, rel));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// Decodes every segment under `dir`. A file whose name is not a segment start
/// (`clock::segment_start_us`) is refused: its wall time would be a guess.
pub fn scan_recordings(dir: &Path) -> Result<Vec<SegmentScan>, String> {
    let files = mp4s(dir)?;
    let mut out = Vec::with_capacity(files.len());
    for (path, rel) in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let start = clock::segment_start_us(&name).ok_or(format!(
            "{rel}: not a segment name (YYYY-MM-DD_HH-MM-SS-ffffff.mp4)"
        ))?;
        out.push(scan_segment(&path, &rel, start));
    }
    Ok(out)
}

/// Where the camera publishes: MediaMTX's SRT listener, as a caller.
#[derive(Clone, PartialEq, Eq)]
pub struct SrtTarget {
    pub host: String,
    pub port: u16,
    pub stream_id: String,
    pub passphrase: String,
    pub latency_ms: u32,
}

impl fmt::Debug for SrtTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SrtTarget")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("stream_id", &self.stream_id)
            .field("passphrase", &"***")
            .field("latency_ms", &self.latency_ms)
            .finish()
    }
}

/// What the camera's encoder writes to: SRT (the harness) or an MPEG-TS file (the media tests).
#[derive(Debug, Clone)]
pub enum CameraSink {
    Srt(SrtTarget),
    File(PathBuf),
}

/// x264's settings: the brief's 4 Mbit/s, a keyframe a second, and nothing that adds latency.
pub fn x264enc() -> Result<gst::Element, String> {
    let enc = make("x264enc")?;
    enc.set_property("bitrate", 4_000u32);
    enc.set_property("key-int-max", camera::FPS as u32);
    enc.set_property_from_str("tune", "zerolatency");
    enc.set_property_from_str("speed-preset", "ultrafast");
    Ok(enc)
}

fn live_appsrc(caps: &gst::Caps, max_bytes: u64) -> gst_app::AppSrc {
    gst_app::AppSrc::builder()
        .caps(caps)
        .format(gst::Format::Time)
        .is_live(true)
        .do_timestamp(false)
        // A burst's release blocks the camera's thread while the encoder catches up, rather than
        // queueing seconds of raw frames in memory.
        .block(true)
        .max_bytes(max_bytes)
        .build()
}

/// What `CameraPipeline::on_sent` calls with each buffer the camera sends.
pub type SentFn = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// One build of the camera's pipeline. A failure (`failure`) ends the build; the caller makes a new
/// one, whose PTS start from 0 again.
pub struct CameraPipeline {
    pipeline: gst::Pipeline,
    /// The sink the mux writes to (`on_sent`'s probe).
    sink: gst::Element,
    video: gst_app::AppSrc,
    /// The x264enc, for a key frame on demand ([`Self::force_key_unit`]).
    encoder: gst::Element,
    /// `None`: a video-only build (`CameraOptions::audio` false, Owed 81's control).
    audio: Option<gst_app::AppSrc>,
    layout: Layout,
    pattern: Pattern,
    base_counter: Option<u32>,
}

impl fmt::Debug for CameraPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CameraPipeline")
            .field("layout", &self.layout)
            .field("pattern", &self.pattern)
            .field("base_counter", &self.base_counter)
            .field("audio", &self.audio.is_some())
            .finish()
    }
}

/// The TS packets per buffer the camera's mux pushes by default: 7, 1 316 bytes, an SRT payload
/// (C10). `mpegtsmux` holds packets until it has that many (gstbasetsmux.c,
/// `gst_base_ts_mux_push_packets`, 1.24.2), so a frame's last packets wait for later ones (F-028).
pub const TS_ALIGNMENT: u32 = 7;

/// How a camera build is made (`CameraPipeline::start_with`; Owed 81's controls).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraOptions {
    /// The audio track. Without it (fake-camera's `--audio off`) the TS carries the video alone.
    pub audio: bool,
    /// `mpegtsmux alignment`: TS packets per pushed buffer (fake-camera's `--ts-alignment`), 1 to 7
    /// (more than 7 would pass an SRT payload's 1 316 bytes).
    pub ts_alignment: u32,
}

impl Default for CameraOptions {
    fn default() -> Self {
        Self {
            audio: true,
            ts_alignment: TS_ALIGNMENT,
        }
    }
}

impl CameraPipeline {
    pub fn start(sink: &CameraSink, w: u32, h: u32, pattern: Pattern) -> Result<Self, String> {
        Self::start_with(sink, w, h, pattern, CameraOptions::default())
    }

    /// `start`, with or without the audio track and with another mux alignment (Owed 81's
    /// controls). An alignment outside 1 to 7 is refused.
    pub fn start_with(
        sink: &CameraSink,
        w: u32,
        h: u32,
        pattern: Pattern,
        opts: CameraOptions,
    ) -> Result<Self, String> {
        if !(1..=TS_ALIGNMENT).contains(&opts.ts_alignment) {
            return Err(format!(
                "ts alignment {}: 1 to {TS_ALIGNMENT} packets a buffer",
                opts.ts_alignment
            ));
        }
        let layout = Layout::i420(w, h);
        check_layout(&layout)?;
        let pipeline = gst::Pipeline::new();
        let video = live_appsrc(&i420_caps(w, h)?, 8 * layout.size as u64);
        let audio = opts.audio.then(|| live_appsrc(&audio_caps(), 1 << 20));
        let (h264parse, mux) = (make("h264parse")?, make("mpegtsmux")?);
        h264parse.set_property("config-interval", -1i32);
        mux.set_property("alignment", opts.ts_alignment as i32);
        let out = match sink {
            CameraSink::Srt(t) => {
                let s = make("srtsink")?;
                s.set_property("uri", format!("srt://{}:{}?mode=caller", t.host, t.port));
                s.set_property("streamid", &t.stream_id);
                if !t.passphrase.is_empty() {
                    s.set_property("passphrase", &t.passphrase);
                }
                s.set_property("latency", t.latency_ms as i32);
                s.set_property("sync", false);
                s
            }
            CameraSink::File(p) => {
                let s = make("filesink")?;
                s.set_property("location", p.to_str().ok_or("a non-UTF-8 path")?);
                s
            }
        };
        let encoder = x264enc()?;
        let v: [gst::Element; 4] = [video.clone().upcast(), encoder.clone(), h264parse, make("queue")?];
        pipeline
            .add_many(v.iter().chain([&mux, &out]))
            .map_err(|e| e.to_string())?;
        gst::Element::link_many(&v).map_err(|e| format!("camera video: {e}"))?;
        v[3].link(&mux).map_err(|e| format!("camera video to mux: {e}"))?;
        if let Some(audio) = &audio {
            let a: [gst::Element; 5] = [
                audio.clone().upcast(),
                make("audioconvert")?,
                make("voaacenc")?,
                make("aacparse")?,
                make("queue")?,
            ];
            pipeline.add_many(a.iter()).map_err(|e| e.to_string())?;
            gst::Element::link_many(&a).map_err(|e| format!("camera audio: {e}"))?;
            a[4].link(&mux).map_err(|e| format!("camera audio to mux: {e}"))?;
        }
        mux.link(&out).map_err(|e| format!("camera mux to sink: {e}"))?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("camera: cannot play: {e}"))?;
        Ok(Self {
            pipeline,
            sink: out,
            video,
            encoder,
            audio,
            layout,
            pattern,
            base_counter: None,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.layout.width, self.layout.height)
    }

    /// Hands `f` the bytes of every buffer the sink is about to send, on the mux's streaming thread,
    /// just before the sink takes it (Owed 81's probe: `wire::scan` reads them). Install it before
    /// the first `push`, so no buffer passes unseen.
    pub fn on_sent(&self, f: SentFn) -> Result<(), String> {
        let pad = self
            .sink
            .static_pad("sink")
            .ok_or("the camera's sink has no sink pad")?;
        pad.add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            move |_, info| {
                if let Some(b) = info.buffer() {
                    if let Ok(m) = b.map_readable() {
                        f(m.as_slice());
                    }
                } else if let Some(list) = info.buffer_list() {
                    for b in list.iter() {
                        if let Ok(m) = b.map_readable() {
                            f(m.as_slice());
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            },
        )
        .ok_or("the camera's sink refused the probe")?;
        Ok(())
    }

    /// A new picture size from the next frame (appsrc carries the caps change in order).
    pub fn set_size(&mut self, w: u32, h: u32) -> Result<(), String> {
        let layout = Layout::i420(w, h);
        check_layout(&layout)?;
        self.video.set_caps(Some(&i420_caps(w, h)?));
        self.video.set_max_bytes(8 * layout.size as u64);
        self.layout = layout;
        Ok(())
    }

    /// Pushes frame `counter` (its picture and its 1 600 samples), stamped `(counter − first) ×
    /// FRAME_NS`, where `first` is this build's first counter.
    pub fn push(&mut self, counter: u32) -> Result<(), String> {
        let base = *self.base_counter.get_or_insert(counter);
        let pts = gst::ClockTime::from_nseconds(u64::from(counter.saturating_sub(base)) * FRAME_NS);
        let dur = gst::ClockTime::from_nseconds(FRAME_NS);
        let mut vb = gst::Buffer::from_mut_slice(camera::frame(&self.layout, self.pattern, counter));
        let stamp = |b: &mut gst::Buffer| -> Result<(), String> {
            let m = b.get_mut().ok_or("a new buffer is not writable")?;
            m.set_pts(pts);
            m.set_duration(dur);
            Ok(())
        };
        stamp(&mut vb)?;
        self.video
            .push_buffer(vb)
            .map_err(|e| format!("camera video push: {e:?}"))?;
        if let Some(a) = &self.audio {
            let pcm: Vec<u8> = audio::frame_samples(counter)
                .iter()
                .flat_map(|s| s.to_le_bytes())
                .collect();
            let mut ab = gst::Buffer::from_mut_slice(pcm);
            stamp(&mut ab)?;
            a.push_buffer(ab)
                .map_err(|e| format!("camera audio push: {e:?}"))?;
        }
        Ok(())
    }

    /// Asks the encoder for a key frame (with its SPS and PPS) on the next frame pushed (C14: a
    /// resume, `camera::Schedule::take_key_due`). An upstream force-key-unit event on the encoder's
    /// source pad, which GstVideoEncoder answers on the next frame it codes.
    pub fn force_key_unit(&self) -> Result<(), String> {
        let event = gst_video::UpstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        let pad = self.encoder.static_pad("src").ok_or("x264enc has no src pad")?;
        if pad.send_event(event) {
            Ok(())
        } else {
            Err("x264enc refused the key-unit request".into())
        }
    }

    /// The build's error or EOS, if the bus has one (never blocks).
    pub fn failure(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        let msg = bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])?;
        Some(match msg.view() {
            gst::MessageView::Error(e) => error_text(e),
            _ => "end of stream".to_owned(),
        })
    }

    /// Ends the streams and waits (at most `wait_s`) for the sink to take them, so a file is whole.
    pub fn finish(self, wait_s: u64) -> Result<(), String> {
        let _ = self.video.end_of_stream();
        if let Some(a) = &self.audio {
            let _ = a.end_of_stream();
        }
        let bus = self.pipeline.bus().ok_or("no bus")?;
        let msg = bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(wait_s),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        match msg.as_ref().map(|m| m.view()) {
            Some(gst::MessageView::Eos(_)) => Ok(()),
            Some(gst::MessageView::Error(e)) => Err(error_text(e)),
            _ => Err(format!("no EOS within {wait_s} s")),
        }
    }
}

impl Drop for CameraPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
