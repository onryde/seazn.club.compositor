//! The heartbeat's `/proc` sampler and its join with C09's counters (C12 Step 2). The `/proc` tests
//! need Linux; CI runs Linux, and `check.yml` refuses a run with 0 passed, so the gate is not vacuous
//! there. `copies_every_stats_field_into_measured` runs everywhere.
use std::sync::Arc;

use adapters::metrics_proc::{parse_proc_stat, parse_vm_rss_kb, ProcMetrics};
use adapters::stats::{Stats, StatsSnapshot};
use app::ports::Metrics;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn first_sample_has_no_cpu() {
        let mut m = ProcMetrics::new(Arc::new(Stats::default()));
        assert_eq!(m.sample().cpu_pct, None, "no delta yet");
        let second = m.sample().cpu_pct;
        assert!(second.is_some(), "the second sample has a delta: {second:?}");
    }

    #[test]
    fn busy_loop_raises_cpu() {
        let mut m = ProcMetrics::new(Arc::new(Stats::default()));
        m.sample();
        let spin = std::thread::spawn(|| {
            let until = Instant::now() + Duration::from_millis(300);
            let mut x: u64 = 0;
            while Instant::now() < until {
                x = std::hint::black_box(x.wrapping_add(1));
            }
            x
        });
        spin.join().unwrap();
        let cpu = m.sample().cpu_pct.expect("a delta");
        eprintln!("cpu over a 300 ms spin: {cpu:.2}%");
        assert!(cpu > 0.0, "{cpu}");
        assert!(cpu.is_finite());
    }

    #[test]
    fn mem_is_positive() {
        let mut m = ProcMetrics::new(Arc::new(Stats::default()));
        let mb = m.sample().mem_mb.expect("VmRSS read");
        eprintln!("tree VmRSS: {mb} MB");
        assert!(mb > 0, "{mb}");
    }

    #[test]
    fn latency_is_never_reported() {
        let mut m = ProcMetrics::new(Arc::new(Stats::default()));
        let mut checked = 0;
        for _ in 0..3 {
            assert_eq!(
                m.sample().measured_latency_ms,
                None,
                "needs SEI (F-003): omitted, never 0"
            );
            checked += 1;
        }
        assert_eq!(checked, 3);
    }

    /// A child process is part of the tree: its CPU counts toward the sample, so a busy child raises
    /// `cpu_pct` while this process sleeps (the CEF children are what this field is for).
    #[test]
    fn a_busy_child_process_is_counted() {
        let mut m = ProcMetrics::new(Arc::new(Stats::default()));
        m.sample();
        let t = Instant::now();
        let mut child = std::process::Command::new("sh")
            // Builtins only: a forked helper per turn would spend the time in processes that are
            // gone before the sample.
            .args(["-c", "i=0; while [ $i -lt 1000000000 ]; do i=$((i+1)); done"])
            .spawn()
            .expect("sh");
        std::thread::sleep(Duration::from_millis(800));
        let cpu = m.sample().cpu_pct.expect("a delta");
        child.kill().ok();
        child.wait().ok();
        eprintln!("cpu with a busy child over {:?}: {cpu:.2}%", t.elapsed());
        assert!(cpu > 1.0, "the child's ticks were not counted: {cpu}");
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn proc_tests_need_linux() {
    eprintln!("skipped: not linux");
}

/// Every `Stats` field but `encoded_frames` reaches `Measured` unchanged; `encoded_frames` is read
/// through `BeatRate` into `fps` (Owed 86), and one sample is no window, so the first `fps` is absent.
#[test]
fn copies_every_stats_field_into_measured() {
    let stats = Arc::new(Stats::default());
    let set = StatsSnapshot {
        encoded_frames: Some(9_000),
        bitrate_kbps: Some(3_987),
        egress_bytes: Some(123_456_789),
        encoder_speed: Some(0.97),
        camera_frames_replaced: Some(41),
    };
    stats.force_snapshot(set);
    let got = ProcMetrics::new(Arc::clone(&stats)).sample();
    assert_eq!(got.fps, None, "one sample opens the window and reads nothing");
    assert_eq!(got.bitrate_kbps, set.bitrate_kbps);
    assert_eq!(got.egress_bytes, set.egress_bytes);
    assert_eq!(got.encoder_speed, set.encoder_speed);
    assert_eq!(got.camera_frames_replaced, set.camera_frames_replaced);
    let fresh = ProcMetrics::new(Arc::new(Stats::default())).sample();
    assert_eq!(
        (
            fresh.fps,
            fresh.bitrate_kbps,
            fresh.egress_bytes,
            fresh.encoder_speed,
            fresh.camera_frames_replaced
        ),
        (None, None, None, None, None),
        "a field C09 reports as None stays absent"
    );
}

/// Owed 86, the seam from the counter to the beat: frames counted by a real `Stats` (the call the
/// programme's encoder sink makes for every video frame) come out of the real sampler as the beat's
/// `fps`, the frames since the last sample over the wall time since it. The window's length is
/// bracketed by the test's own clock reads around each sample, so the expected rate is the test's
/// frame count over that bracket, never the sampler's arithmetic. Then the empty window: no frame
/// for a whole window is a measured 0, the starved beat the alarm is for; then the rate again.
#[test]
fn the_beats_fps_is_the_frames_out_of_the_encoder_over_the_beat_window() {
    use std::time::{Duration, Instant};
    let stats = Arc::new(Stats::default());
    let mut m = ProcMetrics::new(Arc::clone(&stats));
    assert_eq!(m.sample().fps, None, "nothing encoded");
    let mut pts = Duration::ZERO;
    let mut encode = |n: u32| {
        for _ in 0..n {
            stats.on_encoded_video(pts);
            pts += Duration::from_millis(33);
        }
    };
    encode(1);
    let sampled = |m: &mut ProcMetrics| {
        let before = Instant::now();
        let fps = m.sample().fps;
        (before, fps, Instant::now())
    };
    let (mut before, fps, mut after) = sampled(&mut m);
    assert_eq!(fps, None, "the first total opens the window");
    // A window comfortably past the 1 s minimum (OUTPUT_FPS_MIN_WINDOW_MS) on a loaded runner.
    let wait = Duration::from_millis(1_300);
    let mut checked = 0;
    for frames in [40u32, 0, 39] {
        encode(frames);
        std::thread::sleep(wait);
        let (b, fps, a) = sampled(&mut m);
        let fps = fps.expect("a window of at least the minimum");
        let (shortest, longest) = ((b - after).as_secs_f64(), (a - before).as_secs_f64());
        let (lo, hi) = (f64::from(frames) / longest, f64::from(frames) / shortest);
        eprintln!("{frames} frames over {shortest:.3}..{longest:.3} s read {fps:.3} fps");
        assert!(
            (lo..=hi).contains(&fps),
            "{frames} frames: {fps} outside {lo}..{hi}"
        );
        if frames == 0 {
            assert_eq!(fps, 0.0, "an empty window is a measured 0");
        }
        (before, after) = (b, a);
        checked += 1;
    }
    assert_eq!(checked, 3);
}

/// proc(5)'s layout, with a command name that holds spaces and a `)` (fields read after the last).
#[test]
fn proc_stat_fields_are_read_after_the_last_paren() {
    // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt utime stime
    // cutime cstime priority nice num_threads itrealvalue starttime ...
    let line =
        "4242 (cef (gpu) x) S 17 4242 4242 0 -1 4194560 100 0 0 0 750 250 9 9 30 10 12 0 98765 1000 50";
    let s = parse_proc_stat(line).expect("parses");
    assert_eq!(s.ppid, 17);
    assert_eq!(s.cpu_ticks, 750 + 250, "utime + stime, never cutime/cstime");
    assert_eq!(s.start_ticks, 98_765);
    assert_eq!(parse_proc_stat("4242 (x"), None);
    assert_eq!(parse_proc_stat("4242 (x) S 17"), None);
    assert_eq!(
        parse_vm_rss_kb("Name:\tx\nVmRSS:\t  51200 kB\nVmSwap:\t0 kB\n"),
        Some(51_200)
    );
    assert_eq!(
        parse_vm_rss_kb("Name:\tkthreadd\n"),
        None,
        "a kernel thread has none"
    );
}
