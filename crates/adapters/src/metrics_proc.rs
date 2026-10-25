//! The heartbeat's measured values (C12): the process tree's CPU and memory from `/proc`, and C09's
//! output counters from the programme's `Stats`.
//!
//! | Field | Source |
//! |---|---|
//! | `cpu_pct` | `utime + stime` of this process and every descendant (`/proc/<pid>/stat`), the delta since the last sample ÷ the wall delta ÷ the CPUs this process may use × 100 |
//! | `mem_mb` | the sum of `VmRSS` over the same tree (`/proc/<pid>/status`) |
//! | `fps` | the programme's REAL output rate (Owed 86): the video frames that left the encoder since the last sample, over the wall time since it (`BeatRate`, fed `Stats::snapshot()`'s `encoded_frames`), never the profile's nominal rate; absent until a window has both ends |
//! | `bitrate_kbps`, `egress_bytes`, `encoder_speed` | `Stats::snapshot()`, unchanged: a `None` there stays absent on the wire |
//! | `camera_frames_replaced` | `Stats::snapshot()`, unchanged; logged by the driver, never on the wire (Owed 90) |
//! | `memory()` (not a `Measured` field) | the last sample's `/proc/<pid>/status` read of this process (`VmRSS`, `RssAnon`, `RssFile`, `RssShmem`, `Threads`: the read `mem_mb` already makes) and the CEF child poll's last PSS by kind (`with_cef`); logged by the driver once a minute, never on the wire (the soak's memory split) |
//! | `measured_latency_ms` | never: it needs SEI (F-003), so it is omitted, never 0 |
//!
//! The driver samples once a beat (`Effect::SendHeartbeat`, the only caller), so "since the last
//! sample" is the beat window. The first sample has no CPU delta yet, so its `cpu_pct` is `None`. A process is keyed by its pid
//! AND its start time, so a pid reused between two samples is a new process, never a delta against
//! the old one. A descendant that started since the last sample counts all of its ticks (it ran only
//! since then); one that ended in between is lost, which only ever under-reads. A CEF process
//! orphaned to pid 1 (F-013) has left the tree and is not counted. Off Linux there is no `/proc`, and
//! `cpu_pct`, `mem_mb` and `memory` stay `None`.
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use app::ports::{CefPoll, Measured, MemorySplit, Metrics, ProcStatus};

use crate::stats::{BeatRate, Stats};

/// A process's identity across samples: its pid and its start time (clock ticks since boot).
type ProcKey = (u32, u64);

/// The fields of `/proc/<pid>/stat` this sampler reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    pub ppid: u32,
    /// `utime + stime`, in clock ticks.
    pub cpu_ticks: u64,
    pub start_ticks: u64,
}

/// Parses a `/proc/<pid>/stat` line. The command name (field 2) is parenthesised and may hold spaces
/// and parentheses itself, so the fields are read after the LAST `)`: there, field 3 (`state`) is
/// index 0, so field n is index n − 3 (`ppid` 4, `utime` 14, `stime` 15, `starttime` 22; proc(5)).
pub fn parse_proc_stat(line: &str) -> Option<ProcStat> {
    let tail = &line[line.rfind(')')? + 1..];
    let f: Vec<&str> = tail.split_whitespace().collect();
    let num = |n: usize| f.get(n - 3)?.parse::<u64>().ok();
    Some(ProcStat {
        ppid: u32::try_from(num(4)?).ok()?,
        cpu_ticks: num(14)?.checked_add(num(15)?)?,
        start_ticks: num(22)?,
    })
}

/// The `VmRSS` of a `/proc/<pid>/status` text, in kB (`VmRSS:  1234 kB`). A kernel thread has none.
pub fn parse_vm_rss_kb(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let mut it = line["VmRSS:".len()..].split_whitespace();
    let kb = it.next()?.parse::<u64>().ok()?;
    (it.next() == Some("kB")).then_some(kb)
}

/// A `<name>  <n>[ <unit>]` line of a `/proc/<pid>/status` text: `n`, when the line is there, its
/// number reads and its unit is `unit` (`None`: no unit).
fn status_field(status: &str, name: &str, unit: Option<&str>) -> Option<u64> {
    let line = status
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))?;
    let mut it = line.split_whitespace();
    let n = it.next()?.parse::<u64>().ok()?;
    (it.next() == unit && it.next().is_none()).then_some(n)
}

/// The memory split's fields of a `/proc/<pid>/status` text (proc(5)): `VmRSS`, `RssAnon`, `RssFile`
/// and `RssShmem` in kB, and `Threads`. A missing or malformed line leaves its field absent, never 0.
pub fn parse_proc_status(status: &str) -> ProcStatus {
    let kb = |name| status_field(status, name, Some("kB"));
    ProcStatus {
        vm_rss_kb: kb("VmRSS"),
        rss_anon_kb: kb("RssAnon"),
        rss_file_kb: kb("RssFile"),
        rss_shmem_kb: kb("RssShmem"),
        threads: status_field(status, "Threads", None),
    }
}

/// This process and every descendant, each with its `stat` fields, read from `/proc`. Empty off
/// Linux (no `/proc`).
fn process_tree(root: u32) -> BTreeMap<u32, ProcStat> {
    let mut all = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return all;
    };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if let Some(s) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .as_deref()
            .and_then(parse_proc_stat)
        {
            all.insert(pid, s);
        }
    }
    let mut tree = BTreeMap::new();
    let Some(root_stat) = all.get(&root).copied() else {
        return tree;
    };
    tree.insert(root, root_stat);
    // Parents before children is not guaranteed by pid order, so widen until nothing is added.
    loop {
        let before = tree.len();
        for (pid, s) in &all {
            if !tree.contains_key(pid) && tree.contains_key(&s.ppid) {
                tree.insert(*pid, *s);
            }
        }
        if tree.len() == before {
            return tree;
        }
    }
}

/// The live `Metrics`: `/proc` for the process tree and the programme's `Stats` for the output.
pub struct ProcMetrics {
    stats: Arc<Stats>,
    root: u32,
    ticks_per_s: f64,
    cpus: f64,
    last: Option<(Instant, BTreeMap<ProcKey, u64>)>,
    /// The beat's real output rate (Owed 86).
    rate: BeatRate,
    /// The CEF child poll's last PSS by kind, for the memory split (`with_cef`; none with no CEF).
    cef: Option<CefReader>,
    /// This process's own status fields, from the last sample's `status` read (the memory split).
    own: Option<ProcStatus>,
}

/// Reads the CEF child poll's last PSS by kind (`CefGraphics::memory`).
type CefReader = Box<dyn Fn() -> Option<CefPoll> + Send>;

impl fmt::Debug for ProcMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcMetrics")
            .field("root", &self.root)
            .field("ticks_per_s", &self.ticks_per_s)
            .field("cpus", &self.cpus)
            .field("sampled", &self.last.is_some())
            .field("rate", &self.rate)
            .field("cef", &self.cef.is_some())
            .finish_non_exhaustive()
    }
}

impl ProcMetrics {
    /// `stats` is the programme's (`GstProgramme::stats`), shared, so the beat reads C09's counters.
    pub fn new(stats: Arc<Stats>) -> Self {
        Self {
            stats,
            root: std::process::id(),
            ticks_per_s: clock_ticks_per_second(),
            cpus: std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64),
            last: None,
            rate: BeatRate::default(),
            cef: None,
            own: None,
        }
    }

    /// The memory split's CEF part: `read` gives the child poll's last PSS by kind, once a beat.
    pub fn with_cef(mut self, read: impl Fn() -> Option<CefPoll> + Send + 'static) -> Self {
        self.cef = Some(Box::new(read));
        self
    }
}

#[cfg(target_os = "linux")]
fn clock_ticks_per_second() -> f64 {
    rustix::param::clock_ticks_per_second() as f64
}

/// No `/proc` to read ticks from: the value is never used.
#[cfg(not(target_os = "linux"))]
fn clock_ticks_per_second() -> f64 {
    100.0
}

impl Metrics for ProcMetrics {
    fn sample(&mut self) -> Measured {
        let now = Instant::now();
        // The counters first, read at `now`, so the `/proc` walk's time (one read per process of
        // the tree) never lands inside the rate's window (the PR #69 review's m3).
        let s = self.stats.snapshot();
        let fps = self.rate.sample(now, s.encoded_frames);
        let tree = process_tree(self.root);
        let ticks: BTreeMap<ProcKey, u64> = tree
            .iter()
            .map(|(pid, s)| ((*pid, s.start_ticks), s.cpu_ticks))
            .collect();
        let cpu_pct = match &self.last {
            Some((at, before)) if !ticks.is_empty() => {
                let wall_s = now.duration_since(*at).as_secs_f64();
                let delta: u64 = ticks
                    .iter()
                    .map(|(k, t)| t.saturating_sub(before.get(k).copied().unwrap_or(0)))
                    .sum();
                (wall_s > 0.0).then(|| delta as f64 / self.ticks_per_s / wall_s / self.cpus * 100.0)
            }
            _ => None,
        };
        // One `status` read per process: the tree's VmRSS sum, and this process's own split.
        let mut own: Option<ProcStatus> = None;
        let mem_kb: Option<u64> = (!tree.is_empty()).then(|| {
            tree.keys()
                .filter_map(|pid| {
                    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
                    if *pid == self.root {
                        own = Some(parse_proc_status(&text));
                    }
                    parse_vm_rss_kb(&text)
                })
                .sum()
        });
        self.own = own;
        self.last = (!ticks.is_empty()).then_some((now, ticks));
        Measured {
            fps,
            bitrate_kbps: s.bitrate_kbps,
            egress_bytes: s.egress_bytes,
            measured_latency_ms: None,
            encoder_speed: s.encoder_speed,
            cpu_pct,
            mem_mb: mem_kb.map(|kb| kb / 1024),
            camera_frames_replaced: s.camera_frames_replaced,
        }
    }

    fn memory(&mut self) -> Option<MemorySplit> {
        Some(MemorySplit {
            status: self.own?,
            cef: self.cef.as_ref().and_then(|read| read()),
        })
    }
}
