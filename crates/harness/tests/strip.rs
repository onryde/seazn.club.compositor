//! The camera strip through a real encoder and decoder (C13 brief, Tests: `strip.rs`). The pure
//! round trips are `harness::strip`'s unit tests; these run the pictures through x264 at the
//! camera's settings and avdec_h264, as the compositor's input does, and the camera's own pipeline
//! through the checker's own decode.
mod common;

use common::{explain, slate, x264_round_trip, Rng};
use harness::camera::{self, Layout, Pattern};
use harness::checker::CAMERA_AAC_DELAY_MS;
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
    for (i, d) in read.iter().enumerate() {
        assert_eq!(d.index, i as u64, "frame {i} decoded in order");
        assert_eq!(
            d.strip,
            expected[i],
            "frame {i} ({}):\n{}",
            if (1_000..1_100).contains(&i) {
                "snow"
            } else if i < 1_000 {
                "ball"
            } else {
                "slate"
            },
            explain(expected[i], &d.cells)
        );
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
    for (d, want) in read.iter().zip(&counters) {
        assert_eq!(
            d.strip,
            Some(*want),
            "frame {}:\n{}",
            d.index,
            explain(Some(*want), &d.cells)
        );
    }
    let got: Vec<Option<u32>> = read.iter().map(|d| d.strip).collect();
    let want: Vec<Option<u32>> = counters.into_iter().map(Some).collect();
    eprintln!("scaled strips checked: {}", got.len());
    assert_eq!(got, want);
}

/// The camera's own pipeline (`CameraPipeline`, here into an MPEG-TS file instead of SRT) read by
/// the checker's own decode (`scan_segment`): the producer and the consumer of the strip and the
/// tone, with nothing standing in for either. Ball and snow: snow's every keyframe (frames 1, 31, …)
/// is where per-pixel noise lost the strip (`camera::SNOW_BLOCK`).
#[test]
fn the_cameras_pipeline_round_trips_through_the_checkers_decode() {
    camera_round_trip(Pattern::Ball);
}

#[test]
fn the_cameras_snow_keeps_its_strip_through_the_checkers_decode() {
    camera_round_trip(Pattern::Snow);
}

fn camera_round_trip(pattern: Pattern) {
    common::init();
    let dir = std::env::temp_dir().join(format!("harness-strip-{}-{pattern:?}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ts = dir.join("camera.ts");
    let mut cam = CameraPipeline::start(&CameraSink::File(ts.clone()), 1280, 720, pattern).unwrap();
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
        "camera frames checked ({pattern:?}): {}; tone onsets: {}",
        counters.len(),
        s.audio.onsets_ns.len()
    );
    assert_eq!(
        counters, want,
        "{pattern:?}: every frame, in order, with its counter"
    );
    // Ten onsets (the fixture pushed ten bursts), each late on its burst frame's PTS by the camera's
    // AAC delay (`checker::CAMERA_AAC_DELAY_MS`, measured on the checker fixture's MP4) within 5 ms:
    // a second measurement of that constant, through the camera's real MPEG-TS path.
    let first = s.frames[0].pts_ns;
    let burst_pts: Vec<u64> = s
        .frames
        .iter()
        .filter(|f| f.counter.is_some_and(|c| c % 30 == 0))
        .map(|f| f.pts_ns)
        .collect();
    assert_eq!(burst_pts.len(), 10);
    assert_eq!(s.audio.onsets_ns.len(), 10, "{:?}", s.audio.onsets_ns);
    let late_ms: Vec<f64> = s
        .audio
        .onsets_ns
        .iter()
        .zip(&burst_pts)
        .map(|(o, v)| (*o as f64 - *v as f64) / 1e6)
        .collect();
    eprintln!("camera onsets late by (ms): {late_ms:?}");
    for d in &late_ms {
        assert!(
            (d - CAMERA_AAC_DELAY_MS).abs() <= 5.0,
            "an onset {d:.2} ms after its burst frame (first frame {first}); the camera's AAC delay is {CAMERA_AAC_DELAY_MS:.2} ms"
        );
    }
    assert!(
        s.audio.silences.is_empty() && s.audio.holes.is_empty(),
        "{:?}",
        s.audio
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Owed 81's probe at the camera's sink (`CameraPipeline::on_sent` read by `harness::wire`): every
/// frame the camera pushes is one video PES start in what the sink takes, in order, one camera
/// period apart in PTS (90 kHz: 3 000 ticks a frame, give or take the mux's rounding of
/// 33 333 333 ns), and the audio's PES are there too. Then the same with the audio off (the
/// control's `--audio off`): video PES only, and the checker still reads every strip.
#[test]
fn the_wire_probe_sees_every_frame_the_camera_sends_with_and_without_audio() {
    use std::sync::{Arc, Mutex};

    use harness::wire::{self, Stream};

    common::init();
    const FRAMES: u32 = 90;
    for audio in [true, false] {
        let dir = std::env::temp_dir().join(format!("harness-wire-{}-{audio}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ts = dir.join("camera.ts");
        let mut cam =
            CameraPipeline::start_with(&CameraSink::File(ts.clone()), 1280, 720, Pattern::Ball, audio)
                .unwrap();
        let seen: Arc<Mutex<Vec<wire::Scan>>> = Arc::default();
        let s2 = Arc::clone(&seen);
        cam.on_sent(Arc::new(move |b: &[u8]| s2.lock().unwrap().push(wire::scan(b))))
            .unwrap();
        for c in 1..=FRAMES {
            cam.push(c).unwrap();
        }
        cam.finish(60).unwrap();
        let scans = seen.lock().unwrap().clone();
        let (bad, partial): (u64, usize) = scans
            .iter()
            .fold((0, 0), |(b, p), s| (b + s.bad_sync, p + s.partial_bytes));
        let starts: Vec<_> = scans.iter().flat_map(|s| s.starts.clone()).collect();
        let video: Vec<u64> = starts
            .iter()
            .filter(|p| p.stream == Stream::Video)
            .map(|p| p.pts90k.expect("every video PES carries a PTS"))
            .collect();
        let audio_starts = starts.iter().filter(|p| p.stream == Stream::Audio).count();
        eprintln!(
            "wire (audio {audio}): {} buffers, {} video PES, {audio_starts} audio PES",
            scans.len(),
            video.len()
        );
        assert_eq!((bad, partial), (0, 0), "whole, well-synced packets");
        assert_eq!(video.len(), FRAMES as usize, "one video PES per pushed frame");
        for w in video.windows(2) {
            assert!(
                (2_999..=3_001).contains(&(w[1] - w[0])),
                "video PTS step {} (90 kHz)",
                w[1] - w[0]
            );
        }
        if audio {
            // Each push of 1 600 samples completes at most two AAC frames of 1 024, and the mux
            // packs one or more AAC frames into each audio PES (plus the encoder's flush at EOS).
            assert!(
                (1..=2 * FRAMES as usize + 1).contains(&audio_starts),
                "{audio_starts} audio PES"
            );
        } else {
            assert_eq!(audio_starts, 0, "no audio track, no audio PES");
        }
        let s = scan_segment(&ts, "camera.ts", 0);
        let counters: Vec<Option<u32>> = s.frames.iter().map(|f| f.counter).collect();
        assert_eq!(
            counters,
            (1..=FRAMES).map(Some).collect::<Vec<_>>(),
            "audio {audio}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
