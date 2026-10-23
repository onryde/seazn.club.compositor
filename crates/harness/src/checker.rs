//! The reference checker's analysis (C13 brief, "Report schema"). It reports facts; **the cases
//! decide pass or fail** (C14), so one checker serves every case and the soak. Its one failure of its
//! own is a heartbeat `rung` the contract does not carry (Owed 20): see [`Report::checker_failures`].
//!
//! Every recorded frame gets a wall time: when the destination received it, as near as the
//! recording tells. MediaMTX names each segment by the ARRIVAL time of its first sample, and every
//! segment's PTS restarts near 0. A frame's wall time is therefore its segment's name plus its PTS
//! since the segment's first frame, while frames arrive at their own pace.
//!
//! A connection's first segment breaks that rule. The output's connect delivers a backlog, which
//! arrives as a burst: in smoke run 39979756278, segment 1 held 11.70 s of PTS but was named only
//! 9.71 s before segment 2. Read by name plus PTS, a segment like that runs past the next one's start
//! (an overlap), so its tail is dated up to the backlog too late.
//!
//! The checker re-dates such a segment from the next one. Its last frame is placed one period before
//! the next segment's start, less any camera time the strips show missing across the boundary
//! ([`SegmentOverlap::content_gap_ms`]). Every frame is dated no earlier than the segment's own name.
//! The backlog then reads as the burst it was, the tail as real time, and a real outage after the
//! backlog still reads as one. Nothing is folded away silently: each overlap is reported in
//! `segment_overlaps`.
//!
//! All the harness's containers share the host's clock, so recording, camera and heartbeat times
//! compare directly.
//!
//! Where the run's edges are (the brief, Owed 5 and the plan's 2026-10-15 C13/C14 amendment):
//! - a no-strip span from the first recorded frame is the **start** slate (the programme's Waiting
//!   phase before the camera's first second), reported in `camera.start_slate`;
//! - a no-strip span that starts at or after the case's end request is an **end** slate (the final
//!   hold), reported in `camera.end_slates`; with no end request given, the span that runs to the
//!   last recorded frame is the end slate;
//! - neither counts in `camera.slates` or `camera.slate_frames`, which are the run's interior.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::audio::AudioFacts;
use crate::inputs::{Beat, Emit, Timeline};

/// The rungs the contract carries (Owed 20: `Full → OverlayHalfRate`, and the bottom holds).
pub const RUNGS: [&str; 2] = ["full", "overlay_half_rate"];
/// A PTS step longer than this many frame periods is a gap (one frame missing or more).
pub const PTS_GAP_PERIODS: f64 = 1.5;
/// A wall gap between two segments longer than this many frame periods is an outage; a wall step
/// back as long, an overlap.
pub const OUTAGE_PERIODS: f64 = 3.0;
/// The wall window `latency_windows_ms` reports a p50 for (the brief: 10 s).
pub const LATENCY_WINDOW_MS: f64 = 10_000.0;
/// An audio onset pairs with the nearest burst frame no further away than this.
pub const AV_PAIR_WITHIN_MS: f64 = 500.0;
/// The fake camera's own AAC generation (voaacenc): its encoder delay, which the recording carries
/// unsignalled (no edit list in MPEG-TS or in MediaMTX's fMP4).
///
/// Measured with the checker's own decode on `tests/checker.rs`'s fixture, whose true offset is 0:
/// +33 ms on both pairs (CI run 39979756301), which is 1 600 samples at 48 kHz. The fixture's
/// shifted case checks it against a known offset.
///
/// It is subtracted from every pair, so `av_offset_ms` is the programme's offset with the harness's
/// camera taken out. The compositor's own encoder delay stays in, because the viewer hears it too.
pub const CAMERA_AAC_DELAY_MS: f64 = 1_600.0 / 48.0;
/// Fewer repeats than this after a counter's first frame are cadence (`camera.cadence`), not a
/// freeze. A 30 fps programme sampling a 30 fps camera repeats and skips single frames: the smoke run
/// had 456 one- and two-frame repeats (PR #48 review).
pub const FREEZE_MIN_REPEATS: u64 = 3;
/// A camera gap's covering slate span starts no later than this after the camera resumes. Without
/// one, the resume has no covering span (`first_shown_counter: None`): a later, unrelated slate never
/// stands in.
pub const RESUME_SPAN_WITHIN_MS: i64 = 10_000;
/// A crash logged this long before the beat that still read `ready` counts for the next beat.
pub const CRASH_SLACK_MS: i64 = 1_000;

/// One decoded programme frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRec {
    pub pts_ns: u64,
    pub counter: Option<u32>,
    pub score: Option<u16>,
}

/// One recorded segment, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentScan {
    /// The file, relative to the recordings directory.
    pub file: String,
    pub start_wall_us: i64,
    pub frames: Vec<FrameRec>,
    pub audio: AudioFacts,
    /// A decoder error part way through; the frames before it are kept.
    pub decode_error: Option<String>,
}

/// Everything the analysis reads.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub segments: Vec<SegmentScan>,
    pub emit: Vec<Emit>,
    pub emit_truncated: bool,
    pub beats: Vec<Beat>,
    pub beats_truncated: bool,
    pub timeline: Timeline,
    /// `cef crashed` lines in the compositor's log, epoch ms.
    pub crashes_ms: Vec<i64>,
    /// When the case asked the run to end (run-case.sh's `run.json`), epoch ms.
    pub end_request_ms: Option<i64>,
}

/// The analysis's settings: the camera's frame rate, and the programme threshold it measures
/// resumes against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// `Thresholds::slate_after_ms`: a camera gap at least this long slates the programme.
    pub slate_after_ms: i64,
    /// The camera's frame period (30 fps).
    pub camera_period_ms: f64,
}

impl Settings {
    pub fn from_thresholds(t: &domain::config::Thresholds) -> Self {
        Self {
            slate_after_ms: t.slate_after_ms as i64,
            camera_period_ms: 1000.0 / 30.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PtsGap {
    pub at_ms: i64,
    pub gap_ms: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SegmentFact {
    pub file: String,
    pub start_wall_ms: i64,
    pub frames: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Outage {
    pub from_wall_ms: i64,
    pub to_wall_ms: i64,
    /// Where the outage sits:
    /// - `head`: from the first `live` beat to the first frame;
    /// - `between`: across a segment boundary;
    /// - `tail`: from the last frame to the end request.
    pub kind: &'static str,
}

/// A segment whose frames, read by name and PTS, run `overlap_ms` past the next segment's start (a
/// connection's backlog: the module comment). The checker re-dates it from the next segment.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SegmentOverlap {
    /// The segment that was re-dated.
    pub file: String,
    pub next_file: String,
    pub overlap_ms: i64,
    /// Camera time the strips show missing across the boundary, beyond what the programme frames
    /// between them cover. `None` when one side has no strip, so nothing could tell a roll from an
    /// outage, and the boundary was taken as a roll.
    pub content_gap_ms: Option<i64>,
}

/// Consecutive strip frames, compared: the programme's cadence against the camera's.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Cadence {
    /// Pairs of consecutive frames that both carry a strip.
    pub pairs: u64,
    /// Frames that repeat the counter before them. A freeze's repeats are counted too.
    pub repeated_frames: u64,
    /// Counters the camera emitted that no frame showed, between two consecutive frames.
    pub skipped_counters: u64,
    /// Pairs with at least one skipped counter between them.
    pub skip_events: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Freeze {
    pub from_ms: i64,
    pub ms: i64,
    /// Frames that repeated the counter, after the frame that first showed it.
    pub frames: u64,
    pub counter: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SlateSpan {
    pub from_ms: i64,
    pub ms: i64,
    pub frames: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Resume {
    pub gap_from_wall_ms: i64,
    pub gap_ms: i64,
    pub first_emitted_counter: u32,
    /// `None` when no slate span starts between the gap's start and [`RESUME_SPAN_WITHIN_MS`] after
    /// the camera resumes, or when no strip frame follows that span.
    pub first_shown_counter: Option<u32>,
    pub suppressed_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Percentiles {
    pub p50: i64,
    pub p95: i64,
    pub max: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LatencyWindow {
    pub from_wall_ms: i64,
    pub to_wall_ms: i64,
    pub p50: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Camera {
    pub strip_frames: u64,
    pub slate_frames: u64,
    pub freezes: Vec<Freeze>,
    pub slates: Vec<SlateSpan>,
    pub start_slate: Option<SlateSpan>,
    pub end_slates: Vec<SlateSpan>,
    pub counter_regressions: u64,
    pub cadence: Cadence,
    pub resumes: Vec<Resume>,
    pub latency_ms: Percentiles,
    /// How many counters the latency was measured on.
    pub latency_samples: u64,
    pub latency_windows_ms: Vec<LatencyWindow>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScoreChange {
    pub frame_wall_ms: i64,
    pub index: u16,
    pub due_wall_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Overlay {
    pub score_index_changes: Vec<ScoreChange>,
    pub late_frames_max: u64,
    pub future_dated: u64,
    /// Frames whose score strip read.
    pub frames_read: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct AvOffset {
    pub p50: i64,
    pub max_abs: i64,
    /// Onsets paired with a burst frame.
    pub pairs: u64,
    /// Subtracted from each pair: the camera's own AAC delay ([`CAMERA_AAC_DELAY_MS`]).
    pub camera_codec_delay_ms: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AudioGap {
    pub at_ms: i64,
    pub ms: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Audio {
    pub av_offset_ms: AvOffset,
    pub silent_gaps: Vec<AudioGap>,
    pub onsets: u64,
    pub samples: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Heartbeats {
    pub count: u64,
    pub max_gap_ms: i64,
    pub fields_seen: BTreeMap<String, u64>,
    pub last: Value,
    pub rungs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Graphics {
    /// Beats that move `graphics` out of `ready` with no `cef crashed` logged since the beat before
    /// (the 2026-10-18 amendment, Owed 18): a hang, a recycle, or a page that never beats.
    pub restarts_without_crash: u64,
    /// Every beat that moves `graphics` out of `ready`.
    pub ready_exits: u64,
    pub crashes_logged: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SegmentError {
    pub file: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InputFacts {
    pub segments_read: u64,
    pub decode_errors: Vec<SegmentError>,
    pub frame_period_ms: f64,
    pub emit_lines: u64,
    pub emit_truncated: bool,
    pub beats_truncated: bool,
    pub end_request_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub frames: u64,
    pub duration_ms: i64,
    pub fps_mean: f64,
    pub pts_gaps: Vec<PtsGap>,
    pub segments: Vec<SegmentFact>,
    pub output_outages: Vec<Outage>,
    pub segment_overlaps: Vec<SegmentOverlap>,
    pub camera: Camera,
    pub overlay: Overlay,
    pub audio: Audio,
    pub heartbeats: Heartbeats,
    pub graphics: Graphics,
    pub inputs: InputFacts,
    /// Why the checker itself fails the run (any rung outside [`RUNGS`]); empty on a run it accepts.
    pub checker_failures: Vec<String>,
}

/// A frame with its wall time.
#[derive(Debug, Clone, Copy)]
struct Frame {
    wall: f64,
    counter: Option<u32>,
    score: Option<u16>,
}

fn round(x: f64) -> i64 {
    x.round() as i64
}

/// Nearest-rank percentile of a sorted list (`p` in 0..=100); 0 for an empty list.
fn percentile(sorted: &[i64], p: u64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p as usize * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

fn percentiles(mut v: Vec<i64>) -> Percentiles {
    v.sort_unstable();
    Percentiles {
        p50: percentile(&v, 50),
        p95: percentile(&v, 95),
        max: v.last().copied().unwrap_or(0),
    }
}

/// The recording's frame period, or 30 fps when no segment has two frames. Each segment gives its PTS
/// span over its frame steps, and the period is the median of those. A median step would read
/// RTMP's millisecond PTS as 33.0 instead of 33.333 (PR #48 review m5), and a segment's own gaps
/// raise only its own figure.
fn frame_period_ms(segments: &[SegmentScan]) -> f64 {
    let mut per: Vec<f64> = segments
        .iter()
        .filter_map(|s| match (s.frames.first(), s.frames.last()) {
            (Some(a), Some(b)) if s.frames.len() >= 2 && b.pts_ns > a.pts_ns => {
                Some((b.pts_ns - a.pts_ns) as f64 / 1e6 / (s.frames.len() - 1) as f64)
            }
            _ => None,
        })
        .collect();
    if per.is_empty() {
        return 1000.0 / 30.0;
    }
    per.sort_by(f64::total_cmp);
    per[per.len() / 2]
}

/// A segment's PTS origin: its first frame's PTS.
fn base_ns(s: &SegmentScan) -> u64 {
    s.frames.first().map_or(0, |f| f.pts_ns)
}

/// How one segment's frames are dated (the module comment).
#[derive(Debug, Clone, Copy)]
struct Dating {
    name_ms: f64,
    base_ns: u64,
    /// Set for a re-dated segment: its last frame's wall time and that frame's PTS.
    anchor: Option<(f64, u64)>,
}

impl Dating {
    fn of(s: &SegmentScan) -> Self {
        Self {
            name_ms: s.start_wall_us as f64 / 1000.0,
            base_ns: base_ns(s),
            anchor: None,
        }
    }

    fn wall(&self, pts_ns: u64) -> f64 {
        match self.anchor {
            None => self.name_ms + (pts_ns as f64 - self.base_ns as f64) / 1e6,
            Some((end_ms, last_ns)) => self.name_ms.max(end_ms - (last_ns as f64 - pts_ns as f64) / 1e6),
        }
    }
}

/// The camera time missing across the boundary from `a` to `b`, from the strips on either side:
/// the camera time between `a`'s last strip and `b`'s first strip, less the programme time the
/// frames between them cover. A roll reads 0, and so does a slate the programme kept showing.
/// `None` when either side has no strip.
fn content_gap_ms(a: &SegmentScan, b: &SegmentScan, period: f64, camera_period: f64) -> Option<f64> {
    let (ia, last) = a
        .frames
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, f)| f.counter.map(|c| (i, c)))?;
    let (ib, first) = b
        .frames
        .iter()
        .enumerate()
        .find_map(|(i, f)| f.counter.map(|c| (i, c)))?;
    let camera = f64::from(first.saturating_sub(last)) * camera_period;
    let shown = ((a.frames.len() - 1 - ia) + ib + 1) as f64 * period;
    Some((camera - shown).max(0.0))
}

/// When the first beat that reads `programmePhase: live` arrived.
fn first_live_ms(beats: &[Beat]) -> Option<i64> {
    beats
        .iter()
        .filter(|b| b.body.get("programmePhase").and_then(Value::as_str) == Some("live"))
        .map(|b| b.received_ms)
        .min()
}

pub fn analyze(input: &Inputs, settings: &Settings) -> Report {
    let mut segments = input.segments.clone();
    segments.sort_by_key(|s| s.start_wall_us);
    for s in &mut segments {
        s.frames.sort_by_key(|f| f.pts_ns);
    }
    let period = frame_period_ms(&segments);
    let threshold = OUTAGE_PERIODS * period;

    // The boundaries between segments with frames, judged by name and PTS. An overlap re-dates the
    // earlier segment; an outage is a gap either side of the overlap correction.
    let mut datings: Vec<Dating> = segments.iter().map(Dating::of).collect();
    let mut outages = Vec::new();
    let mut overlaps = Vec::new();
    let with_frames: Vec<usize> = (0..segments.len())
        .filter(|&k| !segments[k].frames.is_empty())
        .collect();
    for w in with_frames.windows(2) {
        let (ka, kb) = (w[0], w[1]);
        let (a, b) = (&segments[ka], &segments[kb]);
        let last_ns = a.frames[a.frames.len() - 1].pts_ns;
        let prev_end = datings[ka].wall(last_ns);
        let next_first = datings[kb].wall(b.frames[0].pts_ns);
        let gap = next_first - (prev_end + period);
        if gap > threshold {
            outages.push(Outage {
                from_wall_ms: round(prev_end + period),
                to_wall_ms: round(next_first),
                kind: "between",
            });
        } else if -gap > threshold {
            let content_gap = content_gap_ms(a, b, period, settings.camera_period_ms);
            let end = next_first - content_gap.unwrap_or(0.0) - period;
            datings[ka].anchor = Some((end, last_ns));
            if content_gap.is_some_and(|g| g > threshold) {
                outages.push(Outage {
                    from_wall_ms: round(end + period),
                    to_wall_ms: round(next_first),
                    kind: "between",
                });
            }
            overlaps.push(SegmentOverlap {
                file: a.file.clone(),
                next_file: b.file.clone(),
                overlap_ms: round(-gap),
                content_gap_ms: content_gap.map(round),
            });
        }
    }

    // Frames, wall-timed, in recording order; PTS gaps inside a segment.
    let mut frames: Vec<Frame> = Vec::new();
    let mut pts_gaps = Vec::new();
    let mut seg_facts = Vec::new();
    let mut content_ms = 0.0;
    for (s, d) in segments.iter().zip(&datings) {
        seg_facts.push(SegmentFact {
            file: s.file.clone(),
            start_wall_ms: s.start_wall_us.div_euclid(1000),
            frames: s.frames.len() as u64,
        });
        if let (Some(a), Some(b)) = (s.frames.first(), s.frames.last()) {
            content_ms += (b.pts_ns - a.pts_ns) as f64 / 1e6 + period;
        }
        for (i, f) in s.frames.iter().enumerate() {
            let wall = d.wall(f.pts_ns);
            if i > 0 {
                let step_ms = (f.pts_ns - s.frames[i - 1].pts_ns) as f64 / 1e6;
                if step_ms > PTS_GAP_PERIODS * period {
                    pts_gaps.push(PtsGap {
                        at_ms: round(wall - step_ms),
                        gap_ms: round(step_ms),
                    });
                }
            }
            frames.push(Frame {
                wall,
                counter: f.counter,
                score: f.score,
            });
        }
    }
    // The run's edges (PR #48 review, Gap Hunt): an output that starts late or dies early.
    if let (Some(first), Some(live)) = (frames.first(), first_live_ms(&input.beats)) {
        if first.wall - live as f64 > threshold {
            outages.push(Outage {
                from_wall_ms: live,
                to_wall_ms: round(first.wall),
                kind: "head",
            });
        }
    }
    if let (Some(last), Some(end)) = (frames.last(), input.end_request_ms) {
        if end as f64 - (last.wall + period) > threshold {
            outages.push(Outage {
                from_wall_ms: round(last.wall + period),
                to_wall_ms: end,
                kind: "tail",
            });
        }
    }
    outages.sort_by_key(|o| o.from_wall_ms);
    // The recorded content's length (every segment's PTS span), so a re-dated backlog shortens
    // nothing; the frame rate over it.
    let duration_ms = round(content_ms);
    let fps_mean = if duration_ms > 0 {
        frames.len() as f64 * 1000.0 / duration_ms as f64
    } else {
        0.0
    };

    let camera = camera(&frames, period, input, settings);
    let overlay = overlay(&frames, &input.timeline);
    let audio = audio(&segments, &datings, &frames);
    let (heartbeats, checker_failures) = heartbeats(&input.beats);
    let graphics = graphics(&input.beats, &input.crashes_ms);
    Report {
        frames: frames.len() as u64,
        duration_ms,
        fps_mean: (fps_mean * 1000.0).round() / 1000.0,
        pts_gaps,
        segments: seg_facts,
        output_outages: outages,
        segment_overlaps: overlaps,
        camera,
        overlay,
        audio,
        heartbeats,
        graphics,
        inputs: InputFacts {
            segments_read: segments.len() as u64,
            decode_errors: segments
                .iter()
                .filter_map(|s| {
                    s.decode_error.clone().map(|error| SegmentError {
                        file: s.file.clone(),
                        error,
                    })
                })
                .collect(),
            frame_period_ms: (period * 1000.0).round() / 1000.0,
            emit_lines: input.emit.len() as u64,
            emit_truncated: input.emit_truncated,
            beats_truncated: input.beats_truncated,
            end_request_ms: input.end_request_ms,
        },
        checker_failures,
    }
}

/// Runs of no-strip frames, as `(first index, length)`.
fn no_strip_runs(frames: &[Frame]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        if frames[i].counter.is_none() {
            let start = i;
            while i < frames.len() && frames[i].counter.is_none() {
                i += 1;
            }
            runs.push((start, i - start));
        } else {
            i += 1;
        }
    }
    runs
}

fn camera(frames: &[Frame], period: f64, input: &Inputs, settings: &Settings) -> Camera {
    let span = |(start, len): (usize, usize)| SlateSpan {
        from_ms: round(frames[start].wall),
        ms: round(len as f64 * period),
        frames: len as u64,
    };
    let mut slates = Vec::new();
    let mut start_slate = None;
    let mut end_slates = Vec::new();
    // Each slate span, with the index of the frame after it (for resumes).
    let mut all_spans: Vec<(SlateSpan, usize)> = Vec::new();
    for run in no_strip_runs(frames) {
        let s = span(run);
        all_spans.push((s.clone(), run.0 + run.1));
        let at_end = run.0 + run.1 == frames.len();
        let ends_run = match input.end_request_ms {
            Some(end) => frames[run.0].wall >= end as f64,
            None => at_end,
        };
        if run.0 == 0 {
            start_slate = Some(s);
        } else if ends_run {
            end_slates.push(s);
        } else {
            slates.push(s);
        }
    }
    let slate_frames = slates.iter().map(|s| s.frames).sum();

    // Freezes: a counter shown on at least FREEZE_MIN_REPEATS more consecutive frames.
    let mut freezes = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        let Some(c) = frames[i].counter else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < frames.len() && frames[j].counter == Some(c) {
            j += 1;
        }
        let repeats = (j - i - 1) as u64;
        if repeats >= FREEZE_MIN_REPEATS {
            freezes.push(Freeze {
                from_ms: round(frames[i + 1].wall),
                ms: round(repeats as f64 * period),
                frames: repeats,
                counter: c,
            });
        }
        i = j;
    }

    // Cadence: consecutive strip frames that repeat a counter, or pass over counters the camera
    // emitted (a counter the camera never emitted, in a pause, is no skip of the programme's).
    let emitted: BTreeMap<u32, i64> = input.emit.iter().map(|e| (e.counter, e.wall_ms)).collect();
    let mut cadence = Cadence::default();
    for w in frames.windows(2) {
        let (Some(a), Some(b)) = (w[0].counter, w[1].counter) else {
            continue;
        };
        cadence.pairs += 1;
        if b == a {
            cadence.repeated_frames += 1;
        } else if b > a.saturating_add(1) {
            let skipped = emitted.range(a + 1..b).count() as u64;
            if skipped > 0 {
                cadence.skip_events += 1;
                cadence.skipped_counters += skipped;
            }
        }
    }

    // Regressions, against the highest counter shown so far.
    let mut regressions = 0;
    let mut highest = 0u32;
    for c in frames.iter().filter_map(|f| f.counter) {
        if c < highest {
            regressions += 1;
        } else {
            highest = c;
        }
    }

    // Latency, on each counter's first frame.
    let mut seen = BTreeSet::new();
    let mut lat: Vec<(f64, i64)> = Vec::new();
    for f in frames {
        if let Some(c) = f.counter {
            if seen.insert(c) {
                if let Some(e) = emitted.get(&c) {
                    lat.push((f.wall, round(f.wall - *e as f64)));
                }
            }
        }
    }
    let mut windows = Vec::new();
    if let Some((w0, _)) = lat.first() {
        let mut by_window: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for (wall, l) in &lat {
            by_window
                .entry(((wall - w0) / LATENCY_WINDOW_MS).floor() as i64)
                .or_default()
                .push(*l);
        }
        for (k, v) in by_window {
            let from = w0 + k as f64 * LATENCY_WINDOW_MS;
            windows.push(LatencyWindow {
                from_wall_ms: round(from),
                to_wall_ms: round(from + LATENCY_WINDOW_MS),
                p50: percentiles(v).p50,
            });
        }
    }

    // Resumes: one per camera gap of at least slate_after_ms.
    let mut emit = input.emit.clone();
    emit.sort_by_key(|e| e.wall_ms);
    let mut resumes = Vec::new();
    for w in emit.windows(2) {
        let gap = w[1].wall_ms - w[0].wall_ms;
        if gap < settings.slate_after_ms {
            continue;
        }
        let covering = all_spans
            .iter()
            .find(|(s, _)| s.from_ms >= w[0].wall_ms && s.from_ms <= w[1].wall_ms + RESUME_SPAN_WITHIN_MS);
        let shown = covering.and_then(|(_, after)| frames[*after..].iter().find_map(|f| f.counter));
        resumes.push(Resume {
            gap_from_wall_ms: w[0].wall_ms,
            gap_ms: gap,
            first_emitted_counter: w[1].counter,
            first_shown_counter: shown,
            suppressed_ms: shown
                .map(|s| round((f64::from(s) - f64::from(w[1].counter)) * settings.camera_period_ms)),
        });
    }

    Camera {
        strip_frames: frames.iter().filter(|f| f.counter.is_some()).count() as u64,
        slate_frames,
        freezes,
        slates,
        start_slate,
        end_slates,
        counter_regressions: regressions,
        cadence,
        resumes,
        latency_samples: lat.len() as u64,
        latency_ms: percentiles(lat.into_iter().map(|(_, l)| l).collect()),
        latency_windows_ms: windows,
    }
}

fn overlay(frames: &[Frame], timeline: &Timeline) -> Overlay {
    let mut changes = Vec::new();
    // Each change's exact wall time (the report rounds it; the late count must not).
    let mut change_walls = Vec::new();
    let mut current = 0u16;
    let mut future_dated = 0;
    let mut frames_read = 0;
    for f in frames {
        let Some(i) = f.score else { continue };
        frames_read += 1;
        if timeline.due(i).is_some_and(|d| d as f64 > f.wall) {
            future_dated += 1;
        }
        if i != current {
            changes.push(ScoreChange {
                frame_wall_ms: round(f.wall),
                index: i,
                due_wall_ms: timeline.due(i),
            });
            change_walls.push(f.wall);
            current = i;
        }
    }
    let late_frames_max = changes
        .iter()
        .zip(&change_walls)
        .filter_map(|(c, &shown)| {
            let due = c.due_wall_ms? as f64;
            Some(frames.iter().filter(|f| f.wall >= due && f.wall < shown).count() as u64)
        })
        .max()
        .unwrap_or(0);
    Overlay {
        score_index_changes: changes,
        late_frames_max,
        future_dated,
        frames_read,
    }
}

fn audio(segments: &[SegmentScan], datings: &[Dating], frames: &[Frame]) -> Audio {
    let mut onsets = Vec::new();
    let mut gaps = Vec::new();
    let mut samples = 0;
    for (s, d) in segments.iter().zip(datings) {
        samples += s.audio.samples;
        onsets.extend(s.audio.onsets_ns.iter().map(|&o| d.wall(o)));
        for g in s.audio.silences.iter().chain(&s.audio.holes) {
            gaps.push(AudioGap {
                at_ms: round(d.wall(g.start_ns)),
                ms: round(g.dur_ns as f64 / 1e6),
            });
        }
    }
    gaps.sort_by_key(|g| g.at_ms);
    // Each burst frame's first showing.
    let mut seen = BTreeSet::new();
    let bursts: Vec<f64> = frames
        .iter()
        .filter_map(|f| {
            f.counter
                .filter(|c| c % crate::audio::BURST_EVERY_FRAMES == 0)
                .map(|c| (c, f.wall))
        })
        .filter(|(c, _)| seen.insert(*c))
        .map(|(_, w)| w)
        .collect();
    let mut offsets: Vec<i64> = onsets
        .iter()
        .filter_map(|a| {
            bursts
                .iter()
                .map(|v| a - v)
                .filter(|d| d.abs() <= AV_PAIR_WITHIN_MS)
                .min_by(|x, y| x.abs().total_cmp(&y.abs()))
        })
        .map(|d| round(d - CAMERA_AAC_DELAY_MS))
        .collect();
    offsets.sort_unstable();
    let av = AvOffset {
        p50: percentile(&offsets, 50),
        max_abs: offsets.iter().map(|o| o.abs()).max().unwrap_or(0),
        pairs: offsets.len() as u64,
        camera_codec_delay_ms: (CAMERA_AAC_DELAY_MS * 10.0).round() / 10.0,
    };
    Audio {
        av_offset_ms: av,
        silent_gaps: gaps,
        onsets: onsets.len() as u64,
        samples,
    }
}

fn heartbeats(beats: &[Beat]) -> (Heartbeats, Vec<String>) {
    let mut fields: BTreeMap<String, u64> = BTreeMap::new();
    let mut rungs = BTreeSet::new();
    for b in beats {
        if let Some(o) = b.body.as_object() {
            for k in o.keys() {
                *fields.entry(k.clone()).or_default() += 1;
            }
        }
        if let Some(r) = b.body.get("rung") {
            rungs.insert(r.as_str().map_or_else(|| r.to_string(), str::to_owned));
        }
    }
    let mut received: Vec<i64> = beats.iter().map(|b| b.received_ms).collect();
    received.sort_unstable();
    let max_gap_ms = received.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
    let mut failures: Vec<String> = rungs
        .iter()
        .filter(|r| !RUNGS.contains(&r.as_str()))
        .map(|r| {
            format!(
                "heartbeat rung {r:?} is not one the contract carries ({})",
                RUNGS.join(", ")
            )
        })
        .collect();
    // The guard above checks nothing when no beat carries the field (a renamed or dropped field).
    if !beats.is_empty() && rungs.is_empty() {
        failures.push(format!(
            "no heartbeat carries `rung` ({} beats): the rung guard (Owed 20) has nothing to check",
            beats.len()
        ));
    }
    let hb = Heartbeats {
        count: beats.len() as u64,
        max_gap_ms,
        fields_seen: fields,
        last: beats
            .last()
            .map_or_else(|| Value::Object(Default::default()), |b| b.body.clone()),
        rungs: rungs.into_iter().collect(),
    };
    (hb, failures)
}

fn graphics(beats: &[Beat], crashes_ms: &[i64]) -> Graphics {
    let g = |b: &Beat| b.body.get("graphics").and_then(Value::as_str).map(str::to_owned);
    let mut ready_exits = 0;
    let mut without_crash = 0;
    for w in beats.windows(2) {
        if g(&w[0]).as_deref() == Some("ready") && g(&w[1]).as_deref() != Some("ready") {
            ready_exits += 1;
            let (from, to) = (w[0].received_ms - CRASH_SLACK_MS, w[1].received_ms);
            if !crashes_ms.iter().any(|&c| c > from && c <= to) {
                without_crash += 1;
            }
        }
    }
    Graphics {
        restarts_without_crash: without_crash,
        ready_exits,
        crashes_logged: crashes_ms.len() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const P_NS: u64 = 33_333_333;

    fn timeline() -> Timeline {
        Timeline {
            start_wall_ms: 0,
            delay_ms: 3_100,
            events: vec![(1, 1_010), (2, 4_010)],
        }
    }

    fn seg(start_ms: i64, frames: Vec<FrameRec>) -> SegmentScan {
        SegmentScan {
            file: format!("live/dest/{start_ms}.mp4"),
            start_wall_us: start_ms * 1000,
            frames,
            audio: AudioFacts::default(),
            decode_error: None,
        }
    }

    fn strip_frames(first_counter: u32, n: u32) -> Vec<FrameRec> {
        (0..n)
            .map(|i| FrameRec {
                pts_ns: u64::from(i) * P_NS,
                counter: Some(first_counter + i),
                score: None,
            })
            .collect()
    }

    fn inputs(segments: Vec<SegmentScan>) -> Inputs {
        Inputs {
            segments,
            emit: vec![],
            emit_truncated: false,
            beats: vec![],
            beats_truncated: false,
            timeline: timeline(),
            crashes_ms: vec![],
            end_request_ms: None,
        }
    }

    fn settings() -> Settings {
        Settings::from_thresholds(&domain::config::Thresholds::default())
    }

    fn beat(at: i64, body: Value) -> Beat {
        Beat {
            received_ms: at,
            body,
        }
    }

    #[test]
    fn an_empty_recording_reports_zero_and_still_has_every_section() {
        let r = analyze(&inputs(vec![]), &settings());
        assert_eq!((r.frames, r.duration_ms, r.camera.strip_frames), (0, 0, 0));
        assert!(r.pts_gaps.is_empty() && r.output_outages.is_empty() && r.checker_failures.is_empty());
        assert!(r.segment_overlaps.is_empty());
        assert_eq!(r.camera.latency_ms, Percentiles::default());
        let v = serde_json::to_value(&r).unwrap();
        for k in [
            "frames",
            "duration_ms",
            "fps_mean",
            "pts_gaps",
            "segments",
            "output_outages",
            "segment_overlaps",
            "camera",
            "overlay",
            "audio",
            "heartbeats",
            "graphics",
        ] {
            assert!(v.get(k).is_some(), "the report's {k}");
        }
    }

    #[test]
    fn slates_at_the_edges_are_not_interior_slates() {
        // 5 no-strip frames, 30 strip, 4 no-strip, 30 strip, 6 no-strip: start 5, interior 4, end 6.
        let mut f = Vec::new();
        let mut pts = 0;
        let mut push = |counter: Option<u32>| {
            f.push(FrameRec {
                pts_ns: pts,
                counter,
                score: None,
            });
            pts += P_NS;
        };
        (0..5).for_each(|_| push(None));
        (1..=30).for_each(|c| push(Some(c)));
        (0..4).for_each(|_| push(None));
        (31..=60).for_each(|c| push(Some(c)));
        (0..6).for_each(|_| push(None));
        let mut i = inputs(vec![seg(10_000, f)]);
        let r = analyze(&i, &settings());
        assert_eq!(r.camera.start_slate.as_ref().map(|s| s.frames), Some(5));
        assert_eq!(
            r.camera.slates.iter().map(|s| s.frames).collect::<Vec<_>>(),
            vec![4]
        );
        assert_eq!(r.camera.slate_frames, 4);
        assert_eq!(
            r.camera.end_slates.iter().map(|s| s.frames).collect::<Vec<_>>(),
            vec![6]
        );
        // An end request at or before the interior span's first frame moves it to the end too; one
        // after it does not.
        let interior_from = r.camera.slates[0].from_ms;
        i.end_request_ms = Some(interior_from - 1);
        let r = analyze(&i, &settings());
        assert_eq!((r.camera.slate_frames, r.camera.end_slates.len()), (0, 2));
        i.end_request_ms = Some(interior_from + 1);
        let r = analyze(&i, &settings());
        assert_eq!((r.camera.slate_frames, r.camera.end_slates.len()), (4, 1));
    }

    #[test]
    fn regressions_count_against_the_highest_counter_shown() {
        let mut f = strip_frames(100, 10);
        f[5].counter = Some(50);
        f[6].counter = Some(51);
        let r = analyze(&inputs(vec![seg(0, f)]), &settings());
        assert_eq!(r.camera.counter_regressions, 2, "50 and 51 are both below 104");
    }

    #[test]
    fn the_rung_guard_fails_only_an_unknown_rung() {
        let mut i = inputs(vec![]);
        i.beats = vec![
            beat(0, json!({"rung": "full", "graphics": "ready"})),
            beat(5_000, json!({"rung": "overlay_half_rate", "graphics": "ready"})),
        ];
        let r = analyze(&i, &settings());
        assert!(r.checker_failures.is_empty());
        assert_eq!(r.heartbeats.rungs, vec!["full", "overlay_half_rate"]);
        assert_eq!((r.heartbeats.count, r.heartbeats.max_gap_ms), (2, 5_000));
        assert_eq!(r.heartbeats.fields_seen.get("rung"), Some(&2));
        i.beats.push(beat(10_000, json!({"rung": "fast_preset"})));
        let r = analyze(&i, &settings());
        assert_eq!(r.checker_failures.len(), 1, "{:?}", r.checker_failures);
        assert!(r.checker_failures[0].contains("fast_preset"));
    }

    #[test]
    fn a_ready_exit_counts_as_a_restart_without_crash_only_when_no_crash_was_logged() {
        let mut i = inputs(vec![]);
        i.beats = vec![
            beat(0, json!({"graphics": "ready"})),
            beat(5_000, json!({"graphics": "restarting"})),
            beat(10_000, json!({"graphics": "ready"})),
            beat(15_000, json!({"graphics": "restarting"})),
            beat(20_000, json!({"graphics": "restarting"})),
        ];
        // A crash logged between the third and fourth beats explains the second exit only.
        i.crashes_ms = vec![12_000];
        let r = analyze(&i, &settings());
        assert_eq!(
            (
                r.graphics.ready_exits,
                r.graphics.restarts_without_crash,
                r.graphics.crashes_logged
            ),
            (2, 1, 1)
        );
        i.crashes_ms.clear();
        let r = analyze(&i, &settings());
        assert_eq!(r.graphics.restarts_without_crash, 2);
    }

    #[test]
    fn score_changes_carry_their_due_time_and_late_and_future_frames_count() {
        // Wall 0..: index 0 for 150 frames, then 1 shown at frame 150 (5 000 ms; due 1 010 + 3 100 =
        // 4 110), then 2 at frame 200 (6 667 ms; due 4 010 + 3 100 = 7 110: future-dated until then).
        let f: Vec<FrameRec> = (0..250u32)
            .map(|k| FrameRec {
                pts_ns: u64::from(k) * P_NS,
                counter: Some(k + 1),
                score: Some(match k {
                    0..150 => 0,
                    150..200 => 1,
                    _ => 2,
                }),
            })
            .collect();
        let r = analyze(&inputs(vec![seg(0, f)]), &settings());
        let c = &r.overlay.score_index_changes;
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(
            (c[0].index, c[0].due_wall_ms, c[0].frame_wall_ms),
            (1, Some(4_110), 5_000)
        );
        assert_eq!((c[1].index, c[1].due_wall_ms), (2, Some(7_110)));
        // Frames 124..=149 lie in [4 110, 5 000) (frame 123 is at 4 100): 26 late frames.
        assert_eq!(r.overlay.late_frames_max, 26);
        // Index 2 shows from frame 200 (6 667 ms); those before 7 110 are frames 200..=213 (frame
        // 213 is at 7 100, 214 at 7 133): 14.
        assert_eq!(r.overlay.future_dated, 14);
        assert_eq!(r.overlay.frames_read, 250);
    }

    #[test]
    fn a_wall_gap_between_segments_is_an_outage_and_a_roll_is_not() {
        // Segment 2 starts right after segment 1 (a roll), segment 3 starts 1.5 s after segment 2's end.
        let s1 = seg(0, strip_frames(1, 30));
        let s2 = seg(1_000, strip_frames(31, 30));
        let s3 = seg(2_000 + 1_500, strip_frames(61, 30));
        let r = analyze(&inputs(vec![s3, s1, s2]), &settings());
        assert_eq!(r.output_outages.len(), 1, "{:?}", r.output_outages);
        let o = &r.output_outages[0];
        assert!((o.to_wall_ms - o.from_wall_ms - 1_500).abs() <= 2, "{o:?}");
        assert_eq!(o.kind, "between");
        assert!(r.segment_overlaps.is_empty(), "{:?}", r.segment_overlaps);
        assert_eq!(
            r.segments.iter().map(|s| s.start_wall_ms).collect::<Vec<_>>(),
            vec![0, 1_000, 3_500]
        );
        assert!(r.pts_gaps.is_empty());
        assert_eq!(r.frames, 90);
    }

    #[test]
    fn a_backlogged_segment_is_re_dated_from_the_next_one_and_still_reported() {
        // Segment 1: 30 frames from 0 (ends at 1 000). Segment 2 rolls on at 1 000 but is named
        // 250 ms late for its content, as a connection's backlog makes it: segment 3 is named at
        // 1 750, 250 ms before segment 2's frames end by name and PTS (smoke run 39979756278: 2 s).
        // The strips run on (60 then 61), so the boundary is a roll.
        let s1 = seg(0, strip_frames(1, 30));
        let s2 = seg(1_000, strip_frames(31, 30));
        let s3 = seg(1_750, strip_frames(61, 30));
        let r = analyze(&inputs(vec![s1, s2, s3]), &settings());
        assert_eq!(r.segment_overlaps.len(), 1, "{:?}", r.segment_overlaps);
        let o = &r.segment_overlaps[0];
        assert_eq!(
            (o.file.as_str(), o.next_file.as_str()),
            ("live/dest/1000.mp4", "live/dest/1750.mp4")
        );
        assert!((o.overlap_ms - 250).abs() <= 2, "{o:?}");
        assert_eq!(o.content_gap_ms, Some(0));
        assert!(r.output_outages.is_empty(), "{:?}", r.output_outages);
        // Re-dated: the content's 90 frames span 3 000 ms; the wall time never runs backwards.
        assert_eq!(r.duration_ms, 3_000);
        // The mirror: the same three as rolls only.
        let rolls = analyze(
            &inputs(vec![
                seg(0, strip_frames(1, 30)),
                seg(1_000, strip_frames(31, 30)),
                seg(2_000, strip_frames(61, 30)),
            ]),
            &settings(),
        );
        assert!(rolls.segment_overlaps.is_empty(), "{:?}", rolls.segment_overlaps);
        assert!(rolls.output_outages.is_empty(), "{:?}", rolls.output_outages);
    }

    #[test]
    fn a_re_dated_segment_keeps_its_frames_in_order_and_its_burst_at_its_name() {
        let s2 = seg(1_000, strip_frames(31, 30));
        let d = {
            let mut d = Dating::of(&s2);
            d.anchor = Some((1_750.0 - 1000.0 / 30.0, s2.frames[29].pts_ns));
            d
        };
        let w: Vec<f64> = s2.frames.iter().map(|f| d.wall(f.pts_ns)).collect();
        // The backlog (frames whose real-time slot falls before the name) arrived at the name; the
        // last frame one period before the next segment's start.
        assert_eq!(w[0], 1_000.0);
        assert!((w[29] - (1_750.0 - 1000.0 / 30.0)).abs() < 1e-6, "{}", w[29]);
        assert!(w.windows(2).all(|p| p[1] >= p[0]), "{w:?}");
        let burst = w.iter().filter(|x| **x == 1_000.0).count();
        assert_eq!(
            burst, 8,
            "250 ms of backlog: frames 0..=7 (7 x 33.3 = 233 < 250 < 267)"
        );
    }

    #[test]
    fn an_outage_hidden_under_a_backlog_overlap_is_still_an_outage() {
        // Segment 2 (31..=60) is named at 1 000 with a 500 ms backlog, so its content really ended
        // arriving at 1 500. Then the output was down 300 ms, and segment 3 is named at 1 800. Its
        // first strip is 70: counters 61..=69 (300 ms of camera time) never reached the recording.
        // By name and PTS that is a 200 ms overlap, and no outage at all.
        let s1 = seg(0, strip_frames(1, 30));
        let s2 = seg(1_000, strip_frames(31, 30));
        let s3 = seg(1_800, strip_frames(70, 30));
        let r = analyze(&inputs(vec![s1, s2, s3]), &settings());
        assert_eq!(r.segment_overlaps.len(), 1, "{:?}", r.segment_overlaps);
        assert!((r.segment_overlaps[0].overlap_ms - 200).abs() <= 2);
        assert!(
            r.segment_overlaps[0]
                .content_gap_ms
                .is_some_and(|g| (g - 300).abs() <= 2),
            "{:?}",
            r.segment_overlaps
        );
        assert_eq!(r.output_outages.len(), 1, "{:?}", r.output_outages);
        let o = &r.output_outages[0];
        assert_eq!(o.kind, "between");
        assert!(
            (o.from_wall_ms - 1_500).abs() <= 2 && o.to_wall_ms == 1_800,
            "{o:?}"
        );
    }

    #[test]
    fn an_output_that_starts_late_or_dies_early_is_an_edge_outage() {
        // Live at 0; the first frame arrives at 2 000 (a head outage of 2 s). The 30 frames end at
        // 3 000; the end request is at 5 000 (a tail outage of 2 s).
        let mut i = inputs(vec![seg(2_000, strip_frames(1, 30))]);
        i.beats = vec![
            beat(-500, json!({"programmePhase": "waiting", "rung": "full"})),
            beat(0, json!({"programmePhase": "live", "rung": "full"})),
        ];
        i.end_request_ms = Some(5_000);
        let r = analyze(&i, &settings());
        let kinds: Vec<(&str, i64, i64)> = r
            .output_outages
            .iter()
            .map(|o| (o.kind, o.from_wall_ms, o.to_wall_ms))
            .collect();
        assert_eq!(kinds, vec![("head", 0, 2_000), ("tail", 3_000, 5_000)]);
        // The mirror: frames from before live until past the end request carry no edge outage.
        let mut i = inputs(vec![seg(-1_000, strip_frames(1, 300))]);
        i.beats = vec![beat(0, json!({"programmePhase": "live", "rung": "full"}))];
        i.end_request_ms = Some(5_000);
        assert!(analyze(&i, &settings()).output_outages.is_empty());
    }

    #[test]
    fn the_frame_period_is_each_segments_span_not_its_rounded_step() {
        // RTMP's millisecond PTS: 33 or 34 ms steps. The median step reads 33.0, the span 33.333.
        let f: Vec<FrameRec> = (0..301u64)
            .map(|i| FrameRec {
                pts_ns: (i * 1_000 / 30) * 1_000_000,
                counter: Some(i as u32 + 1),
                score: None,
            })
            .collect();
        let r = analyze(&inputs(vec![seg(0, f)]), &settings());
        assert!(
            (r.inputs.frame_period_ms - 1000.0 / 30.0).abs() < 0.01,
            "{}",
            r.inputs.frame_period_ms
        );
        assert_eq!(r.duration_ms, 10_033);
    }

    #[test]
    fn short_repeats_are_cadence_and_only_a_held_counter_is_a_freeze() {
        // Counters 1..=10 with 5 shown twice (one repeat), 7 skipped (emitted, never shown), and 9
        // held on 4 frames (three repeats: a freeze). 20 is never emitted, so 19 -> 21 skips nothing.
        let counters = [1, 2, 3, 4, 5, 5, 6, 8, 9, 9, 9, 9, 10, 19, 21];
        let f: Vec<FrameRec> = counters
            .iter()
            .enumerate()
            .map(|(i, &c)| FrameRec {
                pts_ns: i as u64 * P_NS,
                counter: Some(c),
                score: None,
            })
            .collect();
        let mut i = inputs(vec![seg(0, f)]);
        i.emit = (1..=21u32)
            .filter(|c| *c != 20)
            .map(|c| Emit {
                counter: c,
                wall_ms: i64::from(c) * 33,
            })
            .collect();
        let r = analyze(&i, &settings());
        assert_eq!(
            r.camera
                .freezes
                .iter()
                .map(|z| (z.counter, z.frames))
                .collect::<Vec<_>>(),
            vec![(9, 3)]
        );
        // 10 -> 19 skips 11..=18 (8 counters); 6 -> 8 skips 7.
        assert_eq!(
            r.camera.cadence,
            Cadence {
                pairs: 14,
                repeated_frames: 4,
                skipped_counters: 9,
                skip_events: 2,
            }
        );
    }

    #[test]
    fn beats_with_no_rung_fail_the_rung_guard() {
        let mut i = inputs(vec![]);
        i.beats = vec![
            beat(0, json!({"graphics": "ready"})),
            beat(5_000, json!({"graphics": "ready"})),
        ];
        let r = analyze(&i, &settings());
        assert_eq!(r.checker_failures.len(), 1, "{:?}", r.checker_failures);
        assert!(r.checker_failures[0].contains("no heartbeat carries `rung`"));
        // No beats at all: nothing to guard, and `heartbeats.count` (0) fails the case instead.
        assert!(analyze(&inputs(vec![]), &settings()).checker_failures.is_empty());
    }

    #[test]
    fn a_pts_step_of_two_frames_is_a_gap() {
        let mut f = strip_frames(1, 20);
        for x in f.iter_mut().skip(10) {
            x.pts_ns += 2 * P_NS;
        }
        let r = analyze(&inputs(vec![seg(0, f)]), &settings());
        assert_eq!(r.pts_gaps.len(), 1, "{:?}", r.pts_gaps);
        assert_eq!(r.pts_gaps[0].gap_ms, 100);
    }

    #[test]
    fn a_resume_names_the_first_counters_emitted_and_shown_after_a_long_gap() {
        // The camera emits 1..=30, pauses 4 s (counters 31..=150 never emitted), resumes at 151.
        // The recording: 1..=30, a 20-frame slate, then 160 onward (frames 151..=159 suppressed).
        let mut i = inputs(vec![]);
        i.emit = (1..=30)
            .map(|c| Emit {
                counter: c,
                wall_ms: i64::from(c) * 33,
            })
            .chain((151..=200).map(|c| Emit {
                counter: c,
                wall_ms: 5_000 + i64::from(c) * 33,
            }))
            .collect();
        let mut f = Vec::new();
        let mut pts = 0;
        for c in (1..=30)
            .map(Some)
            .chain((0..20).map(|_| None))
            .chain((160..=200).map(Some))
        {
            f.push(FrameRec {
                pts_ns: pts,
                counter: c,
                score: None,
            });
            pts += P_NS;
        }
        i.segments = vec![seg(500, f)];
        let r = analyze(&i, &settings());
        assert_eq!(r.camera.resumes.len(), 1, "{:?}", r.camera.resumes);
        let res = &r.camera.resumes[0];
        assert_eq!(
            (res.first_emitted_counter, res.first_shown_counter),
            (151, Some(160))
        );
        assert_eq!(res.suppressed_ms, Some(300), "9 frames at 30 fps");
        // A slate that starts too long after the camera resumed does not cover the gap: the same
        // recording with the slate moved 20 s later (strips meanwhile) names no shown counter.
        let mut late = Vec::new();
        let mut pts = 0;
        for c in (1..=30)
            .map(Some)
            .chain((160..=760).map(Some))
            .chain((0..20).map(|_| None))
            .chain((761..=800).map(Some))
        {
            late.push(FrameRec {
                pts_ns: pts,
                counter: c,
                score: None,
            });
            pts += P_NS;
        }
        let mut j = i.clone();
        j.segments = vec![seg(500, late)];
        let r = analyze(&j, &settings());
        assert_eq!(r.camera.resumes.len(), 1);
        assert_eq!(
            (
                r.camera.resumes[0].first_shown_counter,
                r.camera.resumes[0].suppressed_ms
            ),
            (None, None),
            "{:?}",
            r.camera.resumes
        );
        // A shorter gap than slate_after_ms has no resume.
        i.emit = (1..=30)
            .map(|c| Emit {
                counter: c,
                wall_ms: i64::from(c) * 33,
            })
            .collect();
        assert!(analyze(&i, &settings()).camera.resumes.is_empty());
    }

    #[test]
    fn latency_is_measured_on_first_showings_against_the_emit_log() {
        // Every counter shown twice (a repeat), emitted 700 ms before its first showing.
        let mut f = Vec::new();
        for k in 0..60u32 {
            f.push(FrameRec {
                pts_ns: u64::from(k) * P_NS,
                counter: Some(k / 2 + 1),
                score: None,
            });
        }
        let mut i = inputs(vec![seg(10_000, f)]);
        let shown = |c: u32| (f64::from(c - 1) * 2.0 * 1000.0 / 30.0).round() as i64;
        i.emit = (1..=30u32)
            .map(|c| Emit {
                counter: c,
                wall_ms: 10_000 - 700 + shown(c),
            })
            .collect();
        let r = analyze(&i, &settings());
        assert_eq!(r.camera.latency_samples, 30);
        assert!(
            (r.camera.latency_ms.p50 - 700).abs() <= 2,
            "{:?}",
            r.camera.latency_ms
        );
        // Each counter repeated once: cadence, not a freeze.
        assert!(r.camera.freezes.is_empty(), "{:?}", r.camera.freezes);
        assert_eq!(
            (r.camera.cadence.pairs, r.camera.cadence.repeated_frames),
            (59, 30)
        );
        assert_eq!(r.camera.latency_windows_ms.len(), 1);
    }
}
