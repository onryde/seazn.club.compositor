//! The input pipeline (C10; spec §3.3's refinement): Cloudflare Input A's SRT playback, torn down and
//! rebuilt on every `rebuild(gen, spec)`, so the programme pipeline never sees a reconnect:
//!
//! ```text
//! srtsrc uri=<srt_uri> → tsdemux ┬→ queue(leaky, 1 s) → h264parse → avdec_h264 → videoconvertscale
//!                                │     → I420 W×H, PAR 1/1, BT.709 → appsink(video)
//!                                └→ queue(leaky, 400 ms) → aacparse → avdec_aac → audioconvert
//!                                      → audioresample → S16LE 48 kHz stereo → appsink(audio)
//! ```
//!
//! **Letterboxed fixed caps** (Review Focus 1). `videoconvertscale` scales every camera frame into the
//! programme's own W×H with square pixels, adding black borders where the aspect differs, so a phone
//! rotated mid-match or a CF transcode change never reaches the programme as a caps change. The caps
//! carry BT.709 too, the programme's base-layer colours (F-010), so its fitter takes the frame as it is.
//!
//! **Events**, each tagged with the generation of the pipeline that sent it: the first decoded video
//! sample sends `InputConnected { gen }` (F-002: "connected" is the first decoded frame, never the
//! handshake); every video sample replaces the frame store's camera slot and sends `InputFrame`; every
//! audio sample goes into the frame store's 400 ms ring; a bus error sends `InputLost { gen, Error }`
//! and the video branch's EOS `InputLost { gen, Eos }`, once a generation. `InputFrame` and
//! `InputConnected` are tried, never waited for, so a full inbound channel can never hold the camera
//! slot still: a dropped frame is counted, and `InputConnected` is tried again on the next sample.
//!
//! **No watchdog and no retry here** (AGENTS.md §4): the domain owns the stall rule, the connect
//! timeout and the back-off. So `srtsrc`'s own `auto-reconnect`, on by default in 1.24, is switched
//! off: with it on, a peer that goes away is retried inside the element forever and never reported.
//!
//! **The gate.** One field holds the build whose samples and events may pass. It closes, before the
//! call returns or the event is sent, on `rebuild` (for the old build), on `stop`, on the drop, and
//! when this adapter reports a generation lost; a sample from a closed build is dropped and counted.
//! `InputFrame` carries no generation, so the gate is what keeps a torn-down pipeline's late frames
//! out of the slot and off the loop.
//!
//! **Never blocks the caller.** `set_state(PLAYING)` runs on a starter thread: `srtsrc` resolves its
//! host there, and a hanging resolver held it for 10 s (`_FINDINGS.md` F-012). Teardown runs on a
//! reaper thread (F-001: `set_state(NULL)` did not return for 10 s on a hung renderer); nothing here
//! joins a thread.
//!
//! **Secrets.** The URI carries the SRT stream id and passphrase, so it is never logged: each build logs
//! the URL's scheme and authority alone, and every GStreamer text this adapter forwards passes the
//! scrubber, which knows both values by value.
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use app::ports::{redacted_origin, Inbound, InputSource, InputSpec};
use crossbeam_channel::Sender;
use domain::input_link::LossReason;
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::frame_store::{FrameStore, AUDIO_CHANNELS, AUDIO_RATE};
use crate::gst_output::LogFn;
use crate::redact::{percent_encoded, Scrubber};

/// The leaky queue before the video decoder holds at most 1 s (plan, Global Constraints: "decoded video
/// 1 s … leaky, dropping the oldest"; the C10 brief puts it between the demuxer and the decoder).
pub const VIDEO_QUEUE_NS: u64 = 1_000_000_000;
/// The leaky queue before the audio decoder holds at most 400 ms (plan: "audio 400 ms").
pub const AUDIO_QUEUE_NS: u64 = 400_000_000;
/// Each appsink keeps at most two samples and drops the oldest (C10 brief, Step 3).
pub const APPSINK_MAX_BUFFERS: u32 = 2;
/// How long the bus thread waits for one message before looking at its stop flag again.
const BUS_WAIT_MS: u64 = 50;
/// RFC 3986 upper-case hex, the form `Scrubber::add` also learns.
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn stderr_log() -> LogFn {
    // C12 replaces this with its `ts=… level=info msg=…` sink (`with_log`).
    Arc::new(|line: &str| eprintln!("input: {line}"))
}

/// What the pipeline threads share with the adapter.
struct Shared {
    /// The build whose samples and events may pass; `None` while no generation is live.
    gate: Mutex<Option<u64>>,
    tx: Sender<Inbound>,
    frames: FrameStore,
    log: LogFn,
    scrubber: Arc<Mutex<Scrubber>>,
    /// Teardowns (`set_state(NULL)` on a reaper thread) not yet returned.
    reapers: Arc<AtomicUsize>,
    /// `InputFrame` events not delivered: the inbound channel was full.
    frames_dropped: AtomicU64,
    /// The inbound channel was full at the last frame (logged once a run).
    dropping: AtomicBool,
    /// Samples that arrived from a closed build and were dropped.
    gate_dropped: AtomicU64,
    /// The generation of the last frame that reached the camera slot (0: none yet).
    last_frame_gen: AtomicU64,
    /// Test hook: each reaper waits this many ms before its `set_state(NULL)` (0: none).
    #[cfg(feature = "test-hooks")]
    reaper_delay_ms: AtomicU64,
    /// Test hook: each starter waits this many ms before its `set_state(PLAYING)` (0: none).
    #[cfg(feature = "test-hooks")]
    starter_delay_ms: AtomicU64,
}

impl Shared {
    fn log(&self, line: &str) {
        let clean = locked(&self.scrubber).scrub(line);
        (self.log)(&clean);
    }

    /// Reports `gen` lost, once: only while `build` is the open one, and the gate closes first, so
    /// nothing of that build reaches the slot or the loop afterwards. The event waits for room: this
    /// runs on the pipeline's own threads, never the driver's.
    fn lose(&self, build: u64, gen: u64, reason: LossReason) {
        {
            let mut open = locked(&self.gate);
            if *open != Some(build) {
                return;
            }
            *open = None;
        }
        self.log(&format!("gen={gen}: lost ({reason:?})"));
        let _ = self.tx.send(Inbound::Event(Event::InputLost { gen, reason }));
    }
}

/// The live `InputSource` (C10).
pub struct GstInput {
    shared: Arc<Shared>,
    latency_ms: u64,
    out_w: u32,
    out_h: u32,
    current: Option<Live>,
    /// Builds started so far (each `rebuild` that built a pipeline).
    builds: u64,
    stopped: bool,
    /// `rebuild` calls refused: after `stop`.
    refused_rebuilds: u64,
    /// Events this (the driver's) thread could not deliver: the inbound channel was full.
    events_dropped: u64,
}

/// One generation's pipeline.
struct Live {
    gen: u64,
    pipeline: gst::Pipeline,
    /// Ends this generation's bus thread.
    stop: Arc<AtomicBool>,
}

impl GstInput {
    /// `latency_ms` is `Thresholds::input_srt_latency_ms`; `out_w × out_h` is the programme's size
    /// (`OutputProfile`), which every camera frame is scaled into.
    pub fn new(frames: FrameStore, tx: Sender<Inbound>, latency_ms: u64, out_w: u32, out_h: u32) -> Self {
        Self {
            shared: Arc::new(Shared {
                gate: Mutex::new(None),
                tx,
                frames,
                log: stderr_log(),
                scrubber: Arc::new(Mutex::new(Scrubber::default())),
                reapers: Arc::new(AtomicUsize::new(0)),
                frames_dropped: AtomicU64::new(0),
                dropping: AtomicBool::new(false),
                gate_dropped: AtomicU64::new(0),
                last_frame_gen: AtomicU64::new(0),
                #[cfg(feature = "test-hooks")]
                reaper_delay_ms: AtomicU64::new(0),
                #[cfg(feature = "test-hooks")]
                starter_delay_ms: AtomicU64::new(0),
            }),
            latency_ms,
            out_w,
            out_h,
            current: None,
            builds: 0,
            stopped: false,
            refused_rebuilds: 0,
            events_dropped: 0,
        }
    }

    /// Routes this adapter's log lines (each one scrubbed of every credential it knows) to `log`
    /// instead of stderr. C12 wires its log sink here. Call it before the first `rebuild`.
    pub fn with_log(mut self, log: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            shared.log = log;
        }
        self
    }

    /// `srt://host:port?mode=caller&streamid=<id>&passphrase=<p>&latency=<ms>`, with the stream id and
    /// the passphrase percent-encoded (RFC 3986, upper-case hex; GStreamer's URI parser decodes them),
    /// and each of the two keys left out when its value is empty: libsrt refuses a present passphrase
    /// shorter than 10 characters, and an empty stream id is no stream id. **It holds both
    /// credentials: never log it.**
    pub fn srt_uri(spec: &InputSpec, latency_ms: u64) -> String {
        let mut uri = format!("{}?mode=caller", spec.url);
        if !spec.stream_id.is_empty() {
            uri.push_str("&streamid=");
            uri.push_str(&percent_encoded(&spec.stream_id, HEX_UPPER));
        }
        if !spec.passphrase.is_empty() {
            uri.push_str("&passphrase=");
            uri.push_str(&percent_encoded(&spec.passphrase, HEX_UPPER));
        }
        uri.push_str(&format!("&latency={latency_ms}"));
        uri
    }

    /// Pipelines built so far.
    pub fn builds(&self) -> u64 {
        self.builds
    }

    /// The generation of the last frame that reached the camera slot; `None` before the first.
    pub fn generation_of_last_frame(&self) -> Option<u64> {
        match self.shared.last_frame_gen.load(Ordering::SeqCst) {
            0 => None,
            g => Some(g),
        }
    }

    /// `InputFrame` events not delivered because the inbound channel was full.
    pub fn frames_dropped(&self) -> u64 {
        self.shared.frames_dropped.load(Ordering::Relaxed)
    }

    /// Samples dropped at the gate: they came from a build that was torn down, stopped or lost.
    pub fn gate_dropped(&self) -> u64 {
        self.shared.gate_dropped.load(Ordering::Relaxed)
    }

    /// `rebuild` calls refused (after `stop`).
    pub fn refused_rebuilds(&self) -> u64 {
        self.refused_rebuilds
    }

    /// Events the driver's own calls could not deliver: the inbound channel was full.
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
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

    /// Test hook: the current pipeline's next decoded video frame sleeps `hold` inside the video
    /// branch's streaming thread, holding its stream lock: the decoder hangs, and `set_state(NULL)`
    /// does not return before the sleep ends (F-001's hung teardown). False if there is no pipeline.
    #[cfg(feature = "test-hooks")]
    pub fn hang_decoder(&self, hold: Duration) -> bool {
        let Some(pad) = self.element("vdec").and_then(|d| d.static_pad("src")) else {
            return false;
        };
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            thread::sleep(hold);
            gst::PadProbeReturn::Remove
        })
        .is_some()
    }

    /// Test hook: every later teardown waits `hold` before its `set_state(NULL)`, so a retired pipeline
    /// keeps delivering samples, which the gate must drop.
    #[cfg(feature = "test-hooks")]
    pub fn delay_teardowns(&self, hold: Duration) {
        self.shared
            .reaper_delay_ms
            .store(hold.as_millis() as u64, Ordering::SeqCst);
    }

    /// Test hook: every later start waits `hold` before its `set_state(PLAYING)`, standing in for
    /// `srtsrc` resolving a host on a hanging resolver.
    #[cfg(feature = "test-hooks")]
    pub fn delay_starts(&self, hold: Duration) {
        self.shared
            .starter_delay_ms
            .store(hold.as_millis() as u64, Ordering::SeqCst);
    }

    /// Test hook: posts an error from the current pipeline's SRT source, with `debug` as its debug
    /// text, as a failing element would. The pipeline keeps running. False if there is no pipeline.
    #[cfg(feature = "test-hooks")]
    pub fn plant_error(&self, debug: &str) -> bool {
        let Some(src) = self.element("src") else {
            return false;
        };
        let msg = gst::message::Error::builder(gst::ResourceError::Read, "planted input error")
            .debug(debug)
            .src(&src)
            .build();
        src.post_message(msg).is_ok()
    }

    /// Test hook: sends an EOS into the current pipeline at its SRT source, as a source that ended
    /// would push one. False if there is no pipeline.
    #[cfg(feature = "test-hooks")]
    pub fn plant_eos(&self) -> bool {
        self.element("src")
            .is_some_and(|src| src.send_event(gst::event::Eos::new()))
    }

    /// Test hook: the current pipeline, to read how its elements were set.
    #[cfg(feature = "test-hooks")]
    pub fn pipeline(&self) -> Option<gst::Pipeline> {
        self.current.as_ref().map(|l| l.pipeline.clone())
    }

    #[cfg(feature = "test-hooks")]
    fn element(&self, name: &str) -> Option<gst::Element> {
        self.current.as_ref()?.pipeline.by_name(name)
    }

    /// An event from the driver's own calls: tried once, never waited for (C09 review item 2, the
    /// coordinator's ruling: the driver's thread is the inbound channel's only reader). A lost
    /// `InputLost` costs one connect timeout: the domain's link gives the build up and backs off.
    fn send_now(&mut self, e: Event) {
        let what = format!("{e:?}");
        if self.shared.tx.try_send(Inbound::Event(e)).is_err() {
            self.events_dropped += 1;
            self.shared.log(&format!(
                "{what} not delivered: the inbound channel is full ({} dropped in all)",
                self.events_dropped
            ));
        }
    }

    /// Closes the gate and hands the current pipeline, if any, to a reaper. Returns at once.
    fn retire_current(&mut self) {
        *locked(&self.shared.gate) = None;
        if let Some(live) = self.current.take() {
            live.stop.store(true, Ordering::SeqCst);
            reap(&self.shared, live.pipeline, live.gen);
        }
    }

    fn build(&self, gen: u64, build: u64, spec: &InputSpec) -> Result<Live, String> {
        gst::init().map_err(|e| format!("gstreamer did not initialise: {e}"))?;
        let pipeline = gst::Pipeline::with_name(&format!("input-{gen}"));
        let src = make("srtsrc", "src")?;
        src.set_property("uri", Self::srt_uri(spec, self.latency_ms));
        // The domain owns reconnection (AGENTS.md §4): a lost peer is reported, never retried here.
        src.set_property("auto-reconnect", false);
        let demux = make("tsdemux", "demux")?;

        let vqueue = leaky_queue("vqueue", VIDEO_QUEUE_NS)?;
        let vparse = make("h264parse", "vparse")?;
        let vdec = make("avdec_h264", "vdec")?;
        let scale = make("videoconvertscale", "scale")?;
        // Black borders keep the camera's aspect inside the fixed box (its default, set so it is
        // stated: the fixed caps below are what keep the programme's caps still).
        scale.set_property("add-borders", true);
        let vcaps = capsfilter(
            "vcaps",
            gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .field("width", self.out_w as i32)
                .field("height", self.out_h as i32)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .field("colorimetry", "bt709")
                .build(),
        )?;
        let vsink = appsink("vsink");

        let aqueue = leaky_queue("aqueue", AUDIO_QUEUE_NS)?;
        let aparse = make("aacparse", "aparse")?;
        let adec = make("avdec_aac", "adec")?;
        let aconvert = make("audioconvert", "aconvert")?;
        let aresample = make("audioresample", "aresample")?;
        let acaps = capsfilter(
            "acaps",
            gst::Caps::builder("audio/x-raw")
                .field("format", "S16LE")
                .field("rate", AUDIO_RATE as i32)
                .field("channels", AUDIO_CHANNELS as i32)
                .field("layout", "interleaved")
                .build(),
        )?;
        let asink = appsink("asink");

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
                &aqueue,
                &aparse,
                &adec,
                &aconvert,
                &aresample,
                &acaps,
                asink.upcast_ref(),
            ])
            .map_err(|e| format!("input: {e}"))?;
        src.link(&demux).map_err(|e| format!("input source link: {e}"))?;
        gst::Element::link_many([&vqueue, &vparse, &vdec, &scale, &vcaps, vsink.upcast_ref()])
            .map_err(|e| format!("input video chain: {e}"))?;
        gst::Element::link_many([
            &aqueue,
            &aparse,
            &adec,
            &aconvert,
            &aresample,
            &acaps,
            asink.upcast_ref(),
        ])
        .map_err(|e| format!("input audio chain: {e}"))?;

        self.link_demux(&demux, &vqueue, &aqueue, gen);
        self.wire_video(&vsink, build, gen);
        self.wire_audio(&asink, build);
        let stop = Arc::new(AtomicBool::new(false));
        self.watch_bus(&pipeline, &stop, build, gen)?;
        Ok(Live { gen, pipeline, stop })
    }

    /// `tsdemux`'s pads are dynamic: the first H.264 pad goes to the video queue and the first AAC pad
    /// to the audio queue; anything else is left unlinked and logged once a pad.
    fn link_demux(&self, demux: &gst::Element, vqueue: &gst::Element, aqueue: &gst::Element, gen: u64) {
        let shared = Arc::clone(&self.shared);
        let (vq, aq) = (vqueue.clone(), aqueue.clone());
        demux.connect_pad_added(move |_, pad| {
            let caps = pad.current_caps().or_else(|| pad.allowed_caps());
            let name = caps
                .as_ref()
                .and_then(|c| c.structure(0))
                .map(|s| s.name().to_string())
                .unwrap_or_default();
            let target = match name.as_str() {
                "video/x-h264" => Some(&vq),
                "audio/mpeg" => Some(&aq),
                _ => None,
            };
            let linked = target
                .and_then(|q| q.static_pad("sink"))
                .filter(|sink| !sink.is_linked())
                .map(|sink| pad.link(&sink));
            match linked {
                Some(Ok(_)) => {}
                Some(Err(e)) => shared.log(&format!("gen={gen}: a {name} stream did not link ({e:?})")),
                None => shared.log(&format!("gen={gen}: a {name} stream is not used")),
            }
        });
    }

    fn wire_video(&self, sink: &gst_app::AppSink, build: u64, gen: u64) {
        let shared = Arc::clone(&self.shared);
        let eos = Arc::clone(&self.shared);
        let connected = AtomicBool::new(false);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let open = locked(&shared.gate);
                    if *open != Some(build) {
                        shared.gate_dropped.fetch_add(1, Ordering::Relaxed);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    shared.frames.set_camera(sample);
                    shared.last_frame_gen.store(gen, Ordering::SeqCst);
                    if !connected.load(Ordering::SeqCst)
                        && shared
                            .tx
                            .try_send(Inbound::Event(Event::InputConnected { gen }))
                            .is_ok()
                    {
                        connected.store(true, Ordering::SeqCst);
                    }
                    let delivered = shared.tx.try_send(Inbound::Event(Event::InputFrame)).is_ok();
                    drop(open);
                    note_frame(&shared, delivered);
                    Ok(gst::FlowSuccess::Ok)
                })
                .eos(move |_| eos.lose(build, gen, LossReason::Eos))
                .build(),
        );
    }

    fn wire_audio(&self, sink: &gst_app::AppSink, build: u64) {
        let shared = Arc::clone(&self.shared);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let Some(buffer) = sample.buffer() else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let Ok(map) = buffer.map_readable() else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let open = locked(&shared.gate);
                    if *open != Some(build) {
                        shared.gate_dropped.fetch_add(1, Ordering::Relaxed);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    shared.frames.push_audio(map.as_slice());
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    /// A bus error is this generation's loss (`Error`); every error and warning text is logged after
    /// the scrubber.
    fn watch_bus(
        &self,
        pipeline: &gst::Pipeline,
        stop: &Arc<AtomicBool>,
        build: u64,
        gen: u64,
    ) -> Result<(), String> {
        let bus = pipeline.bus().ok_or("input pipeline has no bus")?;
        let shared = Arc::clone(&self.shared);
        let stop = Arc::clone(stop);
        thread::Builder::new()
            .name("input-bus".to_owned())
            .spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Some(msg) = bus.timed_pop_filtered(
                        gst::ClockTime::from_mseconds(BUS_WAIT_MS),
                        &[gst::MessageType::Error, gst::MessageType::Warning],
                    ) else {
                        continue;
                    };
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match msg.view() {
                        gst::MessageView::Error(e) => {
                            shared.log(&format!(
                                "gen={gen}: error: {} ({})",
                                e.error(),
                                e.debug().as_deref().unwrap_or("")
                            ));
                            shared.lose(build, gen, LossReason::Error);
                        }
                        gst::MessageView::Warning(w) => shared.log(&format!(
                            "gen={gen}: warning: {} ({})",
                            w.error(),
                            w.debug().as_deref().unwrap_or("")
                        )),
                        _ => {}
                    }
                }
            })
            .map(|_| ())
            .map_err(|e| format!("input: no bus thread: {e}"))
    }

    /// `set_state(PLAYING)` on a starter thread (F-012). A refusal is logged here; the loss is the
    /// error the refusing element posts on the bus (measured: `srtsrc`'s "Could not open resource"
    /// 20 ms in), so there is one road to `InputLost`, and the domain's connect timeout behind it.
    fn start(&mut self, live: &Live) {
        let shared = Arc::clone(&self.shared);
        let pipeline = live.pipeline.clone();
        let gen = live.gen;
        let spawned = thread::Builder::new()
            .name("input-start".to_owned())
            .spawn(move || {
                #[cfg(feature = "test-hooks")]
                {
                    let hold = shared.starter_delay_ms.load(Ordering::SeqCst);
                    if hold > 0 {
                        thread::sleep(Duration::from_millis(hold));
                    }
                }
                if let Err(e) = pipeline.set_state(gst::State::Playing) {
                    shared.log(&format!("gen={gen}: did not start: {e}"));
                }
            });
        if spawned.is_err() {
            *locked(&self.shared.gate) = None;
            self.shared
                .log(&format!("gen={gen}: no starter thread; the generation is lost"));
            self.send_now(Event::InputLost {
                gen,
                reason: LossReason::Error,
            });
        }
    }
}

impl InputSource for GstInput {
    fn rebuild(&mut self, gen: u64, spec: &InputSpec) {
        if self.stopped {
            self.refused_rebuilds += 1;
            self.shared
                .log(&format!("gen={gen}: rebuild refused: the input was stopped"));
            return;
        }
        {
            let mut scrubber = locked(&self.shared.scrubber);
            scrubber.add(&spec.stream_id);
            scrubber.add(&spec.passphrase);
        }
        self.retire_current();
        if let Err(why) = check_url(&spec.url) {
            self.shared.log(&format!("gen={gen}: {why}"));
            self.send_now(Event::InputLost {
                gen,
                reason: LossReason::Error,
            });
            return;
        }
        let build = self.builds + 1;
        match self.build(gen, build, spec) {
            Ok(live) => {
                self.builds = build;
                self.shared.log(&format!(
                    "gen={gen}: {} latency={}",
                    redacted_origin(&spec.url),
                    self.latency_ms
                ));
                *locked(&self.shared.gate) = Some(build);
                self.start(&live);
                self.current = Some(live);
            }
            Err(text) => {
                self.shared.log(&format!("gen={gen}: {text}"));
                self.send_now(Event::InputLost {
                    gen,
                    reason: LossReason::Error,
                });
            }
        }
    }

    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.retire_current();
    }
}

impl Drop for GstInput {
    /// Returns at once (C08 amendment 2026-10-19: within 500 ms): the gate closes, the pipeline goes
    /// to a reaper thread, and no thread is joined.
    fn drop(&mut self) {
        self.retire_current();
    }
}

impl fmt::Debug for GstInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GstInput")
            .field("builds", &self.builds)
            .field("gen", &self.current.as_ref().map(|l| l.gen))
            .field("stopped", &self.stopped)
            .field("latency_ms", &self.latency_ms)
            .field("out", &(self.out_w, self.out_h))
            .finish()
    }
}

/// `srt://host:port` and nothing else (contract `session.input.url`): no path, query, fragment or
/// userinfo, since the stream id and passphrase travel as their own fields. The refusal's text holds
/// the URL's scheme and authority at most.
fn check_url(url: &str) -> Result<(), String> {
    let origin = redacted_origin(url);
    let scheme_ok = url.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("srt://"));
    let port_ok = url
        .get(6..)
        .and_then(|rest| rest.rsplit_once(':'))
        .is_some_and(|(host, port)| {
            !host.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
        });
    if origin == url && scheme_ok && port_ok {
        Ok(())
    } else {
        Err(format!("refused: the input url {origin} is not srt://host:port"))
    }
}

/// An `InputFrame` the channel took or refused: a refusal is counted, and the first of a run and the
/// end of the run are logged (with the count), so a stalled loop is two lines, not thirty a second.
fn note_frame(shared: &Shared, delivered: bool) {
    if delivered {
        if shared.dropping.swap(false, Ordering::SeqCst) {
            shared.log(&format!(
                "the inbound channel takes frames again ({} dropped in all)",
                shared.frames_dropped.load(Ordering::Relaxed)
            ));
        }
        return;
    }
    shared.frames_dropped.fetch_add(1, Ordering::Relaxed);
    if !shared.dropping.swap(true, Ordering::SeqCst) {
        shared
            .log("the inbound channel is full: frames still reach the camera slot, their events are dropped");
    }
}

/// `set_state(NULL)` on a reaper thread of its own, counted while it runs; nothing waits for it.
/// (`gst_output::reap` does the same for the programme's pipelines, but takes the programme's state.)
fn reap(shared: &Arc<Shared>, pipeline: gst::Pipeline, gen: u64) {
    shared.reapers.fetch_add(1, Ordering::SeqCst);
    let bg = Arc::clone(shared);
    let spawned = thread::Builder::new()
        .name("input-reaper".to_owned())
        .spawn(move || {
            #[cfg(feature = "test-hooks")]
            {
                let hold = bg.reaper_delay_ms.load(Ordering::SeqCst);
                if hold > 0 {
                    thread::sleep(Duration::from_millis(hold));
                }
            }
            if pipeline.set_state(gst::State::Null).is_err() {
                bg.log(&format!("gen={gen}: teardown to NULL failed"));
            }
            drop(pipeline);
            bg.reapers.fetch_sub(1, Ordering::SeqCst);
        });
    if spawned.is_err() {
        // No thread: the pipeline is left as it is rather than torn down on the caller's thread,
        // which must never block (F-001).
        shared.reapers.fetch_sub(1, Ordering::SeqCst);
        shared.log(&format!(
            "gen={gen}: no reaper thread; the pipeline is left as it is"
        ));
    }
}

fn make(factory: &str, name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .name(name)
        .build()
        .map_err(|e| format!("input: no {factory} element: {e}"))
}

fn capsfilter(name: &str, caps: gst::Caps) -> Result<gst::Element, String> {
    let f = make("capsfilter", name)?;
    f.set_property("caps", caps);
    Ok(f)
}

/// A queue that holds at most `max_ns` and then drops its OLDEST buffer (`leaky=downstream`). Its
/// buffer and byte caps stay at their defaults, a bound for buffers that carry no timestamp.
fn leaky_queue(name: &str, max_ns: u64) -> Result<gst::Element, String> {
    let q = make("queue", name)?;
    q.set_property_from_str("leaky", "downstream");
    q.set_property("max-size-time", max_ns);
    Ok(q)
}

/// Two samples at most, the oldest dropped, never synced to the clock, and never waiting to preroll:
/// a stream with no audio leaves the audio branch unlinked (spike trap: a sink that cannot preroll
/// holds the pipeline in PAUSED).
fn appsink(name: &str) -> gst_app::AppSink {
    let sink = gst_app::AppSink::builder()
        .name(name)
        .max_buffers(APPSINK_MAX_BUFFERS)
        .drop(true)
        .sync(false)
        .build();
    sink.set_property("async", false);
    sink
}

#[cfg(test)]
mod tests {
    use super::check_url;

    #[test]
    fn only_a_bare_srt_host_and_port_is_taken() {
        let ok = [
            "srt://127.0.0.1:7001",
            "srt://live.cloudflare.com:778",
            "SRT://h:1",
            "srt://[::1]:9000",
        ];
        let refused = [
            "srt://h",
            "srt://h:",
            "srt://h:77a",
            "srt://:778",
            "srt://h:778/",
            "srt://h:778?streamid=x",
            "srt://user:pass@h:778",
            "rtmp://h:778",
            "h:778",
            "",
        ];
        for u in ok {
            assert!(check_url(u).is_ok(), "{u}");
        }
        for u in refused {
            let e = check_url(u).expect_err(u);
            assert!(!e.contains("streamid") && !e.contains("pass"), "{e}");
        }
        assert!(ok.len() + refused.len() > 0);
    }
}
