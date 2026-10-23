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

#[test]
fn copies_every_stats_field_into_measured() {
    let stats = Arc::new(Stats::default());
    let set = StatsSnapshot {
        fps: Some(29.5),
        bitrate_kbps: Some(3_987),
        egress_bytes: Some(123_456_789),
        encoder_speed: Some(0.97),
    };
    stats.force_snapshot(set);
    let got = ProcMetrics::new(Arc::clone(&stats)).sample();
    assert_eq!(got.fps, set.fps);
    assert_eq!(got.bitrate_kbps, set.bitrate_kbps);
    assert_eq!(got.egress_bytes, set.egress_bytes);
    assert_eq!(got.encoder_speed, set.encoder_speed);
    let fresh = ProcMetrics::new(Arc::new(Stats::default())).sample();
    assert_eq!(
        (
            fresh.fps,
            fresh.bitrate_kbps,
            fresh.egress_bytes,
            fresh.encoder_speed
        ),
        (None, None, None, None),
        "a field C09 reports as None stays absent"
    );
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
