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
//! **Error or EOS is decided when the error is posted** (PR #30 review I1). On a peer close `srtsrc`
//! posts an error and `basesrc` then pushes an EOS, which reaches the video appsink 1–7 ms later. The
//! bus's sync handler, on the posting thread and before that EOS exists, closes the gate for the
//! build, and that close is the decision: the EOS that follows finds the gate shut. The build's own
//! thread logs the error and sends the event, so a slow log sink delays the event, never changes it.
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
//! **The domain's own losses close it too** (G1, the coordinator's ruling on PR #30). The domain gives
//! a generation up by its stall rule or its connect timeout and tells the adapter nothing until it
//! rebuilds, a back-off later; a connection that starts delivering again in between would put the
//! camera back on air for the back-off. So each sample is held against the same two rules, read from
//! the same `Thresholds` and `DOMAIN_LAG_ALLOWANCE_MS` sooner: the first sample past the connect
//! timeout (before `InputConnected` was delivered), or past the stall rule since the video frame
//! before, closes the gate for good, for both branches: an audio sample is held against the same
//! clock, so a camera whose video died while its audio carried on is off air too (the confirm
//! pass's m2). No event is sent: the domain has given the generation up, or will within the
//! allowance while its loop lags by 250 ms or less (`_INDEX.md` Owed 50 has the residuals, and the
//! port call that replaces this copy of the rules).
//!
//! **Never blocks the caller.** Each build has one thread of its own, which starts it, reads its bus
//! and then tears it down: `srtsrc` resolves its host inside `set_state(PAUSED)`, and a hanging
//! resolver held that for 10 s (`_FINDINGS.md` F-012); a teardown can hang as long (F-001). With the
//! start and the teardown on one thread, a build retired mid-start never goes PLAYING after its
//! teardown (review M1). `rebuild`, `stop` and the drop only close the gate and set the build's stop
//! flag; nothing here joins a thread. The plugins are loaded once, when the adapter is built, so no
//! `rebuild` loads one on the driver's thread (review M4).
//!
//! **Secrets.** The URI carries the SRT stream id and passphrase, so it is never logged: each build logs
//! the URL's scheme and authority alone, and every GStreamer text this adapter forwards passes the
//! scrubber, which knows both values by value.
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "test-hooks")]
use std::sync::Condvar;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use app::ports::{redacted_origin, Inbound, InputSource, InputSpec};
use crossbeam_channel::Sender;
use domain::config::Thresholds;
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
/// How much sooner than the domain's stall rule and connect timeout a sample gives its generation up
/// (G1). The domain reads each event on its loop's clock, after the adapter saw the sample, so a
/// sample just inside the rule here could land just past it there; 250 ms (about seven ticks at
/// 30 fps) covers that lag, so whatever the adapter lets through, the domain still counts. A loop
/// that lags by more lets a given-up generation back on air for the back-off; `_INDEX.md` Owed 50
/// (the domain's own `abandon(gen)`) removes this constant and that window together.
pub const DOMAIN_LAG_ALLOWANCE_MS: u64 = 250;
/// Every element factory the input pipeline makes, loaded when the adapter is built (review M4).
pub const INPUT_FACTORIES: [&str; 12] = [
    "srtsrc",
    "tsdemux",
    "queue",
    "h264parse",
    "avdec_h264",
    "videoconvertscale",
    "capsfilter",
    "appsink",
    "aacparse",
    "avdec_aac",
    "audioconvert",
    "audioresample",
];
/// How long a build's thread waits for one bus message before looking at its stop flag again.
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

/// A domain rule (`Thresholds` ms) as the adapter holds a sample against it: `DOMAIN_LAG_ALLOWANCE_MS`
/// sooner, unless the rule is too short to give that much (then the rule itself), so a rule never
/// shrinks to nothing and gives up every sample.
fn ruled(rule_ms: u64) -> Duration {
    if rule_ms > 2 * DOMAIN_LAG_ALLOWANCE_MS {
        Duration::from_millis(rule_ms - DOMAIN_LAG_ALLOWANCE_MS)
    } else {
        Duration::from_millis(rule_ms)
    }
}

/// What the pipeline threads share with the adapter.
struct Shared {
    /// The build whose samples and events may pass; `None` while no generation is live.
    gate: Mutex<Option<u64>>,
    tx: Sender<Inbound>,
    frames: FrameStore,
    log: LogFn,
    scrubber: Arc<Mutex<Scrubber>>,
    /// Retired builds whose teardown (`set_state(NULL)` on the build's own thread) has not returned.
    reapers: Arc<AtomicUsize>,
    /// `InputFrame` events not delivered: the inbound channel was full.
    frames_dropped: AtomicU64,
    /// The inbound channel was full at the last frame (logged once a run).
    dropping: AtomicBool,
    /// Samples that arrived from a closed build and were dropped.
    gate_dropped: AtomicU64,
    /// The generation of the last frame that reached the camera slot (0: none yet).
    last_frame_gen: AtomicU64,
    /// Generations given up at a sample that came past the domain's stall rule or connect timeout.
    abandoned: AtomicU64,
    /// Test hook: each teardown waits this many ms before its `set_state(NULL)` (0: none).
    #[cfg(feature = "test-hooks")]
    reaper_delay_ms: AtomicU64,
    /// Test hook: while set, every start blocks inside its state change (`hold_starts`).
    #[cfg(feature = "test-hooks")]
    start_hold: Mutex<Option<Arc<HoldInner>>>,
}

impl Shared {
    fn log(&self, line: &str) {
        let clean = locked(&self.scrubber).scrub(line);
        (self.log)(&clean);
    }

    /// Closes the gate if `build` is the open one. True when this call closed it: the generation's
    /// loss is decided here, once, and nothing of that build reaches the slot or the loop afterwards.
    fn close(&self, build: u64) -> bool {
        let mut open = locked(&self.gate);
        if *open != Some(build) {
            return false;
        }
        *open = None;
        true
    }

    /// `InputLost { gen, reason }` for a loss already decided. It waits for room: this runs on the
    /// pipeline's own threads, never the driver's.
    fn report(&self, gen: u64, reason: LossReason) {
        self.log(&format!("gen={gen}: lost ({reason:?})"));
        let _ = self.tx.send(Inbound::Event(Event::InputLost { gen, reason }));
    }

    /// The domain's rules have given `gen` up (G1): the gate, held open for this build by the caller,
    /// closes for good, for video and audio alike. Counted and logged; no event is sent.
    fn give_up(&self, mut open: MutexGuard<'_, Option<u64>>, gen: u64, at: &str, why: &str) {
        *open = None;
        drop(open);
        self.gate_dropped.fetch_add(1, Ordering::Relaxed);
        self.abandoned.fetch_add(1, Ordering::SeqCst);
        self.log(&format!(
            "gen={gen}: given up at {at} sample: {why}; nothing more of it reaches the programme"
        ));
    }

    /// Decides `gen` lost and reports it, once: only while `build` is the open one.
    fn lose(&self, build: u64, gen: u64, reason: LossReason) {
        if self.close(build) {
            self.report(gen, reason);
        }
    }
}

/// The live `InputSource` (C10).
pub struct GstInput {
    shared: Arc<Shared>,
    latency_ms: u64,
    /// The domain's stall rule, as a sample is held against it (`ruled`).
    stall: Duration,
    /// The domain's connect timeout, likewise.
    connect: Duration,
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
    /// The gate's key for this build (`Shared::gate`).
    build: u64,
    pipeline: gst::Pipeline,
    /// Ends the build's thread: it then tears the pipeline down.
    stop: Arc<AtomicBool>,
    /// Set by the bus's sync handler when an error decided this build's loss; the build's thread
    /// sends the event.
    errored: Arc<AtomicBool>,
    /// The build's thread is running (false: it could not be spawned, and the pipeline never started).
    running: bool,
}

impl GstInput {
    /// `t` gives the SRT latency (`input_srt_latency_ms`) and the domain's two rules this adapter
    /// holds each sample against (`input_stall_after_ms`, `input_connect_timeout_ms`; G1);
    /// `out_w × out_h` is the programme's size (`OutputProfile`), which every camera frame is scaled
    /// into. It loads the pipeline's plugins (`INPUT_FACTORIES`), so build it before the loop starts.
    pub fn new(frames: FrameStore, tx: Sender<Inbound>, t: &Thresholds, out_w: u32, out_h: u32) -> Self {
        let input = Self {
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
                abandoned: AtomicU64::new(0),
                #[cfg(feature = "test-hooks")]
                reaper_delay_ms: AtomicU64::new(0),
                #[cfg(feature = "test-hooks")]
                start_hold: Mutex::new(None),
            }),
            latency_ms: t.input_srt_latency_ms,
            stall: ruled(t.input_stall_after_ms),
            connect: ruled(t.input_connect_timeout_ms),
            out_w,
            out_h,
            current: None,
            builds: 0,
            stopped: false,
            refused_rebuilds: 0,
            events_dropped: 0,
        };
        input.preload();
        input
    }

    /// Loads every factory's plugin now: a plugin's first use otherwise loads it inside `make`, on
    /// the driver's thread (the review measured 48–77 ms for the first `rebuild`). A factory that
    /// does not load is logged; the build that needs it then fails as a loss.
    fn preload(&self) {
        if let Err(e) = gst::init() {
            self.shared.log(&format!("gstreamer did not initialise: {e}"));
            return;
        }
        for name in INPUT_FACTORIES {
            let loaded = gst::ElementFactory::find(name).is_some_and(|f| f.load().is_ok());
            if !loaded {
                self.shared
                    .log(&format!("input: the {name} element did not load"));
            }
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

    /// Generations given up because a sample came after the domain's stall rule or connect timeout
    /// had already given them up.
    pub fn abandoned(&self) -> u64 {
        self.shared.abandoned.load(Ordering::SeqCst)
    }

    /// `rebuild` calls refused (after `stop`).
    pub fn refused_rebuilds(&self) -> u64 {
        self.refused_rebuilds
    }

    /// Events the driver's own calls could not deliver: the inbound channel was full.
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }

    /// Retired builds whose teardown has not returned yet.
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

    /// Test hook: every later start blocks until the returned hold is released (or dropped), inside
    /// its state change: the SRT source's READY→PAUSED state message is held in the bus's sync
    /// handler, on the starting thread, with the pipeline's state lock held. That is where a hanging
    /// resolver holds a real start (F-012: 10 s inside `srtsrc`'s start, under `set_state(PAUSED)`),
    /// so a teardown orders against it as it would against the real hang.
    #[cfg(feature = "test-hooks")]
    pub fn hold_starts(&self) -> StartHold {
        let inner = Arc::new(HoldInner::default());
        *locked(&self.shared.start_hold) = Some(Arc::clone(&inner));
        StartHold(inner)
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

    /// Closes the gate and tells the current build's thread to tear its pipeline down. Returns at once.
    fn retire_current(&mut self) {
        *locked(&self.shared.gate) = None;
        if let Some(live) = self.current.take() {
            if live.running {
                // Counted before the flag is set: the build's thread counts down once it sees it.
                self.shared.reapers.fetch_add(1, Ordering::SeqCst);
                live.stop.store(true, Ordering::SeqCst);
            }
            // A build with no thread never started: dropping its pipeline here changes no state.
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
        // The owner's ruling, 2026-10-19 (Owed 48): a frame with damaged macroblocks is dropped, so
        // after a loss the slot holds the last good picture for a moment instead of airing smears.
        vdec.set_property("output-corrupt", false);
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
        let vsink = appsink("vsink")?;

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
        let asink = appsink("asink")?;

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
        // One clock for both branches: the domain's rules are about video frames, and a generation
        // they gave up is off air for its audio too (G1; the confirm pass's m2).
        let clock = Arc::new(Mutex::new(SampleClock::new(
            Instant::now(),
            self.stall,
            self.connect,
        )));
        self.wire_video(&vsink, build, gen, Arc::clone(&clock));
        self.wire_audio(&asink, build, gen, clock);
        let errored = Arc::new(AtomicBool::new(false));
        self.sync_handler(&pipeline, build, &errored)?;
        Ok(Live {
            gen,
            build,
            pipeline,
            stop: Arc::new(AtomicBool::new(false)),
            errored,
            running: false,
        })
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

    fn wire_video(&self, sink: &gst_app::AppSink, build: u64, gen: u64, clock: Arc<Mutex<SampleClock>>) {
        let shared = Arc::clone(&self.shared);
        let eos = Arc::clone(&self.shared);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let now = Instant::now();
                    let open = locked(&shared.gate);
                    if *open != Some(build) {
                        shared.gate_dropped.fetch_add(1, Ordering::Relaxed);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    let mut c = locked(&clock);
                    if let Some(why) = c.given_up(now) {
                        drop(c);
                        shared.give_up(open, gen, "a video", why);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    c.last = Some(now);
                    shared.frames.set_camera(sample);
                    shared.last_frame_gen.store(gen, Ordering::SeqCst);
                    if !c.connected
                        && shared
                            .tx
                            .try_send(Inbound::Event(Event::InputConnected { gen }))
                            .is_ok()
                    {
                        c.connected = true;
                    }
                    let delivered = shared.tx.try_send(Inbound::Event(Event::InputFrame)).is_ok();
                    drop(c);
                    drop(open);
                    note_frame(&shared, delivered);
                    Ok(gst::FlowSuccess::Ok)
                })
                .eos(move |_| eos.lose(build, gen, LossReason::Eos))
                .build(),
        );
    }

    /// Audio passes the same gate and the same clock as the video: an audio sample that comes after
    /// the domain's rules gave the generation up (no video frame within the stall rule, or none yet
    /// at the connect timeout) closes the gate for both branches.
    fn wire_audio(&self, sink: &gst_app::AppSink, build: u64, gen: u64, clock: Arc<Mutex<SampleClock>>) {
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
                    let now = Instant::now();
                    let open = locked(&shared.gate);
                    if *open != Some(build) {
                        shared.gate_dropped.fetch_add(1, Ordering::Relaxed);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    let given_up = locked(&clock).given_up(now);
                    if let Some(why) = given_up {
                        shared.give_up(open, gen, "an audio", why);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    shared.frames.push_audio(map.as_slice());
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    /// Runs on the thread that posts each message, before it is queued: an error decides its build's
    /// loss there and then (I1), ahead of the EOS `basesrc` pushes after it. Nothing slow runs here.
    fn sync_handler(
        &self,
        pipeline: &gst::Pipeline,
        build: u64,
        errored: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        let bus = pipeline.bus().ok_or("input pipeline has no bus")?;
        let shared = Arc::clone(&self.shared);
        let errored = Arc::clone(errored);
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(_) => {
                    if shared.close(build) {
                        errored.store(true, Ordering::SeqCst);
                    }
                }
                #[cfg(feature = "test-hooks")]
                gst::MessageView::StateChanged(s) => hold_at_the_start(&shared, msg, s),
                _ => {}
            }
            gst::BusSyncReply::Pass
        });
        Ok(())
    }

    /// Hands the build to a thread of its own (`run_build`). If no thread can be had, the generation
    /// is lost at once and its pipeline, never started, is let go.
    fn start(&mut self, live: &mut Live) {
        let gen = live.gen;
        let Some(bus) = live.pipeline.bus() else {
            return self.lost_unstarted(gen, "no bus");
        };
        let shared = Arc::clone(&self.shared);
        let pipeline = live.pipeline.clone();
        let stop = Arc::clone(&live.stop);
        let errored = Arc::clone(&live.errored);
        let spawned = thread::Builder::new()
            .name("input-build".to_owned())
            .spawn(move || run_build(&shared, &pipeline, &bus, &stop, &errored, gen));
        match spawned {
            Ok(_) => live.running = true,
            Err(_) => self.lost_unstarted(gen, "no thread"),
        }
    }

    fn lost_unstarted(&mut self, gen: u64, why: &str) {
        *locked(&self.shared.gate) = None;
        self.shared
            .log(&format!("gen={gen}: {why} to start it; the generation is lost"));
        self.send_now(Event::InputLost {
            gen,
            reason: LossReason::Error,
        });
    }
}

/// One build's thread, from its start to its teardown, so the two can never cross (review M1).
///
/// 1. The start. READY→PAUSED is where `srtsrc` resolves its host and opens its socket (F-012: a
///    hanging resolver held it 10 s); its connect waits for PLAYING. A build retired by then never
///    goes PLAYING, so a stopped build never reaches the camera. A refused start is lost through
///    the error its element posts on the bus (measured: "Could not open resource" 20 ms in), the one
///    road to `InputLost`, with the domain's connect timeout behind it.
/// 2. The bus, until the build is retired: every error and warning text is logged after the
///    scrubber, and the error that decided the build's loss (the sync handler) is reported.
/// 3. The teardown, `set_state(NULL)`, counted in `teardowns_in_flight` until it returns.
fn run_build(
    shared: &Arc<Shared>,
    pipeline: &gst::Pipeline,
    bus: &gst::Bus,
    stop: &AtomicBool,
    errored: &AtomicBool,
    gen: u64,
) {
    match pipeline.set_state(gst::State::Paused) {
        Err(e) => shared.log(&format!("gen={gen}: did not start: {e}")),
        Ok(_) if stop.load(Ordering::SeqCst) => {}
        Ok(_) => {
            if let Err(e) = pipeline.set_state(gst::State::Playing) {
                shared.log(&format!("gen={gen}: did not start: {e}"));
            }
        }
    }
    while !stop.load(Ordering::SeqCst) {
        let Some(msg) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(BUS_WAIT_MS),
            &[gst::MessageType::Error, gst::MessageType::Warning],
        ) else {
            continue;
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match msg.view() {
            gst::MessageView::Error(e) => {
                shared.log(&format!(
                    "gen={gen}: error: {} ({})",
                    e.error(),
                    e.debug().as_deref().unwrap_or("")
                ));
                if errored.swap(false, Ordering::SeqCst) {
                    shared.report(gen, LossReason::Error);
                }
            }
            gst::MessageView::Warning(w) => shared.log(&format!(
                "gen={gen}: warning: {} ({})",
                w.error(),
                w.debug().as_deref().unwrap_or("")
            )),
            _ => {}
        }
    }
    #[cfg(feature = "test-hooks")]
    {
        let hold = shared.reaper_delay_ms.load(Ordering::SeqCst);
        if hold > 0 {
            thread::sleep(Duration::from_millis(hold));
        }
    }
    if pipeline.set_state(gst::State::Null).is_err() {
        shared.log(&format!("gen={gen}: teardown to NULL failed"));
    }
    shared.reapers.fetch_sub(1, Ordering::SeqCst);
}

/// When a generation's video frames came, held against the domain's two rules (G1). Both branches
/// read it; only a video sample that passes moves it.
struct SampleClock {
    built: Instant,
    /// The generation's `InputConnected` was delivered.
    connected: bool,
    /// The last sample that passed.
    last: Option<Instant>,
    stall: Duration,
    connect: Duration,
}

impl SampleClock {
    fn new(built: Instant, stall: Duration, connect: Duration) -> Self {
        Self {
            built,
            connected: false,
            last: None,
            stall,
            connect,
        }
    }

    /// Why a sample (video or audio) at `now` comes after the domain has given the generation up, if
    /// it does: before `InputConnected` was delivered, at or past the connect timeout from the build;
    /// after it, at or past the stall rule from the last video frame that passed.
    fn given_up(&self, now: Instant) -> Option<&'static str> {
        if !self.connected {
            (now.saturating_duration_since(self.built) >= self.connect)
                .then_some("no video frame came within the connect timeout")
        } else {
            self.last
                .filter(|l| now.saturating_duration_since(*l) >= self.stall)
                .map(|_| "no video frame came within the stall rule")
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
            Ok(mut live) => {
                self.builds = build;
                self.shared.log(&format!(
                    "gen={gen}: {} latency={}",
                    redacted_origin(&spec.url),
                    self.latency_ms
                ));
                *locked(&self.shared.gate) = Some(build);
                self.start(&mut live);
                if live.running {
                    self.current = Some(live);
                }
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

    /// The domain has given `gen` up (C10b): the gate closes for its build, video and audio alike,
    /// before this returns, and no event is sent (the domain decided it). The pipeline stays up,
    /// gated, until the next `rebuild` tears it down. A `gen` that is not the current build's, a
    /// build this adapter already reported lost, and a second call change nothing.
    fn abandon(&mut self, gen: u64) {
        let Some(build) = self.current.as_ref().filter(|l| l.gen == gen).map(|l| l.build) else {
            self.shared
                .log(&format!("gen={gen}: abandon ignored: not the current build"));
            return;
        };
        if self.shared.close(build) {
            self.shared.abandoned.fetch_add(1, Ordering::SeqCst);
            self.shared.log(&format!(
                "gen={gen}: abandoned by the domain; nothing more of it reaches the programme"
            ));
        } else {
            self.shared
                .log(&format!("gen={gen}: abandon: its gate was already shut"));
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
    /// Returns at once (C08 amendment 2026-10-19: within 500 ms): the gate closes, the build's thread
    /// is told to tear its pipeline down, and no thread is joined.
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
            .field("stall", &self.stall)
            .field("connect", &self.connect)
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

/// Test hook (`hold_starts`): a start is held at its SRT source's READY→PAUSED state message.
#[cfg(feature = "test-hooks")]
fn hold_at_the_start(shared: &Shared, msg: &gst::MessageRef, s: &gst::message::StateChanged) {
    let from_src = msg.src().is_some_and(|e| e.name() == "src");
    if from_src && s.old() == gst::State::Ready && s.current() == gst::State::Paused {
        let hold = locked(&shared.start_hold).clone();
        if let Some(h) = hold {
            h.wait();
        }
    }
}

/// Test hook: a start hold (`GstInput::hold_starts`). Released by `release` or on drop.
#[cfg(feature = "test-hooks")]
#[derive(Debug)]
pub struct StartHold(Arc<HoldInner>);

#[cfg(feature = "test-hooks")]
#[derive(Debug, Default)]
struct HoldInner {
    released: Mutex<bool>,
    cv: Condvar,
    waiting: AtomicUsize,
}

#[cfg(feature = "test-hooks")]
impl HoldInner {
    fn wait(&self) {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let mut released = locked(&self.released);
        while !*released {
            released = self.cv.wait(released).unwrap_or_else(PoisonError::into_inner);
        }
        drop(released);
        self.waiting.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(feature = "test-hooks")]
impl StartHold {
    /// Starts blocked in the hold now.
    pub fn waiting(&self) -> usize {
        self.0.waiting.load(Ordering::SeqCst)
    }

    /// Lets every held start, and every later one, go ahead.
    pub fn release(&self) {
        *locked(&self.0.released) = true;
        self.0.cv.notify_all();
    }
}

#[cfg(feature = "test-hooks")]
impl Drop for StartHold {
    fn drop(&mut self) {
        self.release();
    }
}

/// Every factory used here belongs in `INPUT_FACTORIES`, or its plugin loads here on the first build,
/// on the driver's thread (`tests/input_preload.rs` compares the two).
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
fn appsink(name: &str) -> Result<gst_app::AppSink, String> {
    let sink = make("appsink", name)?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| format!("input: {name} is not an appsink"))?;
    sink.set_max_buffers(APPSINK_MAX_BUFFERS);
    sink.set_drop(true);
    sink.set_sync(false);
    sink.set_property("async", false);
    Ok(sink)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{check_url, ruled, SampleClock, DOMAIN_LAG_ALLOWANCE_MS};
    use domain::config::Thresholds;

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

    /// G1's two rules at their edges, from the domain's own defaults: a sample 1 ms inside the rule
    /// passes, one at it is given up; before the connect, from the build; after it, from the last
    /// sample. A rule too short to give the allowance is held as it is, never as zero.
    #[test]
    fn a_sample_is_held_against_the_domains_rules() {
        let t = Thresholds::default();
        let (stall, connect) = (ruled(t.input_stall_after_ms), ruled(t.input_connect_timeout_ms));
        assert_eq!(
            stall,
            Duration::from_millis(t.input_stall_after_ms - DOMAIN_LAG_ALLOWANCE_MS)
        );
        assert_eq!(
            connect,
            Duration::from_millis(t.input_connect_timeout_ms - DOMAIN_LAG_ALLOWANCE_MS)
        );
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut c = SampleClock::new(t0, stall, connect);
        let mut checked = 0;
        assert_eq!(c.given_up(t0 + connect - ms(1)), None);
        assert!(c.given_up(t0 + connect).is_some_and(|w| w.contains("connect")));
        checked += 2;
        c.connected = true;
        c.last = Some(t0);
        assert_eq!(c.given_up(t0 + stall - ms(1)), None);
        assert!(c.given_up(t0 + stall).is_some_and(|w| w.contains("stall")));
        // Connected, the build's age no longer counts: only the gap since the last sample.
        c.last = Some(t0 + ms(60_000));
        assert_eq!(c.given_up(t0 + ms(60_001)), None);
        checked += 3;
        assert_eq!(
            ruled(2 * DOMAIN_LAG_ALLOWANCE_MS),
            ms(2 * DOMAIN_LAG_ALLOWANCE_MS)
        );
        assert_eq!(ruled(0), ms(0));
        assert_eq!(
            ruled(2 * DOMAIN_LAG_ALLOWANCE_MS + 1),
            ms(DOMAIN_LAG_ALLOWANCE_MS + 1)
        );
        checked += 3;
        assert_eq!(checked, 8);
    }

    /// m1 (PR #30 confirm pass): the allowance is 250 ms (the as-built row's (16)), read here as a
    /// number of its own, never from the constant: a sample 250 ms inside either rule is given up,
    /// one 251 ms inside passes, for the domain's defaults and the integration tests' short rules.
    #[test]
    fn the_allowance_gives_up_250_ms_inside_each_rule() {
        let ms = Duration::from_millis;
        let t = Thresholds::default();
        let mut checked = 0;
        for rule in [t.input_stall_after_ms, t.input_connect_timeout_ms, 2_000, 3_000] {
            assert!(rule > 500, "a rule this short is held whole: {rule}");
            let t0 = Instant::now();
            let mut c = SampleClock::new(t0, ruled(rule), ruled(rule));
            assert_eq!(c.given_up(t0 + ms(rule - 251)), None, "connect {rule}");
            assert!(c.given_up(t0 + ms(rule - 250)).is_some(), "connect {rule}");
            c.connected = true;
            c.last = Some(t0);
            assert_eq!(c.given_up(t0 + ms(rule - 251)), None, "stall {rule}");
            assert!(c.given_up(t0 + ms(rule - 250)).is_some(), "stall {rule}");
            checked += 4;
        }
        assert_eq!(checked, 16);
    }
}
