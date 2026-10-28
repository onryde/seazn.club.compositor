//! The programme pipeline (C09; spec §3.3's refinement), built once in `start()` and **never
//! rebuilt**:
//!
//! - video: `appsrc(base) + appsrc(overlay) → compositor → AYUV (BT.709) → videoconvert → I420
//!   (BT.709) → queue(leaky) → x264enc → h264parse → appsink(venc)`;
//! - audio: `appsrc(audio) → audioconvert → voaacenc → aacparse → appsink(aenc)`.
//!
//! **Its own clock.** Every buffer is timestamped by tick index, `pts = index × period`, so the
//! output stays constant-rate however the loop is paced. The appsrcs are not live: nothing in this
//! pipeline waits on, or times out against, the GStreamer clock (F-010: a live compositor composites a
//! frame WITHOUT a push that lands later than its latency, then drops the push).
//!
//! **Per tick** (`push(i, choice)`): the base layer is the latest camera frame (`Emit`, `Repeat`) or
//! the textless slate (`Slate`); the overlay layer is the latest CEF frame, or a transparent frame
//! while the overlay slot is empty (hidden, or never painted); the audio is one period of the 400 ms
//! ring, and any part the ring lacks is low-level pink noise, never digital zero (F-004). On rung
//! `OverlayHalfRate` the overlay is pushed on even indices only, each push covering two periods, so
//! the compositor keeps it on air for the odd ones (owner, 2026-10-18: no rung takes the score off).
//! Every tick hands the camera slot's write count to `Stats::on_camera_tick`, which counts the camera
//! frames no tick put on air (`camera_frames_replaced`, Owed 81 (i)).
//!
//! **Never blocks** the loop: the appsrcs leak the oldest buffer when full, and nothing here waits
//! on a pipeline. Teardown goes to reaper threads.
use std::fmt;
#[cfg(feature = "test-hooks")]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;
#[cfg(feature = "test-hooks")]
use std::time::Instant;

use app::ports::{Inbound, ProgrammeOutput, TargetSpec};
use crossbeam_channel::Sender;
use domain::config::OutputProfile;
use domain::health::ExitDetail;
use domain::ladder::Rung;
use domain::programme::FrameChoice;
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use crate::frame_store::{FrameStore, AUDIO_CHANNELS, AUDIO_FRAME_BYTES, AUDIO_RATE, AUDIO_RING_BYTES};
use crate::gst_output::{self, EncodedRing, Entry, LogFn, OutputGen, Shared, OUTPUT_SINK_TIMEOUT_S};
use crate::redact::Scrubber;
use crate::slate::Slate;
use crate::stats::Stats;

/// CEF renders the overlay page at 1920 × 1080 in every mode (ADR 0005; plan, Global Constraints).
pub const OVERLAY_SIZE: (u32, u32) = (1_920, 1_080);
/// The appsrcs hold at most this many ticks, then leak the oldest (C09 brief: "max-bytes = 3 frames").
const APPSRC_TICKS: u64 = 3;
/// AAC-LC at 128 kbps (F-004: the verified fix encodes the input's audio at AAC-LC 128 kbps, 48 kHz,
/// stereo).
const AAC_BITRATE: i32 = 128_000;
/// x264's preset (owner ruling 2026-10-27 on F-033: 1080p `medium` on 6 CPUs, which keeps every 1080p
/// leg within the 75 % size rule). F-033: `fast` and `medium` score within 0.5 VMAF of `faster`,
/// `slow` within 0.8. Every 1080p `medium` cost leg ran real time with 0 drops (4 CPUs: D:noise;
/// 6 CPUs: C:noise, D:noise, D:snow; C:snow's beats unfetched). Only the one-host sweep, on raw
/// per-pixel noise, fell behind (48.3 to 50.4 s for 45 s of frames), its cause not settled. It drives
/// 720p too. Open: no 3 h soak of 1080p `medium` on 6 CPUs yet. The rest are the settings C00
/// measured real time (F-005): `zerolatency`, a 2 s keyframe interval, no B-frames (F-004), a
/// 1 000 ms VBV and CBR with filler (POC values).
const X264_SPEED_PRESET: &str = "medium";
const X264_TUNE: &str = "zerolatency";
const X264_VBV_MS: u32 = 1_000;
const X264_OPTIONS: &str = "nal-hrd=cbr:filler=1";
/// The pink-noise filler's peak: 0.001 of full scale, about −60 dBFS (F-004: `audiotestsrc
/// wave=pink-noise volume=0.001` encodes as real coded AAC, 3.9 % zero bytes, where digital silence
/// encodes as 94.7 % zero bytes and made YouTube report Poor).
const NOISE_PEAK: f32 = 0.001;
/// The layers, by the index `Running::failing` keeps them at (for the push-failure lines).
const LAYERS: [&str; 3] = ["base", "overlay", "audio"];

/// Owed 94, a test hook (C14's output-hung case): how sink generation 1 stalls once it has run a
/// while (`stall_first_sink`). Either way its total stops while the encoders keep delivering, and no
/// sink error comes: the egress backstop's shape.
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkStall {
    /// `hold_sink_writes`: its writes are held until its teardown flushes them.
    Hold,
    /// `hang_sink(SINK_HANG_HOLD)`: its next write sleeps holding its streaming lock, so its teardown
    /// hangs as well (F-001), on a reaper thread.
    Hang,
}

/// How long a planted hang holds: twice F-004's budget, past the backstop and a whole recovery.
#[cfg(feature = "test-hooks")]
pub const SINK_HANG_HOLD: Duration = Duration::from_millis(2 * domain::config::OUTPUT_GAP_BUDGET_MS);

/// A planned stall: what, how long after sink generation 1's build, and that build's instant.
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy)]
struct PlannedStall {
    stall: SinkStall,
    after: Duration,
    built_at: Option<Instant>,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn stderr_log() -> LogFn {
    // C12 replaces this with its `ts=… level=info msg=…` sink (`with_log`).
    Arc::new(|line: &str| eprintln!("programme: {line}"))
}

/// The live `ProgrammeOutput` (C09).
pub struct GstProgramme {
    frames: FrameStore,
    shared: Shared,
    builds: u32,
    overlay_pushes: u64,
    /// Pushes refused: before `start`, after `drain`, or an index not past the last one.
    refused_pushes: u64,
    /// `Emit`/`Repeat` ticks with no camera frame in the store (the slate was pushed instead).
    camera_missing: u64,
    /// Events this (the driver's) thread could not deliver because the inbound channel was full.
    events_dropped: u64,
    /// Buffers an appsrc refused (`push_buffer` returned a flow error).
    push_errors: u64,
    half_rate: bool,
    running: Option<Running>,
    /// Test hook (`refuse_key_requests`): keyframe requests still to refuse.
    #[cfg(feature = "test-hooks")]
    key_refusals: Arc<AtomicU32>,
    /// Test hook (`stall_first_sink`): the stall still to plant on sink generation 1.
    #[cfg(feature = "test-hooks")]
    sink_stall: Option<PlannedStall>,
}

/// Everything `start()` built.
struct Running {
    profile: OutputProfile,
    period_bytes: usize,
    pipeline: gst::Pipeline,
    base: gst_app::AppSrc,
    overlay: gst_app::AppSrc,
    audio: gst_app::AppSrc,
    #[cfg(feature = "test-hooks")]
    encoder: gst::Element,
    parser_src: gst::Pad,
    slate: Slate,
    transparent: gst::Buffer,
    noise: PinkNoise,
    camera_fit: Fitter,
    overlay_fit: Fitter,
    last_index: Option<u64>,
    /// The most missed periods a tick's audio fills (Owed 40): the frame store's ring, 400 ms.
    audio_fill_periods: u64,
    /// Each layer's last push failed (a run of failures is logged once).
    failing: [bool; 3],
    drained: bool,
    output: Option<OutputGen>,
    stop: Arc<AtomicBool>,
}

impl GstProgramme {
    pub fn new(frames: FrameStore, tx: Sender<Inbound>) -> Self {
        Self {
            frames,
            shared: Shared {
                ring: Arc::new(EncodedRing::default()),
                tx,
                log: stderr_log(),
                stats: Arc::new(Stats::default()),
                scrubber: Arc::new(Mutex::new(Scrubber::default())),
                reapers: Arc::new(AtomicUsize::new(0)),
                progress_dropped: Arc::new(AtomicU64::new(0)),
                sink_timeout_s: OUTPUT_SINK_TIMEOUT_S,
            },
            builds: 0,
            overlay_pushes: 0,
            refused_pushes: 0,
            camera_missing: 0,
            events_dropped: 0,
            push_errors: 0,
            half_rate: false,
            running: None,
            #[cfg(feature = "test-hooks")]
            key_refusals: Arc::new(AtomicU32::new(0)),
            #[cfg(feature = "test-hooks")]
            sink_stall: None,
        }
    }

    /// Routes this adapter's log lines (each one scrubbed of every credential it knows) to `log`
    /// instead of stderr. C12 wires its log sink here.
    pub fn with_log(mut self, log: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        self.shared.log = log;
        self
    }

    /// Programme pipelines built so far: 1 once started, and never more (the pipeline is never
    /// rebuilt).
    pub fn builds(&self) -> u32 {
        self.builds
    }

    /// Overlay buffers pushed into the compositor (the transparent frame included).
    pub fn overlay_pushes(&self) -> u64 {
        self.overlay_pushes
    }

    pub fn refused_pushes(&self) -> u64 {
        self.refused_pushes
    }

    pub fn camera_missing(&self) -> u64 {
        self.camera_missing
    }

    /// Events the driver's calls (`swap_sink`, `drain`) could not deliver: the inbound channel was
    /// full, and the call never waits for room (its only reader is the caller's own loop).
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }

    /// `OutputProgress` events the output's poll could not deliver (the inbound channel was full).
    pub fn progress_dropped(&self) -> u64 {
        self.shared.progress_dropped.load(Ordering::Relaxed)
    }

    /// Buffers the programme's appsrcs refused.
    pub fn push_errors(&self) -> u64 {
        self.push_errors
    }

    pub fn stats(&self) -> Arc<Stats> {
        Arc::clone(&self.shared.stats)
    }

    /// Teardowns handed to reaper threads that have not returned yet.
    pub fn teardowns_in_flight(&self) -> usize {
        self.shared.reapers.load(Ordering::SeqCst)
    }

    /// Test hook: the counter `teardowns_in_flight` reads, to watch after a drop.
    #[cfg(feature = "test-hooks")]
    pub fn teardown_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.shared.reapers)
    }

    /// Test hook: the next `n` keyframe requests of any output are refused, as an encoder that does
    /// not take the event would (`send_event` false).
    #[cfg(feature = "test-hooks")]
    pub fn refuse_key_requests(&self, n: u32) {
        self.key_refusals.store(n, Ordering::SeqCst);
    }

    /// Test hook: the refusals `refuse_key_requests` set that no request has used yet.
    #[cfg(feature = "test-hooks")]
    pub fn key_refusals_left(&self) -> u32 {
        self.key_refusals.load(Ordering::SeqCst)
    }

    /// Test hook: holds every write of the current output's sink (a blocking probe on its sink pad):
    /// a hang inside the output pipeline (Owed 94 (ii)). The sink has no write pending, so rtmp2sink
    /// never errors and its total stays flat while the encoders keep delivering into the ring: the
    /// shape the egress backstop exists for. (Before Owed 87 it stood in for a destination that
    /// accepts and then stops reading; that destination now fails the sink's socket wait instead,
    /// which this hook never reaches.) A flush (the teardown) lets it go. False if there is no
    /// output.
    #[cfg(feature = "test-hooks")]
    pub fn hold_sink_writes(&self) -> bool {
        let Some(pad) = self.current_sink_pad() else {
            return false;
        };
        pad.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_, _| {
            gst::PadProbeReturn::Ok
        })
        .is_some()
    }

    /// Test hook (Owed 94; C14's output-hung case, through the compositor's
    /// `COMPOSITOR_TEST_HOLD_SINK_AFTER_MS` and `COMPOSITOR_TEST_HANG_SINK_AFTER_MS`): sink generation
    /// 1 stalls `after` its build, at the first push past it. Once, and only on generation 1, so the
    /// sink the swap builds next is healthy. The line saying so is the case's witness and its clock.
    #[cfg(feature = "test-hooks")]
    pub fn stall_first_sink(&mut self, stall: SinkStall, after: Duration) {
        self.sink_stall = Some(PlannedStall {
            stall,
            after,
            built_at: None,
        });
        self.shared.log(&format!(
            "WARN test hook (Owed 94): sink gen=1 stalls ({}) {} ms after its build",
            stall_name(stall),
            after.as_millis()
        ));
    }

    /// Plants the planned stall once its moment has come (called on every push).
    #[cfg(feature = "test-hooks")]
    fn plant_due_stall(&mut self) {
        let Some(plan) = self.sink_stall else {
            return;
        };
        if plan.built_at.is_none_or(|at| at.elapsed() < plan.after) {
            return;
        }
        self.sink_stall = None;
        let current = self
            .running
            .as_ref()
            .and_then(|r| r.output.as_ref())
            .map(|o| o.gen());
        let planted = current == Some(1)
            && match plan.stall {
                SinkStall::Hold => self.hold_sink_writes(),
                SinkStall::Hang => self.hang_sink(SINK_HANG_HOLD),
            };
        self.shared.log(&format!(
            "WARN test hook (Owed 94): sink stall={} gen=1 planted={planted}",
            stall_name(plan.stall)
        ));
    }

    /// Test hook: the current output sink's next write sleeps `hold` holding its streaming lock, so
    /// that `set_state(NULL)` on the output does not return before it (F-001's hung teardown).
    #[cfg(feature = "test-hooks")]
    pub fn hang_sink(&self, hold: Duration) -> bool {
        let Some(pad) = self.current_sink_pad() else {
            return false;
        };
        hang_pad(&pad, hold)
    }

    /// Test hook: the encoder's next output sleeps `hold` holding its streaming lock, so that
    /// `set_state(NULL)` on the programme does not return before it.
    #[cfg(feature = "test-hooks")]
    pub fn hang_encoder(&self, hold: Duration) -> bool {
        let Some(pad) = self.running.as_ref().and_then(|r| r.encoder.static_pad("src")) else {
            return false;
        };
        hang_pad(&pad, hold)
    }

    /// Test hook: drops the ring's oldest entries up to a video frame that is not a keyframe, as the
    /// ring does when a stalled sink outlasts it; returns how many.
    #[cfg(feature = "test-hooks")]
    pub fn drop_ring_to_delta(&self) -> usize {
        self.shared.ring.drop_to_delta()
    }

    /// Test hook: the encoded ring's entries now.
    #[cfg(feature = "test-hooks")]
    pub fn ring_entries(&self) -> usize {
        self.shared.ring.len()
    }

    /// Test hook: the bytes queued in the current output's video and audio appsrcs.
    #[cfg(feature = "test-hooks")]
    pub fn output_queued_bytes(&self) -> Option<(u64, u64)> {
        Some(self.running.as_ref()?.output.as_ref()?.queued())
    }

    /// Test hook: posts an error on the current output's bus, as a failing sink would. False if
    /// there is no output.
    #[cfg(feature = "test-hooks")]
    pub fn plant_output_error(&self) -> bool {
        self.running
            .as_ref()
            .and_then(|r| r.output.as_ref())
            .is_some_and(|o| o.plant_error())
    }

    /// Test hook: the current output's sink fails as a dropped connection does: its next write
    /// posts a write error from the sink and returns a flow error upstream, so each of the output's
    /// appsrcs posts its own error and sends an EOS down (F-011's live legs). False if there is no
    /// output.
    #[cfg(feature = "test-hooks")]
    pub fn fail_sink_writes(&self) -> bool {
        let Some(pad) = self.current_sink_pad() else {
            return false;
        };
        let posted = AtomicBool::new(false);
        pad.add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            move |pad, info| {
                if !posted.swap(true, Ordering::SeqCst) {
                    if let Some(sink) = pad.parent_element() {
                        let msg = gst::message::Error::builder(
                            gst::ResourceError::Write,
                            "planted: the connection was lost",
                        )
                        .src(&sink)
                        .build();
                        let _ = sink.post_message(msg);
                    }
                }
                info.flow_res = Err(gst::FlowError::Error);
                gst::PadProbeReturn::Handled
            },
        )
        .is_some()
    }

    /// Test hook: the first PTS each of the current output's appsrcs sent downstream, (video,
    /// audio), as the feeder rebased it. `None` if there is no output.
    #[cfg(feature = "test-hooks")]
    pub fn output_first_pts(&self) -> Option<(Option<gst::ClockTime>, Option<gst::ClockTime>)> {
        Some(self.running.as_ref()?.output.as_ref()?.first_pts())
    }

    /// Test hook: stops the base layer's appsrc (NULL), so every base push is refused (flushing)
    /// until `restart_base_layer`.
    #[cfg(feature = "test-hooks")]
    pub fn stop_base_layer(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.base.set_state(gst::State::Null).is_ok())
    }

    /// Test hook: brings the base layer's appsrc back to the pipeline's state.
    #[cfg(feature = "test-hooks")]
    pub fn restart_base_layer(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.base.sync_state_with_parent().is_ok())
    }

    /// Test hook: posts an error on the programme's bus, as a failing element would, with `debug`
    /// as its debug text. False before `start()`.
    #[cfg(feature = "test-hooks")]
    pub fn plant_programme_error(&self, debug: &str) -> bool {
        let Some(r) = self.running.as_ref() else {
            return false;
        };
        let msg = gst::message::Error::builder(gst::CoreError::Failed, "planted programme error")
            .debug(debug)
            .src(&r.pipeline)
            .build();
        r.pipeline.post_message(msg).is_ok()
    }

    /// Test hook: every RTMP sink built from now on gets `timeout` = `secs` instead of
    /// `OUTPUT_SINK_TIMEOUT_S`, so a read-back tells the adapter's set from rtmp2sink's default.
    #[cfg(feature = "test-hooks")]
    pub fn set_output_sink_timeout_s(&mut self, secs: u32) {
        self.shared.sink_timeout_s = secs;
    }

    /// Test hook: the current output sink's `timeout` property, read back from the element. `None`
    /// if there is no output, or its sink has no such property (a `file://` target's filesink).
    #[cfg(feature = "test-hooks")]
    pub fn output_sink_timeout_s(&self) -> Option<u32> {
        let sink = self.running.as_ref()?.output.as_ref()?.sink();
        sink.find_property("timeout")?;
        Some(sink.property::<u32>("timeout"))
    }

    #[cfg(feature = "test-hooks")]
    fn current_sink_pad(&self) -> Option<gst::Pad> {
        self.running.as_ref()?.output.as_ref()?.sink().static_pad("sink")
    }

    /// An event from the driver's own calls: tried once, never waited for (PR #28 review item 2, the
    /// coordinator's ruling). The driver's thread is the inbound channel's only reader, so waiting for
    /// room there never ends. A lost `OutputError` is recovered by the domain's link, which times the
    /// connect out and swaps again; a lost `OutputDrained` by its `drain_timeout_ms`.
    fn send_now(&mut self, e: Event) {
        let what = format!("{e:?}");
        if self.shared.tx.try_send(Inbound::Event(e)).is_err() {
            self.events_dropped += 1;
            self.shared.log(&format!(
                "programme: {what} not delivered: the inbound channel is full ({} dropped in all)",
                self.events_dropped
            ));
        }
    }

    fn build(&mut self, profile: &OutputProfile) -> Result<Running, String> {
        gst::init().map_err(|e| format!("gstreamer did not initialise: {e}"))?;
        let fps = profile.fps;
        if fps == 0 || !AUDIO_RATE.is_multiple_of(fps) {
            return Err(format!(
                "programme fps {fps} does not divide the {AUDIO_RATE} Hz audio into whole ticks"
            ));
        }
        let period_bytes = (AUDIO_RATE / fps) as usize * AUDIO_FRAME_BYTES;
        let (w, h) = (profile.width, profile.height);
        let slate = Slate::render(w, h)?;
        let base_caps = slate
            .slate_for(domain::programme::SlateKind::Waiting)
            .caps()
            .map(|c| c.to_owned())
            .ok_or("slate has no caps")?;
        let base_caps = with_framerate(&base_caps, fps);
        let base_info = gst_video::VideoInfo::from_caps(&base_caps).map_err(|e| format!("base caps: {e}"))?;
        let overlay_info =
            gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgra, OVERLAY_SIZE.0, OVERLAY_SIZE.1)
                .fps(gst::Fraction::new(fps as i32, 1))
                .build()
                .map_err(|e| format!("overlay caps: {e}"))?;
        let overlay_caps = overlay_info.to_caps().map_err(|e| format!("overlay caps: {e}"))?;
        let audio_caps = gst::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("rate", AUDIO_RATE as i32)
            .field("channels", AUDIO_CHANNELS as i32)
            .field("layout", "interleaved")
            .build();

        let pipeline = gst::Pipeline::new();
        let base = appsrc(&base_caps, APPSRC_TICKS * base_info.size() as u64)?;
        let overlay = appsrc(&overlay_caps, APPSRC_TICKS * overlay_info.size() as u64)?;
        // A tick after a gap pushes the missed periods' audio too (Owed 40), up to the ring's span at
        // once, so the audio appsrc holds that many beyond its usual three.
        let audio_fill_periods = (AUDIO_RING_BYTES / period_bytes) as u64;
        let audio = appsrc(
            &audio_caps,
            (APPSRC_TICKS + audio_fill_periods) * period_bytes as u64,
        )?;
        let mixer = make("compositor")?;
        mixer.set_property_from_str("background", "black");
        // The first push's index is wherever the session's tick grid stands: start there, not at 0.
        mixer.set_property_from_str("start-time-selection", "first");
        // The mix is AYUV (it keeps the overlay's alpha) and the encoder's input I420, both BT.709
        // like the slate: the base layer then never leaves YUV, and only the overlay is converted
        // from RGB. Left to negotiation, the compositor mixed in RGB and the slate's #150b36 came
        // out [13, 8, 43] after GStreamer's truncating 8-bit trips (F-010).
        let mix_caps = capsfilter(
            gst::Caps::builder("video/x-raw")
                .field("format", "AYUV")
                .field("width", w as i32)
                .field("height", h as i32)
                .field("colorimetry", "bt709")
                .build(),
        )?;
        let convert = make("videoconvert")?;
        let enc_caps = capsfilter(
            gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .field("colorimetry", "bt709")
                .build(),
        )?;
        let queue = make("queue")?;
        queue.set_property_from_str("leaky", "downstream");
        queue.set_property("max-size-buffers", fps);
        queue.set_property("max-size-bytes", 0u32);
        queue.set_property("max-size-time", 0u64);
        let encoder = make("x264enc")?;
        encoder.set_property_from_str("tune", X264_TUNE);
        encoder.set_property_from_str("speed-preset", X264_SPEED_PRESET);
        encoder.set_property("bitrate", profile.video_kbps);
        encoder.set_property("key-int-max", fps * 2);
        encoder.set_property("bframes", 0u32);
        encoder.set_property("vbv-buf-capacity", X264_VBV_MS);
        encoder.set_property("option-string", X264_OPTIONS);
        let profile_caps = capsfilter(
            gst::Caps::builder("video/x-h264")
                .field("profile", "main")
                .build(),
        )?;
        let parser = make("h264parse")?;
        parser.set_property("config-interval", -1i32);
        let avc_caps = capsfilter(
            gst::Caps::builder("video/x-h264")
                .field("stream-format", "avc")
                .field("alignment", "au")
                .build(),
        )?;
        let venc = appsink()?;
        let aconvert = make("audioconvert")?;
        let aac = make("voaacenc")?;
        aac.set_property("bitrate", AAC_BITRATE);
        let aparse = make("aacparse")?;
        let raw_aac = capsfilter(
            gst::Caps::builder("audio/mpeg")
                .field("mpegversion", 4i32)
                .field("stream-format", "raw")
                .build(),
        )?;
        let aenc = appsink()?;

        pipeline
            .add_many([
                base.upcast_ref(),
                overlay.upcast_ref(),
                &mixer,
                &mix_caps,
                &convert,
                &enc_caps,
                &queue,
                &encoder,
                &profile_caps,
                &parser,
                &avc_caps,
                venc.upcast_ref(),
                audio.upcast_ref(),
                &aconvert,
                &aac,
                &aparse,
                &raw_aac,
                aenc.upcast_ref(),
            ])
            .map_err(|e| format!("programme: {e}"))?;
        gst::Element::link_many([
            &mixer,
            &mix_caps,
            &convert,
            &enc_caps,
            &queue,
            &encoder,
            &profile_caps,
            &parser,
            &avc_caps,
            venc.upcast_ref(),
        ])
        .map_err(|e| format!("programme video chain: {e}"))?;
        gst::Element::link_many([
            audio.upcast_ref(),
            &aconvert,
            &aac,
            &aparse,
            &raw_aac,
            aenc.upcast_ref(),
        ])
        .map_err(|e| format!("programme audio chain: {e}"))?;
        link_layer(base.upcast_ref(), &mixer, 0, None)?;
        link_layer(overlay.upcast_ref(), &mixer, 1, Some((w, h)))?;

        // The drop before the encoder (C09 brief): every overrun is one EncoderOverrun.
        let tx = self.shared.tx.clone();
        queue.connect("overrun", false, move |_| {
            let _ = tx.try_send(Inbound::Event(Event::EncoderOverrun));
            None
        });
        self.wire_encoder(&venc, true);
        self.wire_encoder(&aenc, false);

        let parser_src = parser
            .static_pad("src")
            .ok_or("programme: h264parse has no src pad")?;
        let transparent = transparent_frame(&overlay_info);
        let stop = Arc::new(AtomicBool::new(false));
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("programme pipeline did not start: {e}"))?;
        self.watch_bus(&pipeline, &stop)?;
        Ok(Running {
            profile: *profile,
            period_bytes,
            pipeline,
            base,
            overlay,
            audio,
            #[cfg(feature = "test-hooks")]
            encoder,
            parser_src,
            camera_fit: Fitter::new(base_info, 0xff00_0000),
            overlay_fit: Fitter::new(overlay_info, 0x0000_0000),
            slate,
            transparent,
            noise: PinkNoise::default(),
            last_index: None,
            audio_fill_periods,
            failing: [false; 3],
            drained: false,
            output: None,
            stop,
        })
    }

    /// Every encoded sample goes into the ring with its running-time PTS; the encoder's EOS marks the
    /// ring, and both together with no output means the drain is already done.
    fn wire_encoder(&self, sink: &gst_app::AppSink, video: bool) {
        let ring = Arc::clone(&self.shared.ring);
        let stats = Arc::clone(&self.shared.stats);
        let eos_ring = Arc::clone(&self.shared.ring);
        let tx = self.shared.tx.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(entry) = running_time_entry(&sample, video) {
                        if video {
                            stats.on_encoded_video(Duration::from_nanos(entry.pts.nseconds()));
                        }
                        ring.push(entry);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .eos(move |_| {
                    eos_ring.end_of_stream(video);
                    if eos_ring.take_drained_without_output() {
                        let _ = tx.send(Inbound::Event(Event::OutputDrained));
                    }
                })
                .build(),
        );
    }

    /// A programme error is fatal: the pipeline is never rebuilt, so the domain exits (`Fatal`,
    /// once, its text scrubbed).
    fn watch_bus(&self, pipeline: &gst::Pipeline, stop: &Arc<AtomicBool>) -> Result<(), String> {
        let bus = pipeline.bus().ok_or("programme pipeline has no bus")?;
        let shared = self.shared.clone();
        let stop = Arc::clone(stop);
        thread::Builder::new()
            .name("programme-bus".to_owned())
            .spawn(move || {
                let mut failed = false;
                while !stop.load(Ordering::SeqCst) {
                    let Some(msg) = bus.timed_pop_filtered(
                        gst::ClockTime::from_mseconds(50),
                        &[gst::MessageType::Error, gst::MessageType::Warning],
                    ) else {
                        continue;
                    };
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match msg.view() {
                        gst::MessageView::Error(e) => {
                            let text = format!(
                                "programme error: {} ({})",
                                e.error(),
                                e.debug().as_deref().unwrap_or("")
                            );
                            shared.log(&text);
                            if !failed {
                                failed = true;
                                let clean = locked(&shared.scrubber).scrub(&text);
                                let detail = Some(ExitDetail::new(clean));
                                let _ = shared.tx.send(Inbound::Event(Event::Fatal { detail }));
                            }
                        }
                        gst::MessageView::Warning(w) => shared.log(&format!(
                            "programme warning: {} ({})",
                            w.error(),
                            w.debug().as_deref().unwrap_or("")
                        )),
                        _ => {}
                    }
                }
            })
            .map(|_| ())
            .map_err(|e| format!("programme: no bus thread: {e}"))
    }
}

impl ProgrammeOutput for GstProgramme {
    fn start(&mut self, profile: &OutputProfile) -> Result<(), String> {
        if self.running.is_some() || self.builds > 0 {
            return Err("programme already started".to_owned());
        }
        let running = self.build(profile)?;
        self.builds += 1;
        self.running = Some(running);
        Ok(())
    }

    fn push(&mut self, index: u64, choice: FrameChoice) {
        #[cfg(feature = "test-hooks")]
        self.plant_due_stall();
        let Some(r) = self.running.as_mut() else {
            self.refused_pushes += 1;
            return;
        };
        if r.drained || r.last_index.is_some_and(|last| index <= last) {
            self.refused_pushes += 1;
            return;
        }
        let missed = r.last_index.map_or(0, |last| index - last - 1);
        let gap = missed > 0;
        r.last_index = Some(index);
        let fps = u64::from(r.profile.fps);
        let at = |i: u64| gst::ClockTime::from_nseconds(i * 1_000_000_000 / fps);
        let (pts, next) = (at(index), at(index + 1));
        let period = next - pts;

        // The base layer. The camera slot's count goes to the stats on every tick, a slate's too, so
        // a camera tick counts only the frames since the tick before it (Owed 81 (i)).
        let camera = match choice {
            FrameChoice::Emit | FrameChoice::Repeat => {
                let (sample, written) = self.frames.camera_frame();
                self.shared.stats.on_camera_tick(written, true);
                let fitted = sample.and_then(|s| r.camera_fit.fit(&s));
                if fitted.is_none() {
                    self.camera_missing += 1;
                    if let Some(shape) = r.camera_fit.take_refusal() {
                        self.shared.log(&format!(
                            "programme: a camera frame of {shape} cannot be fitted; the slate is on air"
                        ));
                    }
                }
                fitted
            }
            FrameChoice::Slate(_) => {
                self.shared
                    .stats
                    .on_camera_tick(self.frames.camera_written(), false);
                None
            }
        };
        let base = match (camera, choice) {
            (Some(b), _) => b,
            (None, FrameChoice::Slate(kind)) => slate_buffer(&r.slate, kind),
            (None, _) => slate_buffer(&r.slate, domain::programme::SlateKind::Waiting),
        };
        let res = push_timed(&r.base, base, pts, period, gap);
        note_push(res, 0, &mut r.failing, &mut self.push_errors, &self.shared);

        // The overlay layer.
        let overlay_due = !self.half_rate || index.is_multiple_of(2);
        if overlay_due {
            let frame = self
                .frames
                .overlay()
                .and_then(|s| r.overlay_fit.fit(&s))
                .unwrap_or_else(|| r.transparent.clone());
            let covers = if self.half_rate { period * 2 } else { period };
            let res = push_timed(&r.overlay, frame, pts, covers, gap);
            note_push(res, 1, &mut r.failing, &mut self.push_errors, &self.shared);
            self.overlay_pushes += 1;
        }

        // The audio: every missed period's first (Owed 40, owner 2026-10-19: no hole, no click; the
        // compositor repeats the picture across the gap), then this tick's. Each is one period of
        // the ring's real audio, and pink noise for whatever it lacks. The ring holds 400 ms, so a
        // longer gap keeps the rest of its hole (marked DISCONT).
        let filled = missed.min(r.audio_fill_periods);
        let mut audio_gap = missed > filled;
        for m in index - filled..=index {
            let noise = &mut r.noise;
            let pcm = self
                .frames
                .take_audio_padded(r.period_bytes, |pad| noise.fill(pad));
            let buffer = gst::Buffer::from_mut_slice(pcm);
            let res = push_timed(&r.audio, buffer, at(m), at(m + 1) - at(m), audio_gap);
            note_push(res, 2, &mut r.failing, &mut self.push_errors, &self.shared);
            audio_gap = false;
        }
    }

    fn swap_sink(&mut self, gen: u64, target: &TargetSpec) {
        // The key alone, so a scrubbed line keeps the host and path an operator needs (no credential
        // is under the Scrubber's 8-character floor, `redact.rs`).
        locked(&self.shared.scrubber).add(&target.stream_key);
        let Some(r) = self.running.as_mut() else {
            self.shared
                .log(&format!("swap_sink gen={gen} before start: refused"));
            self.send_now(Event::OutputError { gen });
            return;
        };
        if let Some(old) = r.output.take() {
            old.retire(&self.shared);
        }
        // The new output starts at a keyframe encoded after its sink can send (Owed 84): its feeder
        // asks the encoder for one then, from its own thread.
        let parser_src = r.parser_src.clone();
        #[cfg(feature = "test-hooks")]
        let refusals = Arc::clone(&self.key_refusals);
        let request_key: gst_output::KeyRequest = Arc::new(move || {
            #[cfg(feature = "test-hooks")]
            if refusals
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return false;
            }
            let ev = gst_video::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            parser_src.send_event(ev)
        });
        match gst_output::build(&self.shared, gen, target, request_key) {
            Ok(out) => {
                r.output = Some(out);
                #[cfg(feature = "test-hooks")]
                if let Some(plan) = self.sink_stall.as_mut().filter(|_| gen == 1) {
                    plan.built_at = Some(Instant::now());
                }
            }
            Err(text) => {
                self.shared.log(&format!("swap_sink gen={gen}: {text}"));
                self.send_now(Event::OutputError { gen });
            }
        }
    }

    fn step_down(&mut self, rung: Rung) {
        // No rung clears the overlay or stops its pushes (owner, 2026-10-18, Owed 20): the bottom rung
        // only halves its update rate, and `Full` restores every tick.
        self.half_rate = match rung {
            Rung::Full => false,
            Rung::OverlayHalfRate => true,
        };
    }

    fn hide_overlay(&mut self) {
        // Clears the slot and sets nothing else (Owed 17): the next frame CEF delivers is on air.
        self.frames.clear_overlay();
    }

    fn drain(&mut self) {
        let Some(r) = self.running.as_mut() else {
            // Nothing was ever started, so nothing is owed to a sink.
            if self.shared.ring.take_drained() {
                self.send_now(Event::OutputDrained);
            }
            return;
        };
        if r.drained {
            return;
        }
        r.drained = true;
        for src in [&r.base, &r.overlay, &r.audio] {
            let _ = src.end_of_stream();
        }
    }
}

impl Drop for GstProgramme {
    /// Returns at once (C08 amendment 2026-10-19: within 500 ms): every pipeline goes to a reaper
    /// thread, and no thread is joined.
    fn drop(&mut self) {
        let Some(r) = self.running.take() else {
            return;
        };
        r.stop.store(true, Ordering::SeqCst);
        if let Some(out) = r.output {
            out.retire(&self.shared);
        }
        gst_output::reap(&self.shared, r.pipeline.upcast(), "programme".to_owned());
    }
}

impl fmt::Debug for GstProgramme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GstProgramme")
            .field("builds", &self.builds)
            .field("overlay_pushes", &self.overlay_pushes)
            .field("half_rate", &self.half_rate)
            .field(
                "output_gen",
                &self
                    .running
                    .as_ref()
                    .and_then(|r| r.output.as_ref().map(OutputGen::gen)),
            )
            .finish()
    }
}

fn make(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|e| format!("programme: no {name} element: {e}"))
}

fn capsfilter(caps: gst::Caps) -> Result<gst::Element, String> {
    let f = make("capsfilter")?;
    f.set_property("caps", caps);
    Ok(f)
}

fn appsrc(caps: &gst::Caps, max_bytes: u64) -> Result<gst_app::AppSrc, String> {
    let src = gst_app::AppSrc::builder()
        .caps(caps)
        .format(gst::Format::Time)
        .is_live(false)
        .do_timestamp(false)
        .block(false)
        .max_bytes(max_bytes)
        .leaky_type(gst_app::AppLeakyType::Downstream)
        .build();
    Ok(src)
}

fn appsink() -> Result<gst_app::AppSink, String> {
    let sink = gst_app::AppSink::builder().sync(false).build();
    sink.set_property("async", false);
    Ok(sink)
}

/// Links `src` to a new compositor pad at `zorder`, scaled to `size` when given.
fn link_layer(
    src: &gst::Element,
    mixer: &gst::Element,
    zorder: u32,
    size: Option<(u32, u32)>,
) -> Result<(), String> {
    let pad = mixer
        .request_pad_simple("sink_%u")
        .ok_or("programme: compositor gave no sink pad")?;
    // Pad request order is not the z order (spike trap): set it on every pad.
    pad.set_property("zorder", zorder);
    if let Some((w, h)) = size {
        pad.set_property("width", w as i32);
        pad.set_property("height", h as i32);
    }
    src.static_pad("src")
        .ok_or("programme: appsrc has no src pad")?
        .link(&pad)
        .map(|_| ())
        .map_err(|e| format!("programme: layer {zorder} link: {e:?}"))
}

fn with_framerate(caps: &gst::Caps, fps: u32) -> gst::Caps {
    let mut caps = caps.to_owned();
    if let Some(s) = caps.make_mut().structure_mut(0) {
        s.set("framerate", gst::Fraction::new(fps as i32, 1));
    }
    caps
}

fn slate_buffer(slate: &Slate, kind: domain::programme::SlateKind) -> gst::Buffer {
    slate
        .slate_for(kind)
        .buffer_owned()
        .expect("a rendered slate sample always holds its buffer")
}

/// A shallow copy of `buffer` (the memory is shared) stamped for its tick, then pushed. Never panics
/// or blocks; a refusal is the caller's to count (`note_push`).
fn push_timed(
    src: &gst_app::AppSrc,
    buffer: gst::Buffer,
    pts: gst::ClockTime,
    duration: gst::ClockTime,
    gap: bool,
) -> Result<(), gst::FlowError> {
    let mut b = buffer.copy();
    {
        let m = b.make_mut();
        m.set_pts(pts);
        m.set_dts(gst::ClockTime::NONE);
        m.set_duration(duration);
        if gap {
            m.set_flags(gst::BufferFlags::DISCONT);
        } else {
            m.unset_flags(gst::BufferFlags::DISCONT);
        }
    }
    src.push_buffer(b).map(|_| ())
}

/// One layer's push result (brief Step 6: "push_buffer errors are logged, redacted and counted"):
/// every refusal is counted, and the first of a run is logged, through the scrubbing log, so a
/// layer that keeps failing is one line, not one a tick.
fn note_push(
    res: Result<(), gst::FlowError>,
    layer: usize,
    failing: &mut [bool; 3],
    errors: &mut u64,
    shared: &Shared,
) {
    match res {
        Ok(()) => failing[layer] = false,
        Err(e) => {
            *errors += 1;
            if !std::mem::replace(&mut failing[layer], true) {
                shared.log(&format!(
                    "programme: the {} layer's push failed ({e:?}); each refusal is counted, and this \
                     is said again only after the layer recovers",
                    LAYERS[layer]
                ));
            }
        }
    }
}

/// The encoded `sample` as a ring entry, its PTS and DTS converted to running time through its own
/// segment (x264enc's output segment starts at 1 000 h: spike trap, F-001's chain).
fn running_time_entry(sample: &gst::Sample, video: bool) -> Option<Entry> {
    let buffer = sample.buffer()?;
    let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
    let pts = segment.to_running_time(buffer.pts()?)?;
    let dts = buffer.dts().and_then(|d| segment.to_running_time(d));
    let key = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
    let mut copy = buffer.copy();
    {
        let m = copy.make_mut();
        m.set_pts(pts);
        m.set_dts(dts);
    }
    let mut builder = gst::Sample::builder().buffer(&copy);
    let caps = sample.caps_owned();
    if let Some(caps) = &caps {
        builder = builder.caps(caps);
    }
    Some(Entry::new(builder.build(), video, key, pts))
}

/// BGRA zeros: alpha 0 everywhere.
fn transparent_frame(info: &gst_video::VideoInfo) -> gst::Buffer {
    gst::Buffer::from_mut_slice(vec![0u8; info.size()])
}

/// Owed 94: the hook's word for a stall, in its log lines.
#[cfg(feature = "test-hooks")]
fn stall_name(s: SinkStall) -> &'static str {
    match s {
        SinkStall::Hold => "hold",
        SinkStall::Hang => "hang",
    }
}

/// The test hooks' sleeping probe: `hold` inside the streaming thread, once.
#[cfg(feature = "test-hooks")]
fn hang_pad(pad: &gst::Pad, hold: Duration) -> bool {
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        thread::sleep(hold);
        gst::PadProbeReturn::Remove
    })
    .is_some()
}

/// Converts a sample whose caps differ from the programme's into its exact format and size, so the
/// programme's caps never renegotiate (Review Focus 1). The picture keeps its aspect ratio, centred,
/// and the border is `border_argb` (black under the camera, transparent under the overlay).
struct Fitter {
    target: gst_video::VideoInfo,
    border_argb: u32,
    /// The converter for the last input shape.
    converter: Option<(
        gst_video::VideoInfo,
        gst_video::VideoConverter,
        gst_video::VideoInfo,
    )>,
    /// The last input buffer converted, and its conversion (a `Repeat` tick re-pushes the same
    /// camera sample, which is converted once). The input is HELD, not just its address noted: a
    /// buffer pool recycles a buffer nothing holds, so the next frame could arrive in the same
    /// `GstBuffer` with new pixels (PR #28 review item 6, the coordinator's ruling). Held, it cannot
    /// be recycled, and an equal address is the same frame.
    last: Option<(gst::Buffer, gst::Buffer)>,
    /// The last input shape that could not be fitted, and whether it is still to be logged.
    refused: Option<(gst_video::VideoInfo, bool)>,
}

impl Fitter {
    fn new(target: gst_video::VideoInfo, border_argb: u32) -> Self {
        Self {
            target,
            border_argb,
            converter: None,
            last: None,
            refused: None,
        }
    }

    /// `sample` as one of the programme's frames, or `None` when it has no buffer or caps or cannot
    /// be converted (a refused shape is noted once for `take_refusal`).
    fn fit(&mut self, sample: &gst::Sample) -> Option<gst::Buffer> {
        let buffer = sample.buffer_owned()?;
        let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
        if same_shape(&info, &self.target) {
            return Some(buffer);
        }
        if let Some((held, out)) = &self.last {
            if held.as_ptr() == buffer.as_ptr() {
                return Some(out.clone());
            }
        }
        let fitted = self.convert(buffer.clone(), &info);
        match &fitted {
            Some(out) => self.last = Some((buffer, out.clone())),
            None => {
                let new = !self.refused.as_ref().is_some_and(|(i, _)| same_shape(i, &info));
                if new {
                    self.refused = Some((info, true));
                }
            }
        }
        fitted
    }

    fn convert(&mut self, buffer: gst::Buffer, info: &gst_video::VideoInfo) -> Option<gst::Buffer> {
        if !self
            .converter
            .as_ref()
            .is_some_and(|(i, _, _)| same_shape(i, info) && i.fps() == info.fps())
        {
            let (conv, dest) = self.converter_for(info)?;
            self.converter = Some((info.clone(), conv, dest));
        }
        let (_, conv, dest) = self.converter.as_mut()?;
        let in_frame = gst_video::VideoFrame::from_buffer_readable(buffer, info).ok()?;
        let out = gst::Buffer::with_size(dest.size()).ok()?;
        let mut out_frame = gst_video::VideoFrame::from_buffer_writable(out, dest).ok()?;
        conv.frame(&in_frame, &mut out_frame);
        Some(out_frame.into_buffer())
    }

    /// The shape last refused, described once (the caller logs it; a camera that keeps sending it
    /// is not a line per tick).
    fn take_refusal(&mut self) -> Option<String> {
        let (info, pending) = self.refused.as_mut()?;
        if !std::mem::replace(pending, false) {
            return None;
        }
        Some(format!(
            "{:?} {}x{} {}",
            info.format(),
            info.width(),
            info.height(),
            info.colorimetry()
        ))
    }

    /// A converter from `from` into the target box, and the target as the converter sees it: with
    /// the input's frame rate, because `VideoConverter` refuses a rate change (it converts one frame;
    /// the programme stamps its own times).
    fn converter_for(
        &self,
        from: &gst_video::VideoInfo,
    ) -> Option<(gst_video::VideoConverter, gst_video::VideoInfo)> {
        let (tw, th) = (self.target.width(), self.target.height());
        let (fw, fh) = (from.width().max(1), from.height().max(1));
        // The largest box of the input's aspect that fits the target, centred.
        let (dw, dh) = if u64::from(fw) * u64::from(th) >= u64::from(fh) * u64::from(tw) {
            (
                tw,
                ((u64::from(tw) * u64::from(fh) / u64::from(fw)) as u32).min(th),
            )
        } else {
            (
                ((u64::from(th) * u64::from(fw) / u64::from(fh)) as u32).min(tw),
                th,
            )
        };
        let (dw, dh) = (dw & !1, dh & !1);
        let mut config = gst_video::VideoConverterConfig::new();
        config.set_dest_x((((tw - dw) / 2) & !1) as i32);
        config.set_dest_y((((th - dh) / 2) & !1) as i32);
        config.set_dest_width(Some(dw as i32));
        config.set_dest_height(Some(dh as i32));
        config.set_fill_border(true);
        config.set_border_argb(self.border_argb);
        let mut caps = self.target.to_caps().ok()?;
        caps.get_mut()?.set("framerate", from.fps());
        let dest = gst_video::VideoInfo::from_caps(&caps).ok()?;
        let conv = gst_video::VideoConverter::new(from, &dest, Some(config)).ok()?;
        Some((conv, dest))
    }
}

/// The fields that make a frame interchangeable with the programme's: its format, size and colours
/// (the frame rate does not matter: the programme stamps its own).
fn same_shape(a: &gst_video::VideoInfo, b: &gst_video::VideoInfo) -> bool {
    a.format() == b.format()
        && a.width() == b.width()
        && a.height() == b.height()
        && a.colorimetry() == b.colorimetry()
}

/// Pink noise at `NOISE_PEAK` (F-004's filler), S16LE stereo: a xorshift white source through Paul
/// Kellet's three-pole "economy" pink filter. Deterministic, so a session's filler is reproducible.
struct PinkNoise {
    state: u32,
    b: [f32; 3],
}

impl Default for PinkNoise {
    fn default() -> Self {
        Self {
            state: 0x9e37_79b9,
            b: [0.0; 3],
        }
    }
}

impl PinkNoise {
    fn white(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn sample(&mut self) -> f32 {
        let w = self.white();
        self.b[0] = 0.997_65 * self.b[0] + w * 0.099_046;
        self.b[1] = 0.963 * self.b[1] + w * 0.296_516_4;
        self.b[2] = 0.57 * self.b[2] + w * 1.052_691_3;
        // The filter's peak gain is about 4; scale it into [-1, 1] before the level.
        ((self.b[0] + self.b[1] + self.b[2] + w * 0.184_8) * 0.25).clamp(-1.0, 1.0)
    }

    /// Fills `pad` (whole S16LE stereo frames) with noise, the same sample on both channels.
    fn fill(&mut self, pad: &mut [u8]) {
        for frame in pad.chunks_exact_mut(AUDIO_FRAME_BYTES) {
            let v = (self.sample() * NOISE_PEAK * f32::from(i16::MAX)).round() as i16;
            let [lo, hi] = v.to_le_bytes();
            frame.copy_from_slice(&[lo, hi, lo, hi]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PinkNoise;

    // F-004: the filler is ~−60 dBFS noise, never digital zero. Expected values from the finding, not
    // from the generator: a peak at or under 0.001 of full scale (about 33 counts), an RMS between
    // −75 and −55 dBFS, and almost no sample at exactly 0.
    #[test]
    fn the_filler_is_low_level_noise_not_digital_silence() {
        let mut n = PinkNoise::default();
        let mut pad = vec![0u8; 48_000 * 4];
        n.fill(&mut pad);
        let samples: Vec<i16> = pad
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        let rms = (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt();
        let dbfs = 20.0 * (rms / f64::from(i16::MAX)).log10();
        let zeros = samples.iter().filter(|&&s| s == 0).count() as f64 / samples.len() as f64;
        assert!(peak <= 33, "peak {peak}");
        assert!((-75.0..=-55.0).contains(&dbfs), "rms {dbfs:.1} dBFS");
        assert!(zeros < 0.2, "zero samples {zeros:.3}");
    }
}
