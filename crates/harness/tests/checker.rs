//! The checker on recorded segments it did not make (C13 brief, Tests: `checker.rs`): two
//! fragmented MP4 segments written by mp4mux (as MediaMTX records), decoded by the checker's own
//! decode, analysed, and every expected value taken from the fixture's construction below, never
//! from the checker.
//!
//! The fixture, at 30 fps from T0 = 2026-10-23 12:00:00 UTC:
//! - segment 1 (90 frames, 3.0 s): counters 11..=40; counter 41 on 21 frames (a 20-frame freeze);
//!   10 mid-grey slate frames; counters 72..=100;
//! - a 1.5 s outage;
//! - segment 2 (60 frames) from T0 + 4.5 s: counters 101..=160.
//!
//! The camera emitted each counter 700 ms before its first showing; the audio is the camera's tone
//! by frame counter, so its bursts fall on counters 30, 90, 120 and 150. The burst on 60 falls in
//! the freeze, where the picture still shows 41, so it pairs with nothing.
//!
//! The counters start at 11, not 1, so that no burst sits near the end of a segment. Each fixture
//! segment is its own AAC encode, and an AAC encode loses its tail: CI run 39979756301 decoded
//! 238 592 of 240 000 samples. A first version started at 1, which put bursts on the last frames
//! (90 and 150), and neither burst survived.
mod common;

use std::path::{Path, PathBuf};

use common::SegFrame;
use harness::camera::Layout;
use harness::checker::{analyze, Inputs, Report, Settings};
use harness::inputs::{Beat, Emit, Timeline};
use harness::media::scan_recordings;
use serde_json::json;
use time::macros::{datetime, format_description};

const PERIOD_MS: f64 = 1000.0 / 30.0;
const LATENCY_MS: i64 = 700;

/// The epoch ms of T0 + `ms`, and a MediaMTX segment name for it, from the `time` crate (a clock
/// independent of the checker's own `clock::segment_start_us`).
fn at(ms: i64) -> (i64, String) {
    let t = datetime!(2026-10-23 12:00:00 UTC) + time::Duration::milliseconds(ms);
    let name = t
        .format(format_description!(
            "[year]-[month]-[day]_[hour]-[minute]-[second]-[subsecond digits:6].mp4"
        ))
        .unwrap();
    ((t.unix_timestamp_nanos() / 1_000_000) as i64, name)
}

/// Segment 1's first counter (the module comment: why not 1).
const FIRST: u32 = 11;
/// The counter the freeze holds: frame 30's, shown on frames 30..=50.
const FROZEN: u32 = FIRST + 30;

fn segment_1() -> Vec<SegFrame> {
    (0..90u32)
        .map(|i| SegFrame {
            strip: match i {
                0..30 => Some(i + FIRST),
                30..51 => Some(FROZEN),
                51..61 => None,
                _ => Some(i + FIRST),
            },
            audio_counter: i + FIRST,
        })
        .collect()
}

fn segment_2(swap: bool) -> Vec<SegFrame> {
    let mut f: Vec<SegFrame> = (0..60u32)
        .map(|j| SegFrame {
            strip: Some(FIRST + 90 + j),
            audio_counter: FIRST + 90 + j,
        })
        .collect();
    if swap {
        // Counter 111 shown before 110: one regression.
        f.swap(9, 10);
    }
    f
}

/// Each counter's emit time: its first showing in the fixture minus `LATENCY_MS`.
fn emit_log(seg1_start: i64, seg2_start: i64) -> Vec<Emit> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (start, frames) in [(seg1_start, segment_1()), (seg2_start, segment_2(false))] {
        for (i, f) in frames.iter().enumerate() {
            if let Some(c) = f.strip {
                if seen.insert(c) {
                    let shown = start as f64 + i as f64 * PERIOD_MS;
                    out.push(Emit {
                        counter: c,
                        wall_ms: shown.round() as i64 - LATENCY_MS,
                    });
                }
            }
        }
    }
    out
}

fn beats(t0: i64, rungs: &[&str]) -> Vec<Beat> {
    rungs
        .iter()
        .enumerate()
        .map(|(i, r)| Beat {
            received_ms: t0 + 1_000 * i as i64,
            body: json!({"state": "playing", "programmePhase": "live", "graphics": "ready", "rung": r}),
        })
        .collect()
}

fn write(dir: &Path, swap: bool) -> (i64, i64) {
    let l = Layout::i420(1280, 720);
    let (t1, n1) = at(0);
    let (t2, n2) = at(4_500);
    let rec = dir.join("live").join("dest");
    std::fs::create_dir_all(&rec).unwrap();
    common::write_mp4(&rec.join(n1), &l, &segment_1());
    common::write_mp4(&rec.join(n2), &l, &segment_2(swap));
    (t1, t2)
}

fn run(name: &str, swap: bool, rungs: &[&str]) -> Report {
    common::init();
    let dir: PathBuf = std::env::temp_dir().join(format!("harness-checker-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (t1, t2) = write(&dir, swap);
    let segments = scan_recordings(&dir).unwrap();
    let inputs = Inputs {
        segments,
        emit: emit_log(t1, t2),
        emit_truncated: false,
        beats: beats(t1, rungs),
        beats_truncated: false,
        timeline: Timeline {
            start_wall_ms: t1,
            delay_ms: 3_100,
            events: vec![],
        },
        crashes_ms: vec![],
        end_request_ms: None,
    };
    let r = analyze(
        &inputs,
        &Settings::from_thresholds(&domain::config::Thresholds::default()),
    );
    let _ = std::fs::remove_dir_all(&dir);
    r
}

#[test]
fn two_recorded_segments_give_the_fixtures_outage_freeze_slate_and_latency() {
    let r = run("main", false, &["full", "full", "overlay_half_rate", "full"]);
    eprintln!(
        "segments read: {}, frames decoded: {}, strip frames: {}",
        r.inputs.segments_read, r.frames, r.camera.strip_frames
    );
    assert_eq!(r.inputs.segments_read, 2);
    assert!(r.inputs.decode_errors.is_empty(), "{:?}", r.inputs.decode_errors);
    assert_eq!(r.frames, 150, "every recorded frame decoded");
    assert_eq!(r.camera.strip_frames, 140);

    assert_eq!(r.output_outages.len(), 1, "{:?}", r.output_outages);
    let o = &r.output_outages[0];
    let gap = o.to_wall_ms - o.from_wall_ms;
    assert!(
        (gap - 1_500).abs() <= 100,
        "the fixture's 1.5 s outage, read as {gap} ms"
    );
    assert!(r.pts_gaps.is_empty(), "{:?}", r.pts_gaps);

    assert_eq!(r.camera.freezes.len(), 1, "{:?}", r.camera.freezes);
    assert_eq!(
        (r.camera.freezes[0].frames, r.camera.freezes[0].counter),
        (20, FROZEN)
    );
    assert_eq!(
        r.camera.slates.iter().map(|s| s.frames).collect::<Vec<_>>(),
        vec![10]
    );
    assert_eq!(
        (
            r.camera.slate_frames,
            r.camera.start_slate.as_ref(),
            r.camera.end_slates.len()
        ),
        (10, None, 0)
    );
    assert_eq!(r.camera.counter_regressions, 0);

    // 120 counters first shown (11..=41, 72..=160), each 700 ms after its emit.
    assert_eq!(r.camera.latency_samples, 120);
    assert!(
        (r.camera.latency_ms.p50 - LATENCY_MS).abs() <= 2,
        "{:?}",
        r.camera.latency_ms
    );
    assert!(
        (r.camera.latency_ms.max - LATENCY_MS).abs() <= 2,
        "{:?}",
        r.camera.latency_ms
    );

    // Bursts on counters 30, 90, 120 and 150 pair; the freeze's (60) does not.
    assert_eq!(r.audio.onsets, 5, "{:?}", r.audio);
    assert_eq!(r.audio.av_offset_ms.pairs, 4, "{:?}", r.audio);
    assert!(r.audio.av_offset_ms.max_abs <= 50, "{:?}", r.audio.av_offset_ms);
    assert!(r.audio.silent_gaps.is_empty(), "{:?}", r.audio.silent_gaps);

    assert_eq!(
        r.heartbeats.rungs,
        vec!["full".to_owned(), "overlay_half_rate".to_owned()]
    );
    assert!(r.checker_failures.is_empty(), "{:?}", r.checker_failures);
}

#[test]
fn one_frame_out_of_order_is_one_regression() {
    let r = run("swap", true, &["full"]);
    assert_eq!(r.frames, 150);
    assert_eq!(r.camera.counter_regressions, 1);
}

#[test]
fn a_rung_the_contract_does_not_carry_fails_the_checker() {
    let r = run("rung", false, &["full", "quarter_rate"]);
    assert_eq!(r.checker_failures.len(), 1, "{:?}", r.checker_failures);
    assert!(
        r.checker_failures[0].contains("quarter_rate"),
        "{:?}",
        r.checker_failures
    );
}

#[test]
fn a_file_that_is_not_a_segment_name_is_refused() {
    common::init();
    let dir = std::env::temp_dir().join(format!("harness-checker-{}-name", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("recording.mp4"), b"").unwrap();
    let e = scan_recordings(&dir).unwrap_err();
    assert!(e.contains("recording.mp4"), "{e}");
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(
        scan_recordings(&empty).unwrap().is_empty(),
        "an empty directory reads as no segments"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
