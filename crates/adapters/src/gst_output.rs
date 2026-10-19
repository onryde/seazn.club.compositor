//! The output pipeline (C09; spec §3.3's refinement): `appsrc(h264) + appsrc(aac) → flvmux
//! streamable=true → rtmp2sink`, or `filesink` for a `file://` target. Rebuilt on every
//! `swap_sink(gen, target)`; the programme pipeline that feeds it is never rebuilt.
//!
//! The two pipelines are joined only by the **encoded ring**: the programme's encoder appsinks push
//! every encoded sample into it (2 s by PTS, dropping the oldest; plan, Global Constraints), and one
//! feeder thread per output generation takes from it. A new output starts at a video keyframe (the
//! feeder skips the ring up to the first one, and the swap asks the encoder for one), and its PTS
//! starts at 0.
//!
//! **Progress** (Owed 13). A poll every `OUTPUT_PROGRESS_POLL_MS` reads the sink's bytes-SENT total,
//! rtmp2sink `stats` field `out-bytes-total` (F-010), or the file's size for `file://`, and sends
//! `Event::OutputProgress { gen, total_bytes }`. Never acknowledged bytes: CF Input B sends none
//! (F-005). A bus error sends `Event::OutputError { gen }`, and its text goes to the log only after
//! the scrubber. The sink's `Eos` after `drain()` takes one final total into `Stats` and sends
//! `Event::OutputDrained`.
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
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::redact::Scrubber;
use crate::stats::Stats;

/// How often the output's bytes-sent total is read (Owed 15, the choice recorded in `_FINDINGS.md`
/// F-011): 250 ms, so the stall rule fires at most 250 ms after the true last byte plus
/// `output_stall_after_ms` (5 s), and the PDF's "detected within 5 s" claim keeps its 5 s with a
/// quarter-second poll term. A 1 s poll would add a whole second (Owed 15). C14 case 5's bound
/// reads this const's value, never a literal.
pub const OUTPUT_PROGRESS_POLL_MS: u64 = 250;

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

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One encoded sample, its PTS already in running time (x264enc's output segment starts at 1 000 h,
/// so a raw PTS would put the video 1 000 h ahead of the audio: spike trap, F-001's chain).
#[derive(Clone)]
pub(crate) struct Entry {
    seq: u64,
    pub(crate) sample: gst::Sample,
    pub(crate) video: bool,
    pub(crate) key: bool,
    pub(crate) pts: gst::ClockTime,
}

impl Entry {
    pub(crate) fn new(sample: gst::Sample, video: bool, key: bool, pts: gst::ClockTime) -> Self {
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
/// encoder's next keyframe is asked for by the caller (`force_key`).
pub(crate) fn build(shared: &Shared, gen: u64, target: &TargetSpec) -> Result<OutputGen, String> {
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

/// Takes the ring into one output generation.
struct Feeder {
    gen: u64,
    vsrc: gst_app::AppSrc,
    asrc: gst_app::AppSrc,
    ring: Arc<EncodedRing>,
    retired: Arc<AtomicBool>,
}

enum Next {
    Entry(Entry),
    Eos,
    Stop,
}

impl Feeder {
    fn run(self) {
        // The first entry pushed is a video keyframe; its PTS becomes 0.
        let mut base: Option<gst::ClockTime> = None;
        let mut need_key = true;
        let mut expected_seq: Option<u64> = None;
        // Audio that reached the ring before the first keyframe did: the audio encoder runs ahead
        // of the video one, so the keyframe's first ~60 ms of audio arrives before it. Held, and
        // pushed once the keyframe sets the base (dropping it lost 2 or 3 AAC frames per sink).
        // Bounded like the ring it came from.
        let mut early_audio: VecDeque<Entry> = VecDeque::new();
        loop {
            match self.next() {
                Next::Stop => return,
                Next::Eos => {
                    let _ = self.vsrc.end_of_stream();
                    let _ = self.asrc.end_of_stream();
                    return;
                }
                Next::Entry(e) => {
                    // The ring dropped entries this output never took: video resumes at a keyframe.
                    if expected_seq.is_some_and(|s| s != e.seq) {
                        need_key = true;
                    }
                    expected_seq = Some(e.seq + 1);
                    if e.video && need_key {
                        if !e.key {
                            continue;
                        }
                        need_key = false;
                        if base.is_none() {
                            base = Some(e.pts);
                            self.push(&e, e.pts);
                            for a in early_audio.drain(..) {
                                self.push(&a, e.pts);
                            }
                            continue;
                        }
                    }
                    let Some(base) = base else {
                        if !e.video {
                            if early_audio.len() >= ENCODED_RING_MAX_ENTRIES {
                                early_audio.pop_front();
                            }
                            early_audio.push_back(e);
                        }
                        continue;
                    };
                    self.push(&e, base);
                }
            }
        }
    }

    /// Pushes `e` rebased to `base`; an entry from before the base (audio older than the first
    /// keyframe) is not this output's.
    fn push(&self, e: &Entry, base: gst::ClockTime) {
        if e.pts < base {
            return;
        }
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
            None => self
                .sink
                .property::<gst::Structure>("stats")
                .get::<u64>("out-bytes-total")
                .unwrap_or(0),
        }
    }
}
