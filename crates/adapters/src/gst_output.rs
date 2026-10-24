//! The output pipeline (C09; spec §3.3's refinement): `appsrc(h264) + appsrc(aac) → flvmux
//! streamable=true → rtmp2sink`, or `filesink` for a `file://` target. Rebuilt on every
//! `swap_sink(gen, target)`; the programme pipeline that feeds it is never rebuilt.
//!
//! The two pipelines are joined only by the **encoded ring**: the programme's encoder appsinks push
//! every encoded sample into it (2 s by PTS, dropping the oldest; plan, Global Constraints), and one
//! feeder thread per output generation takes from it. A new output starts at a video keyframe, and
//! its PTS starts at 0.
//!
//! **A sink sends nothing that was queued before it connected** (Owed 84; owner, 2026-10-24,
//! option A: when the destination recovers after an outage, the queued stale frames are dropped and
//! the stream restarts at the next keyframe, so viewers never get a burst of old frames; F-023).
//! The feeder takes nothing from the ring until the sink can send (`Gate`): a file at once, and
//! rtmp2sink once its publish is accepted. Until then the ring stays the only queue, and it drops
//! its oldest. At that moment (the reconnect point) the feeder asks the encoder for a keyframe, and
//! no video frame the ring holds then may start the stream (`Picker`): the first frame sent is a
//! keyframe encoded after the reconnect point, and no audio older than it is sent. A request the
//! encoder does not take is asked again at each entry. A drain that reaches an output that has sent
//! nothing, at the gate or after it, ends with the encoders (`release_unsent`).
//!
//! **Progress** (Owed 13). A poll every `OUTPUT_PROGRESS_POLL_MS` reads the sink's bytes-SENT total,
//! rtmp2sink `stats` field `out-bytes-total` (F-010), or the file's size for `file://`, and sends
//! `Event::OutputProgress { gen, total_bytes }`. Never acknowledged bytes: CF Input B sends none
//! (F-005). A bus error sends `Event::OutputError { gen }`, and its text goes to the log only after
//! the scrubber. The sink's `Eos` after `drain()` takes one final total into `Stats` and sends
//! `Event::OutputDrained`.
//!
//! **A destination stall is the sink's own error** (Owed 87; the owner, 2026-10-24, "A"). Every
//! rtmp2sink gets `timeout` = `OUTPUT_SINK_TIMEOUT_MS`, in its unit, whole seconds: a socket wait
//! that long fails the sink, and that bus error is the `OutputError` the domain swaps on. Set, not
//! inherited: it equals GStreamer 1.24.2's default today, and the domain's 15 s budget reads it.
//!
//! **Teardown never blocks the caller.** A retired output is set to NULL on a reaper thread of its
//! own (F-001: `set_state(NULL)` did not return for 10 s on a hung renderer).
use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use app::ports::{Inbound, TargetSpec};
use crossbeam_channel::Sender;
use domain::config::OUTPUT_SINK_TIMEOUT_MS;
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::redact::Scrubber;
use crate::stats::Stats;

/// How often the output's bytes-sent total is read (Owed 15, the choice recorded in `_FINDINGS.md`
/// F-011): 250 ms. The totals prove a new sink Connected (`output_connected_after_ms`, Owed 38).
/// Since Owed 87 no rule times their silence, so the poll no longer bounds a stall's detection:
/// rtmp2sink's `timeout` does (`OUTPUT_SINK_TIMEOUT_S`).
pub const OUTPUT_PROGRESS_POLL_MS: u64 = 250;

/// rtmp2sink's `timeout` (a uint, "RTMP timeout in seconds") on every RTMP sink: the domain's
/// `OUTPUT_SINK_TIMEOUT_MS` (Owed 87) in the property's unit.
pub const OUTPUT_SINK_TIMEOUT_S: u32 = (OUTPUT_SINK_TIMEOUT_MS / 1_000) as u32;
// The property counts whole seconds and 0 means no timeout: a constant it cannot carry exactly is a
// compile error, never a rounded or disabled detector.
const _: () = assert!(
    OUTPUT_SINK_TIMEOUT_MS.is_multiple_of(1_000)
        && OUTPUT_SINK_TIMEOUT_MS >= 1_000
        && OUTPUT_SINK_TIMEOUT_MS / 1_000 <= u32::MAX as u64
);

/// The encoded ring's span (plan, Global Constraints: "Encoded ring buffer 2 s, dropping the oldest").
pub const ENCODED_RING_SPAN: Duration = Duration::from_secs(2);
/// A hard cap on the ring's entries beside its span, so that samples with no PTS cannot grow it:
/// 2 s holds 60 video frames at 30 fps and about 94 AAC frames at 48 kHz (1 024 samples each), 154 in
/// all; twice that is the cap.
pub const ENCODED_RING_MAX_ENTRIES: usize = 308;
/// The feeder stops taking from the ring when the output's two appsrcs both hold data and either holds
/// at least this much: the sink is not consuming, so the backlog stays in the (bounded) ring rather
/// than in the output. 512 KiB is about 1 s at 4 Mbps.
pub const OUTPUT_QUEUE_BYTES: u64 = 512 * 1024;
/// How long the feeder and the bus thread wait before looking again (a retire is seen within it).
const WAKE: Duration = Duration::from_millis(20);
/// The bus thread's longest wait for one message.
const BUS_WAIT: Duration = Duration::from_millis(50);

pub(crate) type LogFn = Arc<dyn Fn(&str) + Send + Sync>;
/// Asks the programme's encoder for a keyframe (a force-key-unit event); false if it was not taken.
pub(crate) type KeyRequest = Arc<dyn Fn() -> bool + Send + Sync>;

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One encoded sample, its PTS already in running time (x264enc's output segment starts at 1 000 h,
/// so a raw PTS would put the video 1 000 h ahead of the audio: spike trap, F-001's chain). `S` is
/// the sample, a `gst::Sample` everywhere but in the `Picker`'s own tests.
#[derive(Clone)]
pub(crate) struct Entry<S = gst::Sample> {
    seq: u64,
    pub(crate) sample: S,
    pub(crate) video: bool,
    pub(crate) key: bool,
    pub(crate) pts: gst::ClockTime,
}

impl<S> Entry<S> {
    pub(crate) fn new(sample: S, video: bool, key: bool, pts: gst::ClockTime) -> Self {
        Self {
            seq: 0,
            sample,
            video,
            key,
            pts,
        }
    }
}

#[derive(Default)]
struct RingState {
    entries: VecDeque<Entry>,
    next_seq: u64,
    video_eos: bool,
    audio_eos: bool,
    /// The output generation allowed to take entries; 0 = no output.
    owner: u64,
    /// `OutputDrained` was sent (it carries no generation, so it is sent once in all).
    drained_sent: bool,
}

/// The programme → output hand-off: bounded by span and by count, dropping the oldest.
#[derive(Default)]
pub(crate) struct EncodedRing {
    state: Mutex<RingState>,
    wake: Condvar,
}

impl EncodedRing {
    pub(crate) fn push(&self, mut e: Entry) {
        let mut r = locked(&self.state);
        e.seq = r.next_seq;
        r.next_seq += 1;
        r.entries.push_back(e);
        loop {
            let over_count = r.entries.len() > ENCODED_RING_MAX_ENTRIES;
            let over_span = match (r.entries.front(), r.entries.back()) {
                (Some(a), Some(b)) => b.pts.saturating_sub(a.pts) > span(),
                _ => false,
            };
            if !(over_count || over_span) {
                break;
            }
            r.entries.pop_front();
        }
        drop(r);
        self.wake.notify_all();
    }

    /// The entries held now (the test hooks read it).
    #[cfg(feature = "test-hooks")]
    pub(crate) fn len(&self) -> usize {
        locked(&self.state).entries.len()
    }

    /// Drops the oldest entries as an overflowing ring does, up to the first video entry that is
    /// not a keyframe (at least one is dropped), so the next entry a feeder takes follows a gap and
    /// cannot start a picture (the test hooks plant it). Returns how many were dropped.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn drop_to_delta(&self) -> usize {
        let mut r = locked(&self.state);
        let mut dropped = 0;
        while let Some(front) = r.entries.front() {
            if dropped > 0 && front.video && !front.key {
                break;
            }
            r.entries.pop_front();
            dropped += 1;
        }
        dropped
    }

    pub(crate) fn end_of_stream(&self, video: bool) {
        let mut r = locked(&self.state);
        if video {
            r.video_eos = true;
        } else {
            r.audio_eos = true;
        }
        drop(r);
        self.wake.notify_all();
    }

    /// Both encoders have reached EOS and no output exists: there is nothing to drain (what the
    /// ring still holds has no sink to go to), and `OutputDrained` is due now (once).
    pub(crate) fn take_drained_without_output(&self) -> bool {
        let mut r = locked(&self.state);
        if r.video_eos && r.audio_eos && r.owner == 0 && !r.drained_sent {
            r.drained_sent = true;
            return true;
        }
        false
    }

    pub(crate) fn take_drained(&self) -> bool {
        let mut r = locked(&self.state);
        !std::mem::replace(&mut r.drained_sent, true)
    }

    /// An output reached EOS after the programme's drain reached the ring (both encoders at EOS):
    /// the EOS is that drain's, and `OutputDrained` is due now (once). An output's EOS before then
    /// is a failed sink's aftermath (each appsrc whose push fails sends one down; F-011), not a
    /// drain, and must not spend the once-only event.
    pub(crate) fn take_drained_at_eos(&self) -> bool {
        let mut r = locked(&self.state);
        if r.video_eos && r.audio_eos && !r.drained_sent {
            r.drained_sent = true;
            return true;
        }
        false
    }

    fn set_owner(&self, gen: u64) {
        locked(&self.state).owner = gen;
        self.wake.notify_all();
    }

    fn release(&self, gen: u64) {
        let mut r = locked(&self.state);
        if r.owner == gen {
            r.owner = 0;
        }
        drop(r);
        self.wake.notify_all();
    }
}

fn span() -> gst::ClockTime {
    gst::ClockTime::from_nseconds(ENCODED_RING_SPAN.as_nanos() as u64)
}

/// What the output threads share with the programme.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) ring: Arc<EncodedRing>,
    pub(crate) tx: Sender<Inbound>,
    pub(crate) log: LogFn,
    pub(crate) stats: Arc<Stats>,
    pub(crate) scrubber: Arc<Mutex<Scrubber>>,
    /// Teardowns (`set_state(NULL)` on a reaper thread) not yet returned.
    pub(crate) reapers: Arc<AtomicUsize>,
    /// Progress events not sent because the inbound channel was full (the next poll carries a newer
    /// total, so none is owed).
    pub(crate) progress_dropped: Arc<AtomicU64>,
    /// The `timeout` each new rtmp2sink gets: `OUTPUT_SINK_TIMEOUT_S`, which only a test hook
    /// changes (`GstProgramme::set_output_sink_timeout_s`), so a read-back can tell the set from the
    /// element's default (Owed 87).
    pub(crate) sink_timeout_s: u32,
}

impl Shared {
    pub(crate) fn log(&self, line: &str) {
        let clean = locked(&self.scrubber).scrub(line);
        (self.log)(&clean);
    }

    /// An event the domain must see (an error, the drain): this thread waits for room.
    fn send(&self, e: Event) {
        let _ = self.tx.send(Inbound::Event(e));
    }
}

/// Where an output goes.
#[derive(Clone, PartialEq, Eq)]
enum Destination {
    Rtmp(String),
    File(PathBuf),
}

impl Destination {
    /// `rtmp://` or `rtmps://` plus the stream key as the last path segment, or a `file://` path.
    /// Anything else, or an RTMP target with no key, is refused; the text never holds the key.
    fn parse(target: &TargetSpec) -> Result<Destination, String> {
        let url = target.url.trim();
        if let Some(path) = url.strip_prefix("file://") {
            if path.is_empty() {
                return Err("output target file:// has no path".to_owned());
            }
            return Ok(Destination::File(PathBuf::from(path)));
        }
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("rtmp://") || lower.starts_with("rtmps://") {
            if target.stream_key.trim().is_empty() {
                return Err("output target has no stream key".to_owned());
            }
            let base = url.trim_end_matches('/');
            return Ok(Destination::Rtmp(format!("{base}/{}", target.stream_key)));
        }
        Err(format!(
            "output target scheme is not rtmp, rtmps or file: {}",
            app::ports::redacted_origin(url)
        ))
    }
}

/// One output generation: its pipeline and the flag its threads stop on.
pub(crate) struct OutputGen {
    gen: u64,
    pipeline: gst::Pipeline,
    #[cfg(feature = "test-hooks")]
    sink: gst::Element,
    #[cfg(feature = "test-hooks")]
    srcs: (gst_app::AppSrc, gst_app::AppSrc),
    #[cfg(feature = "test-hooks")]
    first_pts: FirstPts,
    retired: Arc<AtomicBool>,
}

/// The first PTS each output appsrc sent downstream, (video, audio) (the test hooks read it).
#[cfg(feature = "test-hooks")]
type FirstPts = Arc<Mutex<(Option<gst::ClockTime>, Option<gst::ClockTime>)>>;

impl fmt::Debug for OutputGen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutputGen").field("gen", &self.gen).finish()
    }
}

impl OutputGen {
    pub(crate) fn gen(&self) -> u64 {
        self.gen
    }

    /// The sink element (the test hooks hold its writes).
    #[cfg(feature = "test-hooks")]
    pub(crate) fn sink(&self) -> &gst::Element {
        &self.sink
    }

    /// Posts an error on this output's bus, as a failing sink would (the test hooks plant it).
    #[cfg(feature = "test-hooks")]
    pub(crate) fn plant_error(&self) -> bool {
        let msg = gst::message::Error::builder(gst::ResourceError::Write, "planted output error")
            .src(&self.sink)
            .build();
        self.pipeline.post_message(msg).is_ok()
    }

    /// The first PTS each appsrc sent downstream (the test hooks read it).
    #[cfg(feature = "test-hooks")]
    pub(crate) fn first_pts(&self) -> (Option<gst::ClockTime>, Option<gst::ClockTime>) {
        *locked(&self.first_pts)
    }

    /// The bytes queued in the video and audio appsrcs (the test hooks read them).
    #[cfg(feature = "test-hooks")]
    pub(crate) fn queued(&self) -> (u64, u64) {
        (
            self.srcs.0.current_level_bytes(),
            self.srcs.1.current_level_bytes(),
        )
    }

    /// Stops this generation's threads and hands its pipeline to a reaper thread, which sets it to
    /// NULL. Returns at once.
    pub(crate) fn retire(self, shared: &Shared) {
        self.retired.store(true, Ordering::SeqCst);
        shared.ring.release(self.gen);
        reap(shared, self.pipeline.upcast(), format!("output gen={}", self.gen));
    }
}

/// Sets `element` to NULL on a thread of its own, counted in `shared.reapers` until it returns.
pub(crate) fn reap(shared: &Shared, element: gst::Element, what: String) {
    let reapers = Arc::clone(&shared.reapers);
    reapers.fetch_add(1, Ordering::SeqCst);
    let log = shared.clone();
    let what_bg = what.clone();
    let spawned = thread::Builder::new().name("gst-reaper".to_owned()).spawn({
        let reapers = Arc::clone(&reapers);
        move || {
            if element.set_state(gst::State::Null).is_err() {
                log.log(&format!("{what_bg}: teardown to NULL failed"));
            }
            drop(element);
            reapers.fetch_sub(1, Ordering::SeqCst);
        }
    });
    if spawned.is_err() {
        // No thread: the pipeline is leaked rather than torn down on the caller's thread, which
        // must never block (F-001).
        reapers.fetch_sub(1, Ordering::SeqCst);
        shared.log(&format!(
            "{what}: no reaper thread; the pipeline is left as it is"
        ));
    }
}

fn make(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|e| format!("output: no {name} element: {e}"))
}

/// Builds and starts generation `gen` for `target`, and starts its feeder and poll threads. The
/// feeder asks for the encoder's next keyframe through `request_key` when the sink can send (Owed 84).
pub(crate) fn build(
    shared: &Shared,
    gen: u64,
    target: &TargetSpec,
    request_key: KeyRequest,
) -> Result<OutputGen, String> {
    let dest = Destination::parse(target)?;
    let pipeline = gst::Pipeline::new();
    let vsrc = make("appsrc")?;
    let asrc = make("appsrc")?;
    let mux = make("flvmux")?;
    mux.set_property("streamable", true);
    let sink = match &dest {
        Destination::Rtmp(location) => {
            let s = make("rtmp2sink")?;
            s.set_property("location", location.as_str());
            s.set_property("sync", false);
            // The gate (Owed 84) waits for the publish before it feeds a byte, so the connect must
            // start without data: rtmp2sink's default, set here because the gate depends on it (with
            // `false` it connects on its first buffer, which the gate never sends).
            s.set_property("async-connect", true);
            // The only detector of a destination stall (Owed 87): see the module doc.
            s.set_property("timeout", shared.sink_timeout_s);
            s
        }
        Destination::File(path) => {
            let s = make("filesink")?;
            s.set_property("location", path.to_string_lossy().as_ref());
            s.set_property("sync", false);
            s
        }
    };
    pipeline
        .add_many([&vsrc, &asrc, &mux, &sink])
        .map_err(|e| format!("output: {e}"))?;
    mux.link(&sink).map_err(|e| format!("output: mux to sink: {e}"))?;
    link_to_mux(&vsrc, &mux, "video")?;
    link_to_mux(&asrc, &mux, "audio")?;
    let vsrc = into_appsrc(vsrc)?;
    let asrc = into_appsrc(asrc)?;
    for src in [&vsrc, &asrc] {
        src.set_format(gst::Format::Time);
        src.set_is_live(false);
        src.set_property("block", false);
        // The feeder bounds what it queues here (`OUTPUT_QUEUE_BYTES`); appsrc's own cap only
        // signals, so it is set out of the way.
        src.set_max_bytes(0);
    }
    // The operator's one line naming where this generation sends: through `Shared::log`, which
    // scrubs the key out of it.
    let shown = match &dest {
        Destination::Rtmp(location) => location.clone(),
        Destination::File(path) => format!("file://{}", path.display()),
    };
    shared.log(&format!("output gen={gen}: sink {shown}"));
    if let Err(e) = pipeline.set_state(gst::State::Playing) {
        // A sink that cannot open (a file in a missing directory) leaves elements past NULL.
        reap(shared, pipeline.upcast(), format!("output gen={gen}"));
        return Err(format!("output gen={gen}: did not start: {e}"));
    }

    let stats_sink = shared.stats.on_new_sink();
    shared.ring.set_owner(gen);
    let retired = Arc::new(AtomicBool::new(false));

    #[cfg(feature = "test-hooks")]
    let srcs = (vsrc.clone(), asrc.clone());
    #[cfg(feature = "test-hooks")]
    let first_pts = record_first_pts(&vsrc, &asrc);
    let feeder = Feeder {
        gen,
        vsrc,
        asrc,
        ring: Arc::clone(&shared.ring),
        retired: Arc::clone(&retired),
        gate: match &dest {
            Destination::Rtmp(_) => Gate::Publish(sink.clone()),
            Destination::File(_) => Gate::Open,
        },
        request_key,
        shared: shared.clone(),
    };
    thread::Builder::new()
        .name(format!("output-feed-{gen}"))
        .spawn(move || feeder.run())
        .map_err(|e| format!("output gen={gen}: no feeder thread: {e}"))?;

    let watch = Watch {
        gen,
        stats_sink,
        dropped_streak: 0,
        bus: pipeline.bus().ok_or("output: pipeline has no bus")?,
        sink: sink.clone(),
        file: match &dest {
            Destination::File(p) => Some(p.clone()),
            Destination::Rtmp(_) => None,
        },
        shared: shared.clone(),
        retired: Arc::clone(&retired),
    };
    thread::Builder::new()
        .name(format!("output-watch-{gen}"))
        .spawn(move || watch.run())
        .map_err(|e| format!("output gen={gen}: no watch thread: {e}"))?;

    Ok(OutputGen {
        gen,
        pipeline,
        #[cfg(feature = "test-hooks")]
        sink,
        #[cfg(feature = "test-hooks")]
        srcs,
        #[cfg(feature = "test-hooks")]
        first_pts,
        retired,
    })
}

/// A probe on each appsrc's src pad that records the first buffer's PTS, then removes itself.
#[cfg(feature = "test-hooks")]
fn record_first_pts(vsrc: &gst_app::AppSrc, asrc: &gst_app::AppSrc) -> FirstPts {
    let first: FirstPts = Arc::default();
    for (video, src) in [(true, vsrc), (false, asrc)] {
        let Some(pad) = src.static_pad("src") else {
            continue;
        };
        let first = Arc::clone(&first);
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let pts = info.buffer().and_then(|b| b.pts());
            let mut f = locked(&first);
            if video {
                f.0 = pts;
            } else {
                f.1 = pts;
            }
            gst::PadProbeReturn::Remove
        });
    }
    first
}

fn link_to_mux(src: &gst::Element, mux: &gst::Element, template: &str) -> Result<(), String> {
    let pad = mux
        .request_pad_simple(template)
        .ok_or_else(|| format!("output: flvmux gave no {template} pad"))?;
    src.static_pad("src")
        .ok_or("output: appsrc has no src pad")?
        .link(&pad)
        .map(|_| ())
        .map_err(|e| format!("output: {template} link: {e:?}"))
}

fn into_appsrc(e: gst::Element) -> Result<gst_app::AppSrc, String> {
    e.downcast::<gst_app::AppSrc>()
        .map_err(|_| "output: appsrc is not an AppSrc".to_owned())
}

/// When an output generation may start taking the ring (Owed 84).
enum Gate {
    /// A file is written as soon as the pipeline plays: open at the build.
    Open,
    /// rtmp2sink, open once its publish is accepted. Measured in GStreamer 1.24.2's
    /// `gstrtmp2sink.c`: `render` waits while `self->connection` is unset, and `connect_task_done`
    /// sets it only after the publish's `NetStream.Publish.Start` (`start_publish_done`). `stats`
    /// reads that connection's counters; without one, the counters saved when a connection ended,
    /// and the null stats (every field 0) before any. Each build makes a new rtmp2sink, whose saved
    /// counters are cleared when its connect task starts, so this sink's `out-bytes-total` leaves 0
    /// exactly when its publish is accepted: the connection has sent its `connect` command by then
    /// (476 B, F-011). Anything fed before that queues in front of a sink that cannot send, and goes
    /// out in one burst at the connect (F-023). The gate is read at each ring push and each `WAKE`
    /// while the sink connects, about 100 reads a second, each a lock and a copy of the stats
    /// structure; it stops at the open (F-027).
    Publish(gst::Element),
}

impl Gate {
    fn is_open(&self) -> bool {
        match self {
            Gate::Open => true,
            Gate::Publish(sink) => rtmp_total(sink) > 0,
        }
    }
}

/// rtmp2sink `stats`'s `out-bytes-total` (F-010): the bytes its connection has sent, 0 before it has
/// one.
fn rtmp_total(sink: &gst::Element) -> u64 {
    // GStreamer 1.24's `stats` contract (F-027); `the_gate_reads_gstreamer_1_24` pins the version.
    sink.property::<gst::Structure>("stats")
        .get::<u64>("out-bytes-total")
        .unwrap_or(0)
}

/// Takes the ring into one output generation.
struct Feeder {
    gen: u64,
    vsrc: gst_app::AppSrc,
    asrc: gst_app::AppSrc,
    ring: Arc<EncodedRing>,
    retired: Arc<AtomicBool>,
    gate: Gate,
    request_key: KeyRequest,
    shared: Shared,
}

enum Next {
    Entry(Entry),
    Eos,
    Stop,
}

/// The keyframe asked for at the reconnect point (Owed 84): asked again at each entry until the
/// encoder takes it or the stream has started, so a refused request does not leave the stream
/// waiting for the encoder's own keyframe, up to 2 s away (PR #57 review m-6).
#[derive(Default)]
struct KeyAsk {
    tries: u32,
    taken: bool,
}

impl Feeder {
    fn run(self) {
        let Some(reconnect) = self.reconnect_point() else {
            return;
        };
        let mut pick = Picker::new(reconnect);
        let mut ask = KeyAsk::default();
        self.ask_key(&mut ask);
        loop {
            match self.next() {
                Next::Stop => return,
                Next::Eos => {
                    if !pick.started() {
                        self.release_unsent();
                    }
                    let _ = self.vsrc.end_of_stream();
                    let _ = self.asrc.end_of_stream();
                    return;
                }
                Next::Entry(e) => {
                    if !pick.started() {
                        self.ask_key(&mut ask);
                    }
                    pick.take(e, |e, base| self.push(e, base));
                }
            }
        }
    }

    /// Asks the encoder for a keyframe, unless it has taken a request already.
    fn ask_key(&self, ask: &mut KeyAsk) {
        if ask.taken {
            return;
        }
        ask.tries += 1;
        let taken = (self.request_key)();
        ask.taken = taken;
        if !taken && ask.tries == 1 {
            self.shared.log(&format!(
                "output gen={}: the keyframe request was not taken; it is asked again at each frame",
                self.gen
            ));
        } else if taken && ask.tries > 1 {
            self.shared.log(&format!(
                "output gen={}: the keyframe request was taken at try {}",
                self.gen, ask.tries
            ));
        }
    }

    /// The drain reached an output that has sent nothing, at the gate or after it (PR #57 review
    /// I-1). Nothing is owed to it, and it could never drain: flvmux 1.24.2 forwards no EOS before
    /// its first buffer (`gst_flv_mux_aggregate` asks for data in its header state, gstflvmux.c
    /// 2057-2059), so its sink never posts the EOS a drain waits for. The ring is released, and the
    /// drain ends with the encoders, as after a sink error (`Watch`).
    fn release_unsent(&self) {
        self.ring.release(self.gen);
        if self.ring.take_drained_without_output() {
            self.shared.send(Event::OutputDrained);
        }
    }

    /// Waits, taking nothing, until the sink can send (`Gate`), and returns the reconnect point: the
    /// seq of the first entry encoded after it (`run` then asks for the keyframe). `None`: retired,
    /// the ring is another generation's, or both encoders ended first. That last is the programme's
    /// drain reaching a sink that never connected: nothing is owed to it (whatever the ring holds
    /// predates any connect), so `release_unsent`.
    fn reconnect_point(&self) -> Option<u64> {
        let since = Instant::now();
        loop {
            if self.retired.load(Ordering::SeqCst) {
                return None;
            }
            if self.gate.is_open() {
                let r = locked(&self.ring.state);
                if r.owner != self.gen {
                    return None;
                }
                let first = r.next_seq;
                let held = r.entries.iter().filter(|e| e.video).count();
                drop(r);
                self.shared.log(&format!(
                    "output gen={}: the sink can send after {} ms; the {held} video frames queued \
                     before it are dropped, and it starts at the next keyframe",
                    self.gen,
                    since.elapsed().as_millis()
                ));
                return Some(first);
            }
            let r = locked(&self.ring.state);
            if r.owner != self.gen {
                return None;
            }
            if r.video_eos && r.audio_eos {
                drop(r);
                self.release_unsent();
                return None;
            }
            drop(
                self.ring
                    .wake
                    .wait_timeout(r, WAKE)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }

    /// Pushes `e` rebased to `base`.
    fn push(&self, e: &Entry, base: gst::ClockTime) {
        let src = if e.video { &self.vsrc } else { &self.asrc };
        if let Some(sample) = rebased(&e.sample, base) {
            let _ = src.push_sample(&sample);
        }
    }

    fn next(&self) -> Next {
        let mut r = locked(&self.ring.state);
        loop {
            if self.retired.load(Ordering::SeqCst) || r.owner != self.gen {
                return Next::Stop;
            }
            if r.entries.is_empty() {
                if r.video_eos && r.audio_eos {
                    return Next::Eos;
                }
            } else if !self.backed_up() {
                let e = r.entries.pop_front().expect("checked non-empty");
                return Next::Entry(e);
            }
            r = self
                .ring
                .wake
                .wait_timeout(r, WAKE)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The sink is not consuming: both appsrcs hold data, so the muxer has what it needs, and one of
    /// them holds `OUTPUT_QUEUE_BYTES`. A pad that is empty is never waited on (the muxer may be
    /// waiting for exactly that pad).
    fn backed_up(&self) -> bool {
        let v = self.vsrc.current_level_bytes();
        let a = self.asrc.current_level_bytes();
        v > 0 && a > 0 && (v >= OUTPUT_QUEUE_BYTES || a >= OUTPUT_QUEUE_BYTES)
    }
}

/// Which ring entries one output generation sends, and the base each is rebased by. No GStreamer
/// call: the feeder pushes what `take` hands it.
///
/// - The first entry sent is a video keyframe, its PTS the base (0 on the wire). A keyframe whose
///   seq is below the reconnect point was queued before the sink could send, so it never starts the
///   stream, and neither does any frame behind it (Owed 84).
/// - Audio that reaches the ring before that keyframe is held, bounded like the ring: the audio
///   encoder runs about 60 ms ahead of the video one, so a keyframe's first audio arrives before it
///   (dropping it lost 2 or 3 AAC frames per sink). Once the keyframe sets the base, the held audio
///   at or after it is sent, and no audio before it, so the audio never starts ahead of the video.
/// - A seq gap (the ring dropped entries this output never took) makes the video resume at a
///   keyframe.
pub(crate) struct Picker<S = gst::Sample> {
    reconnect: u64,
    base: Option<gst::ClockTime>,
    need_key: bool,
    expected_seq: Option<u64>,
    early_audio: VecDeque<Entry<S>>,
}

impl<S> Picker<S> {
    /// `reconnect`: the seq of the first entry encoded after the sink could send.
    pub(crate) fn new(reconnect: u64) -> Self {
        Self {
            reconnect,
            base: None,
            need_key: true,
            expected_seq: None,
            early_audio: VecDeque::new(),
        }
    }

    /// Whether anything has been sent: the first keyframe set the base.
    pub(crate) fn started(&self) -> bool {
        self.base.is_some()
    }

    /// Hands `send` each entry to send now, in order, with its base.
    pub(crate) fn take(&mut self, e: Entry<S>, mut send: impl FnMut(&Entry<S>, gst::ClockTime)) {
        if self.expected_seq.is_some_and(|s| s != e.seq) {
            self.need_key = true;
        }
        self.expected_seq = Some(e.seq + 1);
        if e.video && self.need_key {
            if !e.key || e.seq < self.reconnect {
                return;
            }
            self.need_key = false;
            if self.base.is_none() {
                let base = e.pts;
                self.base = Some(base);
                send(&e, base);
                for a in self.early_audio.drain(..) {
                    if a.pts >= base {
                        send(&a, base);
                    }
                }
                return;
            }
        }
        let Some(base) = self.base else {
            if !e.video {
                if self.early_audio.len() >= ENCODED_RING_MAX_ENTRIES {
                    self.early_audio.pop_front();
                }
                self.early_audio.push_back(e);
            }
            return;
        };
        if e.pts >= base {
            send(&e, base);
        }
    }
}

/// `sample` with its PTS and DTS moved back by `base` (a shallow copy: the memory is shared).
fn rebased(sample: &gst::Sample, base: gst::ClockTime) -> Option<gst::Sample> {
    let mut buffer = sample.buffer_owned()?;
    {
        let b = buffer.make_mut();
        let pts = b.pts().map(|p| p.saturating_sub(base));
        let dts = b.dts().map(|d| d.saturating_sub(base));
        b.set_pts(pts);
        b.set_dts(dts);
    }
    let mut builder = gst::Sample::builder().buffer(&buffer);
    let caps = sample.caps_owned();
    if let Some(caps) = &caps {
        builder = builder.caps(caps);
    }
    Some(builder.build())
}

/// The output's bus and its progress poll.
struct Watch {
    gen: u64,
    /// This sink's serial in `Stats` (`Stats::on_new_sink`).
    stats_sink: u64,
    /// Progress events not delivered since the last one that was.
    dropped_streak: u64,
    bus: gst::Bus,
    sink: gst::Element,
    file: Option<PathBuf>,
    shared: Shared,
    retired: Arc<AtomicBool>,
}

impl Watch {
    fn run(mut self) {
        let poll = Duration::from_millis(OUTPUT_PROGRESS_POLL_MS);
        let mut next_poll = Instant::now() + poll;
        let mut failed = false;
        loop {
            if self.retired.load(Ordering::SeqCst) {
                return;
            }
            let wait = next_poll.saturating_duration_since(Instant::now()).min(BUS_WAIT);
            let msg = self.bus.timed_pop_filtered(
                gst::ClockTime::from_nseconds(wait.as_nanos() as u64),
                &[
                    gst::MessageType::Error,
                    gst::MessageType::Warning,
                    gst::MessageType::Eos,
                ],
            );
            if let Some(msg) = msg {
                if self.retired.load(Ordering::SeqCst) {
                    return;
                }
                match msg.view() {
                    gst::MessageView::Error(e) => {
                        self.shared.log(&format!(
                            "output gen={} error: {} ({})",
                            self.gen,
                            e.error(),
                            e.debug().as_deref().unwrap_or("")
                        ));
                        if !failed {
                            failed = true;
                            // Nothing more reaches a failed sink, so the ring is no longer its: a
                            // drain now ends when the encoders do, whether it started before the
                            // failure or after it, with the dead sink still in place (PR #28 review
                            // item 3; the domain swaps it only after its back-off, and never in
                            // the hold).
                            self.shared.ring.release(self.gen);
                            self.shared.send(Event::OutputError { gen: self.gen });
                            if self.shared.ring.take_drained_without_output() {
                                self.shared.send(Event::OutputDrained);
                            }
                        }
                    }
                    gst::MessageView::Warning(w) => self.shared.log(&format!(
                        "output gen={} warning: {} ({})",
                        self.gen,
                        w.error(),
                        w.debug().as_deref().unwrap_or("")
                    )),
                    gst::MessageView::Eos(_) => {
                        // One final sample, so the last snapshot equals what the sink wrote; a sink
                        // that failed is not sampled again (its EOS follows the error).
                        if !failed {
                            self.progress();
                        }
                        if self.shared.ring.take_drained_at_eos() {
                            self.shared.send(Event::OutputDrained);
                        }
                    }
                    _ => {}
                }
            }
            if Instant::now() >= next_poll {
                next_poll += poll;
                if next_poll < Instant::now() {
                    next_poll = Instant::now() + poll;
                }
                if !failed {
                    self.progress();
                }
            }
        }
    }

    /// One reading into `Stats` and one `OutputProgress`, never waited for: a full channel drops it
    /// (the next poll carries a newer total), counts it, and says so once when the channel fills and
    /// once, with the count, when an event is delivered again.
    fn progress(&mut self) {
        let total = self.total();
        self.shared.stats.on_sink_total(self.stats_sink, total);
        let e = Inbound::Event(Event::OutputProgress {
            gen: self.gen,
            total_bytes: total,
        });
        if self.shared.tx.try_send(e).is_err() {
            self.shared.progress_dropped.fetch_add(1, Ordering::Relaxed);
            if self.dropped_streak == 0 {
                self.shared.log(&format!(
                    "output gen={}: the inbound channel is full; progress events are dropped until it drains",
                    self.gen
                ));
            }
            self.dropped_streak += 1;
        } else if self.dropped_streak > 0 {
            self.shared.log(&format!(
                "output gen={}: {} progress events were dropped while the inbound channel was full",
                self.gen, self.dropped_streak
            ));
            self.dropped_streak = 0;
        }
    }

    /// The sink's bytes-sent total: rtmp2sink `stats`'s `out-bytes-total` (F-010), or the file's
    /// size on disk.
    fn total(&self) -> u64 {
        match &self.file {
            Some(path) => std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            None => rtmp_total(&self.sink),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Owed 84's rule, sample-free: an entry is its seq (the sample) with its kind, key flag and PTS.
    fn entry(seq: u64, video: bool, key: bool, pts_ms: u64) -> Entry<u64> {
        let mut e = Entry::new(seq, video, key, gst::ClockTime::from_mseconds(pts_ms));
        e.seq = seq;
        e
    }

    /// Every entry `take` sends for `entries`, as (seq, base ms), in order.
    fn sent(reconnect: u64, entries: Vec<Entry<u64>>) -> Vec<(u64, u64)> {
        let mut pick = Picker::new(reconnect);
        let mut out = Vec::new();
        for e in entries {
            pick.take(e, |e, base| out.push((e.sample, base.mseconds())));
        }
        out
    }

    /// A ring as the encoders fill it (30 fps video, 48 kHz AAC whose encoder runs about 60 ms ahead),
    /// holding a natural keyframe (seq 0) when a sink can send at seq 6. The control run (a reconnect
    /// point of 0) starts at that keyframe, so it is the reconnect point alone that moves the start.
    fn ring_with_an_old_keyframe() -> Vec<Entry<u64>> {
        vec![
            entry(0, true, true, 0),
            entry(1, false, false, 0),
            entry(2, false, false, 21),
            entry(3, true, false, 33),
            entry(4, false, false, 43),
            entry(5, true, false, 67),
            // The reconnect point: seq 6 is the first entry encoded after the sink could send.
            entry(6, false, false, 64),
            entry(7, true, false, 100),
            entry(8, false, false, 85),
            entry(9, false, false, 107),
            entry(10, false, false, 128),
            entry(11, false, false, 149),
            // The keyframe the reconnect asked for.
            entry(12, true, true, 133),
            entry(13, false, false, 171),
            entry(14, true, false, 167),
        ]
    }

    #[test]
    fn a_keyframe_queued_before_the_reconnect_point_never_starts_the_stream() {
        // Control: with the reconnect point at the ring's start, its old keyframe starts the stream.
        let all = sent(0, ring_with_an_old_keyframe());
        assert_eq!(all.first(), Some(&(0, 0)), "{all:?}");
        // From seq 6: the first entry sent is the keyframe asked for (seq 12, PTS 133 ms the base),
        // then the held audio at or after it (149 ms; 64 to 128 ms are older), then the rest.
        assert_eq!(
            sent(6, ring_with_an_old_keyframe()),
            vec![(12, 133), (11, 133), (13, 133), (14, 133)]
        );
    }

    #[test]
    fn with_no_keyframe_after_the_reconnect_point_nothing_is_sent() {
        let ring: Vec<_> = ring_with_an_old_keyframe()
            .into_iter()
            .filter(|e| e.seq != 12)
            .collect();
        assert_eq!(sent(6, ring), vec![]);
        // An empty ring sends nothing, whatever the point.
        assert_eq!(sent(0, vec![]), vec![]);
    }

    // `started` is what the feeder's drain reads (PR #57 review I-1): false while every entry was
    // refused or held, true from the first one sent, each read checked against what `take` sent.
    #[test]
    fn started_reads_whether_anything_was_sent() {
        let mut checked = 0;
        for reconnect in [0, 6, 13] {
            let mut pick = Picker::new(reconnect);
            assert!(!pick.started(), "{reconnect}: before any entry");
            let mut sent_any = false;
            for e in ring_with_an_old_keyframe() {
                pick.take(e, |_, _| sent_any = true);
                assert_eq!(pick.started(), sent_any, "{reconnect}");
                checked += 1;
            }
            // 13 is past the ring's last keyframe (12): nothing is ever sent.
            assert_eq!(sent_any, reconnect <= 12, "{reconnect}");
        }
        assert_eq!(checked, 3 * ring_with_an_old_keyframe().len());
    }

    #[test]
    fn a_gap_after_the_start_resumes_the_video_at_a_keyframe_and_keeps_the_audio() {
        let mut ring = ring_with_an_old_keyframe();
        ring.extend([
            // The ring dropped seq 15 to 19, so 20 (a delta) follows a gap and is not sent.
            entry(20, true, false, 300),
            entry(21, false, false, 299),
            entry(22, true, true, 333),
            entry(23, true, false, 367),
        ]);
        assert_eq!(sent(6, ring)[4..].to_vec(), vec![(21, 133), (22, 133), (23, 133)]);
    }

    /// A deterministic generator (an LCG, so a failure replays).
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n
        }
    }

    /// One generated ring: what the encoders push (30 fps video with a keyframe every `gop` frames or
    /// at random, AAC frames of 1 024 samples at 48 kHz), each encoder late by up to 60 ms of its own,
    /// so the audio arrives ahead of the video or behind it; with runs of entries this output never
    /// took (the ring dropped them), and a reconnect point anywhere.
    fn generated(rng: &mut Lcg) -> (u64, Vec<Entry<u64>>) {
        let gop = 1 + rng.below(60);
        let (video_late, audio_late) = (rng.below(61), rng.below(61));
        let frames = 1 + rng.below(240);
        let mut timed: Vec<(u64, bool, bool)> = Vec::new();
        for i in 0..frames {
            let key = i % gop == 0 || rng.below(40) == 0;
            timed.push((i * 1_000 / 30, true, key));
        }
        let mut a = 0u64;
        while a * 1_024 * 1_000 / 48_000 < frames * 1_000 / 30 {
            timed.push((a * 1_024 * 1_000 / 48_000, false, false));
            a += 1;
        }
        let arrival =
            |&(pts, video, _): &(u64, bool, bool)| pts + if video { video_late } else { audio_late };
        timed.sort_by_key(arrival);
        let mut out = Vec::new();
        let mut seq = 0u64;
        for (pts, video, key) in timed {
            // About one entry in 50 starts a dropped run of 1 to 30.
            if rng.below(50) == 0 {
                seq += 1 + rng.below(30);
            }
            out.push(entry(seq, video, key, pts));
            seq += 1;
        }
        let reconnect = rng.below(seq + 2);
        (reconnect, out)
    }

    /// Owed 84 and the A/V rule over 4 000 generated rings: the first entry sent is a video keyframe
    /// at or past the reconnect point and its PTS is the base; no video from before that point is
    /// sent; nothing older than the base is sent; no audio goes before the first video; the video
    /// after a dropped run restarts at a keyframe; each kind's PTS never goes back.
    #[test]
    fn a_generated_sweep_never_sends_a_frame_from_before_the_reconnect_point() {
        let mut rng = Lcg(84);
        let (mut cases, mut started, mut old_key_refused, mut gaps_resumed, mut audio_held) = (0, 0, 0, 0, 0);
        let mut late_audio_refused = 0;
        for _ in 0..4_000 {
            let (reconnect, ring) = generated(&mut rng);
            let old_key = ring.iter().any(|e| e.video && e.key && e.seq < reconnect);
            let mut pick = Picker::new(reconnect);
            let mut out: Vec<(Entry<u64>, u64)> = Vec::new();
            let mut last_taken: Option<u64> = None;
            let mut gap_since_video = false;
            for e in ring {
                if last_taken.is_some_and(|s| s + 1 != e.seq) {
                    gap_since_video = true;
                }
                last_taken = Some(e.seq);
                let before = out.len();
                let late = before > 0 && !e.video && e.pts.mseconds() < out[0].1;
                pick.take(e, |e, base| out.push((e.clone(), base.mseconds())));
                if late && out.len() == before {
                    late_audio_refused += 1;
                }
                if before == 0 && out.len() > 1 {
                    audio_held += 1;
                }
                for (e, _) in &out[before..] {
                    if e.video {
                        if gap_since_video && before > 0 {
                            assert!(
                                e.key,
                                "case {}: video seq {} after a dropped run is not a keyframe",
                                cases + 1,
                                e.seq
                            );
                            gaps_resumed += 1;
                        }
                        gap_since_video = false;
                    }
                }
            }
            cases += 1;
            let Some((first, base)) = out.first() else {
                continue;
            };
            started += 1;
            assert!(
                first.video && first.key,
                "case {cases}: the first entry sent is not a keyframe"
            );
            assert!(
                first.seq >= reconnect,
                "case {cases}: it was queued before the reconnect point"
            );
            assert_eq!(
                first.pts.mseconds(),
                *base,
                "case {cases}: its PTS is not the base"
            );
            if old_key {
                old_key_refused += 1;
            }
            let (mut last_v, mut last_a) = (None, None);
            for (e, b) in &out {
                assert_eq!(b, base, "case {cases}: one base for the whole output");
                assert!(
                    e.pts.mseconds() >= *base,
                    "case {cases}: seq {} is older than the base",
                    e.seq
                );
                assert!(
                    !e.video || e.seq >= reconnect,
                    "case {cases}: video seq {} predates the reconnect",
                    e.seq
                );
                let last = if e.video { &mut last_v } else { &mut last_a };
                assert!(
                    last.is_none_or(|p| e.pts >= p),
                    "case {cases}: seq {} goes back",
                    e.seq
                );
                *last = Some(e.pts);
            }
        }
        eprintln!(
            "cases {cases}, started {started}, with an old keyframe refused {old_key_refused}, \
             video resumed after a gap {gaps_resumed}, held audio sent {audio_held}, \
             late audio refused {late_audio_refused}"
        );
        assert_eq!(cases, 4_000);
        for (what, n) in [
            ("started", started),
            ("an old keyframe refused", old_key_refused),
            ("resumed after a gap", gaps_resumed),
            ("held audio sent with the keyframe", audio_held),
            (
                "audio older than the base arriving after it, refused",
                late_audio_refused,
            ),
        ] {
            assert!(n >= 30, "the sweep reached {what} only {n} times");
        }
    }
}
