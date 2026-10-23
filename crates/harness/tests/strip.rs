//! The camera strip through a real encoder and decoder (C13 brief, Tests: `strip.rs`). The pure
//! round trips are `harness::strip`'s unit tests; these run the pictures through x264 at the
//! camera's settings and avdec_h264, as the compositor's input does, and the camera's own pipeline
//! through the checker's own decode.
mod common;

use common::{slate, x264_round_trip, Rng};
use harness::camera::{self, Layout, Pattern};
use harness::media::{scan_segment, CameraPipeline, CameraSink};

/// The brief's 1 000 random counters, each drawn on the ball picture; then the snow subset; then
/// slates, which must read as no strip.
#[test]
fn random_counters_read_back_through_x264_and_avdec_h264_and_slates_read_none() {
    common::init();
    let l = Layout::i420(1280, 720);
    let mut rng = Rng(0x05EA_2C13);
    let mut expected: Vec<Option<u32>> = Vec::new();
    let mut pictures = Vec::new();
    for i in 0..1_100 {
        let counter = rng.next();
        let pattern = if i < 1_000 { Pattern::Ball } else { Pattern::Snow };
        pictures.push(camera::frame(&l, pattern, counter));
        expected.push(Some(counter));
    }
    for luma in [16u8, 128, 235, 70, 200] {
        for _ in 0..6 {
            pictures.push(slate(&l, luma));
            expected.push(None);
        }
    }
    let read = x264_round_trip(&l, None, pictures);
    assert_eq!(read.len(), expected.len(), "every frame decodes");
    let mut checked = 0;
    for (i, (index, got)) in read.iter().enumerate() {
        assert_eq!(*index, i as u64, "frame {i} decoded in order");
        assert_eq!(*got, expected[i], "frame {i}");
        checked += 1;
    }
    eprintln!("strips checked: {checked} (1000 ball, 100 snow, 30 slates)");
    assert_eq!(
        checked, 1_130,
        "zero checked is a failure, and so is any short count"
    );
}

/// A 1080p camera's strip, scaled to the 720p programme as the compositor scales it, still reads
/// (`strip::HEIGHT_PER_CELL`: the cell scales with the picture).
#[test]
fn a_1080p_strip_reads_after_scaling_to_720p() {
    common::init();
    let l = Layout::i420(1920, 1080);
    let mut rng = Rng(1080);
    let counters: Vec<u32> = (0..60).map(|_| rng.next()).collect();
    let pictures = counters
        .iter()
        .map(|&c| camera::frame(&l, Pattern::Ball, c))
        .collect();
    let read = x264_round_trip(&l, Some((1280, 720)), pictures);
    let got: Vec<Option<u32>> = read.iter().map(|(_, c)| *c).collect();
    let want: Vec<Option<u32>> = counters.into_iter().map(Some).collect();
    eprintln!("scaled strips checked: {}", got.len());
    assert_eq!(got, want);
}

/// The camera's own pipeline (`CameraPipeline`, here into an MPEG-TS file instead of SRT) read by
/// the checker's own decode (`scan_segment`): the producer and the consumer of the strip and the
/// tone, with nothing standing in for either.
#[test]
fn the_cameras_pipeline_round_trips_through_the_checkers_decode() {
    common::init();
    let dir = std::env::temp_dir().join(format!("harness-strip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ts = dir.join("camera.ts");
    let mut cam = CameraPipeline::start(&CameraSink::File(ts.clone()), 1280, 720, Pattern::Ball).unwrap();
    // 310 frames: ten whole tone bursts (frames 30, 60, … 300; `audio::BURST_EVERY_FRAMES`).
    for c in 1..=310u32 {
        cam.push(c).unwrap();
    }
    cam.finish(60).unwrap();
    let s = scan_segment(&ts, "camera.ts", 0);
    assert_eq!(s.decode_error, None);
    let counters: Vec<Option<u32>> = s.frames.iter().map(|f| f.counter).collect();
    let want: Vec<Option<u32>> = (1..=310).map(Some).collect();
    eprintln!(
        "camera frames checked: {}; tone onsets: {}",
        counters.len(),
        s.audio.onsets_ns.len()
    );
    assert_eq!(counters, want, "every frame, in order, with its counter");
    // Each onset within 50 ms of its burst frame's PTS (AAC's priming and frame size), and ten of
    // them: the fixture pushed ten bursts.
    let first = s.frames[0].pts_ns;
    let burst_pts: Vec<u64> = s
        .frames
        .iter()
        .filter(|f| f.counter.is_some_and(|c| c % 30 == 0))
        .map(|f| f.pts_ns)
        .collect();
    assert_eq!(burst_pts.len(), 10);
    assert_eq!(s.audio.onsets_ns.len(), 10, "{:?}", s.audio.onsets_ns);
    for (o, v) in s.audio.onsets_ns.iter().zip(&burst_pts) {
        let d = *o as i64 - *v as i64;
        assert!(
            d.abs() <= 50_000_000,
            "onset {o} vs burst frame {v} (first frame {first}): {d} ns"
        );
    }
    assert!(
        s.audio.silences.is_empty() && s.audio.holes.is_empty(),
        "{:?}",
        s.audio
    );
    let _ = std::fs::remove_dir_all(&dir);
}
