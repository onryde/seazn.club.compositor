//! `harness/in-container.sh` (C14), the controls that run as root inside the compositor's container,
//! against a fake `/proc` (`IN_CONTAINER_PROC`) whose pid directories are this test's own `sleep`
//! children, so each kill lands on a real process and nothing else.
//!
//! The command lines are the two shapes the product's own reader takes
//! (`adapters::cef_graphics::split_cmdline`, pinned by its test
//! `a_chromium_command_line_splits_on_nul_and_on_spaces`): arguments split on NUL, and Chromium's
//! renamed children, whose `/proc/<pid>/cmdline` is ONE space-joined string padded with NULs (the
//! PR #52 review's C1: a NUL-only split found no `--type=` in CI, 3 of 3 `kill-cef` and 6 of 6
//! `kill-zygote` runs, so cases 8, 13, 14 and 15 never applied their fault). The rule is mirrored
//! in the script, never imported: the script runs in the relay image, which has no harness.
//!
//! Every fake /proc also holds a VANISHED pid, listed first: a directory whose files are gone, as
//! the script's own command-substitution subshell is by the time its pid list is walked. Fix round 2:
//! CI's `main: line 57: /proc/175/cmdline: No such file or directory`, and under `set -e` bash 5.2's
//! `$(<file)` on a missing file ends the script silently even inside `|| return 1`, so `tree` stopped
//! at the first vanished pid and `kill-zygote` printed nothing at all (6 of 6 legs).
//!
//! Needs bash 5 (`EPOCHREALTIME`, `mapfile -d`): HARNESS_BASH, as in case_verdicts.rs.
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bash() -> String {
    let bash = std::env::var("HARNESS_BASH").unwrap_or_else(|_| "bash".into());
    let out = Command::new(&bash)
        .args(["-c", "echo ${BASH_VERSINFO[0]}"])
        .output()
        .expect("bash runs");
    let major: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    assert!(
        major >= 5,
        "in-container.sh needs bash 5; {bash} is {major} (set HARNESS_BASH)"
    );
    bash
}

/// A pid directory with nothing in it: a process gone between the listing and the read.
const VANISHED: &str = "1";

/// The boot time the fake `/proc/stat` declares, in s.
const BTIME_S: i64 = 1_792_800_000;

/// A renamed renderer, as Chromium's setproctitle leaves it: one space-joined string, NUL-padded to
/// the original argv's length (the flags are CEF 139's renderer flags; the values are made up).
const RENDERER_RENAMED: &[u8] = b"/usr/lib/x86_64-linux-gnu/gstreamer-1.0/gstcefsubprocess --type=renderer \
--no-sandbox --disable-gpu-compositing --lang=en-US --log-severity=disable --num-raster-threads=4 \
--enable-main-frame-before-activation --renderer-client-id=7 --time-ticks-at-unix-epoch=-1761264000123456 \
--launch-time-ticks=4215678901 --shared-files=v8_context_snapshot_data:100 \
--field-trial-handle=3,i,8473920157623481,1204871234987123,262144 --variations-seed-version\0\0\0\0\0\0\0\0";
/// A renamed GPU process and a renamed zygote, the same shape.
const GPU_RENAMED: &[u8] = b"/usr/lib/x86_64-linux-gnu/gstreamer-1.0/gstcefsubprocess --type=gpu-process \
--no-sandbox --disable-gpu-compositing --lang=en-US --log-severity=disable \
--gpu-preferences=UAAAAAAAAAAgAAAEAAAAAAAAAAAAAAAAAABgAAEAAAAAAAAAAAAAAAAAAAACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
--shared-files --field-trial-handle=3,i,8473920157623481,1204871234987123,262144\0\0\0\0";
const ZYGOTE_RENAMED: &[u8] =
    b"/usr/lib/x86_64-linux-gnu/gstreamer-1.0/gstcefsubprocess --type=zygote --no-sandbox --lang=en-US\0\0\0";
/// A zygote and a utility process that were never renamed: one argument per NUL.
const ZYGOTE_ARGV: &[u8] =
    b"/usr/lib/x86_64-linux-gnu/gstreamer-1.0/gstcefsubprocess\0--type=zygote\0--no-sandbox\0";
const UTILITY_ARGV: &[u8] =
    b"/usr/lib/x86_64-linux-gnu/gstreamer-1.0/gstcefsubprocess\0--type=utility\0--utility-sub-type=network.mojom.NetworkService\0";
/// Not CEF: no argument starts with `--type=` (the product's `flag_value` is a prefix match).
const PYTHON_ARGV: &[u8] = b"/usr/bin/python3\0x.py\0see--type=renderer\0--typed=1\0";
const COMPOSITOR_ARGV: &[u8] = b"/usr/local/bin/compositor\0";

/// One fake process: a real `sleep` (its pid names the directory) and what /proc says of it.
struct Proc {
    child: Child,
    pid: u32,
}

/// A fake /proc in a directory of its own, and the processes behind it.
struct FakeProc {
    dir: PathBuf,
    procs: Vec<Proc>,
}

impl FakeProc {
    fn new(tag: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("in-container")
            .join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stat"), format!("cpu  1 2 3 4\nbtime {BTIME_S}\n")).unwrap();
        // The vanished pid: "1" sorts before every real pid in the script's glob.
        std::fs::create_dir_all(dir.join(VANISHED)).unwrap();
        Self { dir, procs: vec![] }
    }

    /// A process with `comm`, `cmdline` and parent `ppid`, started `start_ticks` after boot.
    fn add(&mut self, comm: &str, cmdline: &[u8], ppid: u32, start_ticks: u64) -> u32 {
        let child = Command::new("sleep").arg("120").spawn().expect("sleep");
        let pid = child.id();
        let d = self.dir.join(pid.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(d.join("cmdline"), cmdline).unwrap();
        // proc(5): pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt
        // utime stime cutime cstime priority nice num_threads itrealvalue starttime ...
        std::fs::write(
            d.join("stat"),
            format!(
                "{pid} ({comm}) S {ppid} 1 1 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 {start_ticks} 1000 10\n"
            ),
        )
        .unwrap();
        self.procs.push(Proc { child, pid });
        pid
    }

    /// Removes a process's directory (it is gone from /proc; the sleep is killed and reaped).
    fn remove(&mut self, pid: u32) {
        std::fs::remove_dir_all(self.dir.join(pid.to_string())).unwrap();
    }

    fn run(&self, args: &[&str]) -> (Output, Vec<Value>) {
        let out = Command::new(bash())
            .arg(repo().join("harness/in-container.sh"))
            .args(args)
            .env("IN_CONTAINER_PROC", &self.dir)
            .output()
            .expect("in-container.sh runs");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        eprintln!(
            "--- in-container.sh {args:?}: {:?}\n{stdout}{stderr}",
            out.status.code()
        );
        assert!(
            !stderr.contains("No such file"),
            "a vanished pid is skipped quietly: {stderr}"
        );
        let lines = stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not one JSON object: {l} ({e})")))
            .collect();
        (out, lines)
    }

    /// Whether the sleep behind `pid` was SIGKILLed (reaps it then), within a second.
    fn was_killed(&mut self, pid: u32) -> bool {
        let p = self.procs.iter_mut().find(|p| p.pid == pid).expect("our pid");
        let until = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(st) = p.child.try_wait().unwrap() {
                return st.signal() == Some(9);
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for FakeProc {
    fn drop(&mut self) {
        // Only this test's own children.
        for p in &mut self.procs {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }
}

/// The compositor's tree in the shapes CI met: a renamed zygote parenting a renamed GPU process, a
/// never-renamed zygote parenting a renamed renderer, a utility process, and a non-CEF process.
struct Tree {
    p: FakeProc,
    compositor: u32,
    gpu_zygote: u32,
    gpu: u32,
    renderer_zygote: u32,
    renderer: u32,
    utility: u32,
    python: u32,
}

fn tree(tag: &str) -> Tree {
    let mut p = FakeProc::new(tag);
    let compositor = p.add("compositor", COMPOSITOR_ARGV, 1, 1_000);
    let gpu_zygote = p.add("gstcefsubproces", ZYGOTE_RENAMED, compositor, 1_100);
    let gpu = p.add("gstcefsubproces", GPU_RENAMED, gpu_zygote, 1_200);
    let renderer_zygote = p.add("gstcefsubproces", ZYGOTE_ARGV, compositor, 1_150);
    let renderer = p.add("gstcefsubproces", RENDERER_RENAMED, renderer_zygote, 1_300);
    let utility = p.add("gstcefsubproces", UTILITY_ARGV, compositor, 1_250);
    let python = p.add("python3", PYTHON_ARGV, 1, 900);
    Tree {
        p,
        compositor,
        gpu_zygote,
        gpu,
        renderer_zygote,
        renderer,
        utility,
        python,
    }
}

fn pids(v: &Value) -> Vec<u32> {
    let mut p: Vec<u32> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap() as u32)
        .collect();
    p.sort_unstable();
    p
}

#[test]
fn the_tree_reads_each_process_type_from_both_cmdline_shapes() {
    let t = tree("tree");
    let (out, lines) = t.p.run(&["tree"]);
    assert!(out.status.success());
    let clk: i64 = String::from_utf8_lossy(&Command::new("getconf").arg("CLK_TCK").output().unwrap().stdout)
        .trim()
        .parse()
        .unwrap();
    let want = [
        (t.compositor, None, 1_000),
        (t.gpu_zygote, Some("zygote"), 1_100),
        (t.gpu, Some("gpu-process"), 1_200),
        (t.renderer_zygote, Some("zygote"), 1_150),
        (t.renderer, Some("renderer"), 1_300),
        (t.utility, Some("utility"), 1_250),
        (t.python, None, 900),
    ];
    let mut checked = 0;
    for (pid, ty, ticks) in want {
        let l = lines
            .iter()
            .find(|l| l["pid"].as_u64() == Some(u64::from(pid)))
            .unwrap_or_else(|| panic!("pid {pid} is not listed"));
        assert_eq!(l["type"].as_str(), ty, "pid {pid}");
        assert_eq!(
            l["start_ms"].as_i64(),
            Some(BTIME_S * 1000 + ticks * 1000 / clk),
            "pid {pid}"
        );
        assert!(
            l["at_ms"].as_i64().is_some_and(|a| a > BTIME_S * 1000),
            "pid {pid}"
        );
        checked += 1;
    }
    eprintln!("processes checked: {checked} of {} listed", lines.len());
    assert_eq!(checked, 7);
    assert_eq!(lines.len(), 7, "one line per process, nothing else");
}

#[test]
fn kill_cef_kills_every_cef_child_and_only_them() {
    let mut t = tree("kill-cef");
    let (out, lines) = t.p.run(&["kill-cef"]);
    assert!(out.status.success());
    // The targets are printed BEFORE the kill (the container may die under the exec: PR #52's m2),
    // and the kill's own status after.
    assert_eq!(lines.len(), 2, "{lines:?}");
    let cef = {
        let mut v = vec![t.gpu_zygote, t.gpu, t.renderer_zygote, t.renderer, t.utility];
        v.sort_unstable();
        v
    };
    assert_eq!(pids(&lines[0]["killed"]), cef);
    assert!(lines[0]["at_ms"].as_i64().is_some());
    let mut types: Vec<&str> = lines[0]["types"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    types.sort_unstable();
    assert_eq!(types, ["gpu-process", "renderer", "utility", "zygote", "zygote"]);
    assert_eq!(lines[1]["rc"].as_i64(), Some(0));
    assert!(lines[1]["rc_at_ms"].as_i64() >= lines[0]["at_ms"].as_i64());
    let mut killed = 0;
    for pid in cef {
        assert!(t.p.was_killed(pid), "CEF child {pid} survived");
        killed += 1;
    }
    for pid in [t.compositor, t.python] {
        assert!(!t.p.was_killed(pid), "{pid} is not a CEF child, and was killed");
    }
    eprintln!("CEF children killed: {killed}; others spared: 2");
    assert_eq!(killed, 5);
}

#[test]
fn kill_cef_that_finds_no_cef_child_says_so_and_fails() {
    let mut p = FakeProc::new("kill-cef-none");
    let compositor = p.add("compositor", COMPOSITOR_ARGV, 1, 1_000);
    let python = p.add("python3", PYTHON_ARGV, 1, 900);
    let (out, lines) = p.run(&["kill-cef"]);
    assert!(!out.status.success(), "a kill with no target must fail");
    assert_eq!(lines[0]["killed"].as_array().map(Vec::len), Some(0));
    assert!(!p.was_killed(compositor) && !p.was_killed(python));
}

#[test]
fn kill_zygote_picks_the_zygote_that_parents_the_wanted_child() {
    let mut checked = 0;
    for (want, child_type) in [("gpu", "gpu-process"), ("renderer", "renderer")] {
        let mut t = tree(&format!("kill-zygote-{want}"));
        let (zygote, other) = if want == "gpu" {
            (t.gpu_zygote, t.renderer_zygote)
        } else {
            (t.renderer_zygote, t.gpu_zygote)
        };
        let (out, lines) = t.p.run(&["kill-zygote", want]);
        assert!(out.status.success(), "{want}");
        assert_eq!(lines.len(), 2, "{want}: {lines:?}");
        let l = &lines[0];
        assert_eq!(l["pid"].as_u64(), Some(u64::from(zygote)), "{want}");
        assert_eq!(l["type"].as_str(), Some("zygote"), "{want}");
        assert_eq!(l["ppid"].as_u64(), Some(u64::from(t.compositor)), "{want}");
        assert_eq!(l["compositor"].as_u64(), Some(u64::from(t.compositor)), "{want}");
        assert_eq!(l["child_types"], serde_json::json!([child_type]), "{want}");
        assert_eq!(lines[1]["rc"].as_i64(), Some(0), "{want}");
        assert!(t.p.was_killed(zygote), "{want}: the zygote survived");
        assert!(!t.p.was_killed(other), "{want}: the other zygote was killed");
        checked += 1;
    }
    eprintln!("zygote kinds checked: {checked}");
    assert_eq!(checked, 2);
}

#[test]
fn kill_zygote_with_no_such_zygote_fails_with_a_null_pid() {
    let mut p = FakeProc::new("kill-zygote-none");
    let compositor = p.add("compositor", COMPOSITOR_ARGV, 1, 1_000);
    p.add("gstcefsubproces", ZYGOTE_ARGV, compositor, 1_100);
    let (out, lines) = p.run(&["kill-zygote", "gpu"]);
    assert!(!out.status.success());
    assert!(lines[0]["pid"].is_null(), "{lines:?}");
}

#[test]
fn gpu_loop_stops_at_its_deadline_and_fails() {
    let mut t = tree("gpu-loop-deadline");
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    };
    let deadline = now() + 1_500;
    let started = Instant::now();
    // Two kills wanted, one GPU process ever: the second never comes (CI's C1 shape, and any other
    // reason a new pid never appears), so the loop must stop at the deadline, not at the relay's.
    let (out, lines) = t.p.run(&["gpu-loop", "2", "0", &deadline.to_string()]);
    let took = started.elapsed();
    assert!(!out.status.success(), "a loop that timed out must fail");
    assert!(took < Duration::from_secs(5), "the loop ran {took:?}");
    let kills: Vec<&Value> = lines.iter().filter(|l| l.get("at_ms").is_some()).collect();
    assert_eq!(kills.len(), 1, "{lines:?}");
    assert_eq!(kills[0]["pid"].as_u64(), Some(u64::from(t.gpu)));
    let done = lines.last().unwrap();
    assert_eq!(done["timed_out"], Value::Bool(true));
    assert_eq!(done["done"].as_u64(), Some(1));
    assert!(done["done_at_ms"].as_i64().is_some_and(|d| d >= deadline));
    assert!(t.p.was_killed(t.gpu));
}

#[test]
fn gpu_loop_ends_when_the_compositor_is_gone() {
    let mut t = tree("gpu-loop-gone");
    let c = t.compositor;
    t.p.remove(c);
    let far = i64::MAX / 2;
    let (out, lines) = t.p.run(&["gpu-loop", "3", "0", &far.to_string()]);
    // No compositor process at all: the loop has nothing to watch and ends at once, unsuccessful.
    assert!(!out.status.success());
    let done = lines.last().unwrap();
    assert_eq!(done["compositor_alive"], Value::Bool(false));
    assert_eq!(done["timed_out"], Value::Bool(false));
    assert!(!t.p.was_killed(t.gpu));
}

#[test]
fn burn_runs_its_workers_for_its_time_and_stops() {
    let p = FakeProc::new("burn");
    let started = Instant::now();
    let (out, lines) = p.run(&["burn", "3", "1"]);
    let took = started.elapsed();
    assert!(out.status.success());
    assert!(took < Duration::from_secs(6), "the burn ran {took:?}");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["workers"].as_u64(), Some(3));
    assert_eq!(lines[0]["seconds"].as_u64(), Some(1));
    assert!(lines[0]["at_ms"].as_i64().is_some());
    assert_eq!(lines[1]["workers"].as_u64(), Some(3));
    assert!(
        lines[1]["elapsed_ms"].as_i64().is_some_and(|e| e >= 1_000),
        "{lines:?}"
    );
    assert!(lines[1]["done_at_ms"].as_i64().is_some());
}

#[test]
fn an_unknown_command_is_refused() {
    let p = FakeProc::new("usage");
    let (out, _) = p.run(&["kill-everything"]);
    assert_eq!(out.status.code(), Some(64));
}
