//! The graphics pipeline (C11): the overlay page in CEF, through gstcefsrc (F-001's element), rebuilt
//! on every `restart(gen, url)`:
//!
//! ```text
//! cefsrc url=<overlayUrl> listen-for-js-signals=false max-video-framerate=<fps>/1
//!        → video/x-raw,format=BGRA,width=1920,height=1080 → appsink(max-buffers=1 drop=true sync=false)
//!        → straight alpha (cefsrc's is premultiplied, F-001) → FrameStore::set_overlay, behind the gate
//! ```
//!
//! No framerate in the caps: cefsrc fixates `0/1` and emits only on paint (F-001). The adapter never
//! starts Xvfb (C12 runs `Xvfb :99`) and has no load timeout (the domain's `cef_load_timeout_ms`).
//!
//! **Readiness is F-001's console line**, never the first frame: the page logs `seazn:ready` with
//! `console.info`, and cefsrc logs every console message on the `cefconsole` debug category as
//! `<source>:<line> <message>`. The same channel carries R2005's `seazn:alive`,
//! `seazn:score-stale=true|false` and `seazn:phase-channel=loopback|web`. Each is matched exactly; any
//! other `seazn:` text is logged at WARN and mapped to nothing (`page_signal`).
//!
//! **Events.** `CefReady`, `CefAlive` and `CefCrashed` carry the generation of the pipeline whose page
//! or process they come from, a torn-down one included: the domain ignores a generation that is not
//! its current one. `ScoreStale` and `PhaseChannel` carry none, so only the current build's lines are
//! relayed; a retired page's are logged and dropped. `CefCrashed` is sent once a build, from the first
//! of: a bus error, cefsrc's "Render subprocess terminated" warning (its `OnRenderProcessTerminated`,
//! which then reloads the page), or the memory cap. A dead GPU process is no crash: CEF recovers it
//! and the page keeps painting (F-013), but each is counted (`health()`): the 6th of the process's
//! life aborts it.
//!
//! **The gate** (Owed 17; the 2026-10-18 amendment): one field, the build whose samples may reach the
//! overlay slot. Only that build's own `seazn:ready` opens it, and a build reported crashed never
//! reopens it (its page reloads and signals ready again). It closes, before the call returns or the
//! event is sent, on `restart` (for the old build), on `stop`, on the drop, and when a crash is
//! decided. A sample from a closed build is dropped and counted. The slot is never cleared here: the
//! last frame stays held (`hide_overlay` is the programme's).
//!
//! **Never blocks the caller.** Each build has one thread, which starts it (CEF initialises inside the
//! first build's NULL→READY), waits for its retirement and tears it down. `restart`, `stop` and the
//! drop only close the gate, set the stop flag and start a reaper watch: when the teardown has not
//! returned within `REAPER_KILL_AFTER_MS`, the watch sends SIGKILL to the build's renderer processes
//! and logs their pids at WARN; if it has still not returned `TEARDOWN_LEAK_AFTER_MS` later, it is
//! counted leaked (`health()`, Owed 55). Nothing here joins a thread. Page lines arrive on CEF's own
//! UI thread, so the log handler only decides what closes a gate and queues the rest (`NoteQueue`:
//! bounded, and never dropping a crash or a ready); this adapter's dispatcher thread relays them.
//!
//! **Health** (owner ruling 2026-10-19 "B"): `health()` (the port's) reports what the domain's
//! events cannot say: either of CEF's zygotes gone, which ends CEF for the process (Owed 56: the
//! renderers' zygote's death fails the next build at once; the GPU process's zygote's death turns
//! the next GPU-process death into Chromium's FATAL abort of the whole process, the PR #32 confirm
//! pass's N1), the leaked teardowns, and the GPU-process deaths (Owed 61: Chromium aborts the
//! process at the 6th). It only counts: the domain decides the in-place restart (C11b).
//!
//! **The log handler** (F-013). To receive console lines this adapter raises the `cefconsole`
//! threshold to INFO (and `cefsrc`'s to WARNING), never lowering one; GStreamer's default handler
//! would then print every console line, page URL and token included. So the first `CefGraphics`
//! replaces it for the process: its handler routes this adapter's elements' lines privately, drops
//! every other `cefconsole` line, and prints the rest to stderr in the default handler's layout,
//! scrubbed of every secret any instance knows (`redact` too). `GST_DEBUG_FILE` is not honoured.
//!
//! **Secrets.** The URL carries the job token (`st=`). It is set as a property (never through a
//! launch string, which `GST_DEBUG=5` prints) and logged as its origin alone; every text this adapter
//! logs passes the scrubber, which knows the URL and its `st` value. `log-severity=disable` keeps CEF
//! from writing each console message with its source URL to `chrome_debug.log` (F-013); a
//! `GST_CEF_LOG_SEVERITY` in the environment overrides it, and is warned about. CEF's profile
//! (History, Top Sites, the HTTP cache) holds every URL it loaded, token included: it lives in the
//! process's private directory (`cef-cache-location`, mode 0700, under `std::env::temp_dir()`),
//! which the last adapter's drop removes (PR #32 review I1; C12 puts that directory on tmpfs).
//!
//! **Guardrails** (spec §6), every `CHILD_POLL_MS` on every CEF child process (a descendant whose
//! command line carries Chromium's `--type=`): nice `CEF_NICE` on each of its threads (Linux nice is
//! per thread; the zygote's carries over to the renderers it forks), `oom_score_adj` by kind (the
//! renderer first, the zygote last: `oom_score_adj_for`), and the PSS sum against the cap, counting
//! no other build's renderer. Between two polls the same thread runs the GPU watch every
//! `GPU_WATCH_MS` (Owed 70 (a)): one `stat` read per known GPU process, and a listing only while no
//! GPU process is known, paced on its CPU so a death's wait uses at most `GPU_WAIT_SHARE_PCT` of a
//! core, and never further apart than `GPU_LISTING_GAP_MAX_MS` in a wait, so a GPU process that lives
//! under one poll is still counted (F-026: a crash loop's 5 deaths in 288 to 337 ms read as 1 or 2 at
//! the 1 s poll), down to `GPU_RELAUNCH_LIFE_MIN_MS`.
//!
//! **Renderer attribution** (F-013). The reaper kills one build's renderers only. A renderer's
//! `--renderer-client-id` survives CEF's reload of a crashed page, so each client id is claimed by
//! the newest build when it first appears (and once more when a build is retired). Chromium keeps a
//! spare renderer for the next browser, which would be claimed by the build before the one that uses
//! it, so every element asks for `SPARE_RENDERER_OFF`: one renderer per browser.
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use app::ports::{redacted_origin, CefPoll, CefPss, GraphicsRenderer, Inbound};
use crossbeam_channel::Sender;
use domain::health::PhaseChannel;
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::frame_store::FrameStore;
use crate::gst_output::LogFn;
use crate::gst_programme::OVERLAY_SIZE;
use crate::redact::Scrubber;

/// F-001: a js-signal page's `set_state(NULL)` did not return within 10 s. A retired build whose
/// teardown has not returned after this long has its renderer processes killed (amendment
/// 2026-10-18).
pub const REAPER_KILL_AFTER_MS: u64 = 3_000;
/// The brief's 1 s poll of CEF's child processes.
pub const CHILD_POLL_MS: u64 = 1_000;
/// Owed 70 (a): the GPU watch's step between two child polls. Each step checks that the GPU
/// processes already listed are still alive, and while no GPU process is known (after CEF's start,
/// or after a death) it lists CEF's children, paced by each listing's CPU (`GPU_WAIT_SHARE_PCT`,
/// `GPU_LISTING_GAP_MAX_MS`).
/// F-026 (C14 case 15's fast loop; 9 legs of runs 40051995233, 40056474269 and
/// 40056388445): Chromium relaunched each killed GPU process and the loop killed it again 69 to 88 ms
/// later, so 5 kills took 288 to 337 ms and the 1 s poll counted 1 or 2. The loop kills a pid only
/// after a whole walk of `/proc` has listed it (`harness/in-container.sh`, C14's PR #52), so each of those GPU
/// processes lived at least one walk: an estimated 10 to 19 ms, two or more steps (F-026 says how
/// the estimate is made; the walk itself was not timed).
pub const GPU_WATCH_MS: u64 = 5;
/// Owed 70 (a): how long, on the wall clock, the watch waits for a GPU process at
/// `GPU_WAIT_SHARE_PCT` after a GPU-process death, and after CEF's start (the watch's first step).
/// F-013's probe GR gave each relaunch 2 s, and every relaunch came within it. After it, while a
/// death's relaunch is still unseen, the watch lists at `GPU_LATE_SHARE_PCT`.
pub const GPU_RELAUNCH_WAIT_MS: u64 = 2_000;
/// Owed 70 (a): the share of one core a wait's listings may use (the coordinator's budget,
/// 2026-10-24, on the PR #55 review's I2). A listing walks every process's `stat` and CEF's
/// children's command lines: 3 075 us over 169 processes on CI's runner (check run 40077609130, an
/// unoptimised build, no CEF child), so the first build's 400 listings, one per step, took about
/// 3.2 s and 38% of a core per death. Each listing now delays the next by `100 / GPU_WAIT_SHARE_PCT`
/// times its cost in the thread's CPU (`thread_cpu_us`; the PR #55 review's I3: a listing preempted
/// or blocked costs no more than the CPU it used) from its start, and by `GPU_WATCH_MS` at the
/// least, so a wait spends at most this share while that pace is within `GPU_LISTING_GAP_MAX_MS`.
pub const GPU_WAIT_SHARE_PCT: u64 = 5;
/// Owed 70 (a), the PR #55 review's I3: F-026's shortest estimated life of a relaunched GPU process
/// in C14's fast loop (10 to 19 ms; `GPU_WATCH_MS` says how it is estimated). A wait's listings
/// come closer together than this, or such a relaunch can live and die between two of them unseen.
pub const GPU_RELAUNCH_LIFE_MIN_MS: u64 = 10;
/// Owed 70 (a), the PR #55 review's I3 (the coordinator's decision, 2026-10-24): the ceiling, on the
/// clock, on the gap between two of a wait's listings. `GPU_RELAUNCH_LIFE_MIN_MS` less a 2 ms
/// margin: a listing reads a pid anywhere inside its own time (the longest measured with CEF's
/// children, 1 643 us, image run 40080601547's debug build) and a sleep overshoots its end. A
/// listing whose CPU pace is longer than this is followed this long after its start instead, so a
/// wait spends more than `GPU_WAIT_SHARE_PCT` of a core while a listing costs more CPU than
/// `GPU_WAIT_SHARE_PCT` of this (400 us), and the wait's INFO line says so.
pub const GPU_LISTING_GAP_MAX_MS: u64 = GPU_RELAUNCH_LIFE_MIN_MS - 2;
/// Owed 70 (a), the PR #55 review's m1: after a death's wait, while no GPU process is known, the
/// watch keeps listing at this share (paced as for `GPU_WAIT_SHARE_PCT`, with no ceiling), so a
/// relaunch later than `GPU_RELAUNCH_WAIT_MS` is still listed, and its death counted.
pub const GPU_LATE_SHARE_PCT: u64 = 1;
/// Spec §6: CEF runs under nice 10.
pub const CEF_NICE: i32 = 10;
/// Spec §6's OOM order: CEF's children go first (C12's encoder writes −900), and among them the
/// renderer, the one CEF replaces by itself (its reload). PR #32 review G1: a flat 1000 left the
/// zygote 4 points behind the renderer, and a dead zygote ends CEF for the process (Owed 56), or
/// the process itself at the next GPU-process death (N1).
pub const CEF_OOM_SCORE_ADJ_RENDERER: i32 = 1_000;
/// The GPU and utility processes: Chromium relaunches them under their zygote (F-013: a killed GPU
/// process recovered while its zygote lived, 5 times; the 6th death in about 10 s, or the first
/// after that zygote's, is Chromium's FATAL abort of the whole process: the confirm pass, Owed 61).
pub const CEF_OOM_SCORE_ADJ_HELPER: i32 = 800;
/// The zygotes, and any process type this table does not know: the last of CEF's to go, still
/// ahead of the compositor itself (0) and its encoder (−900).
pub const CEF_OOM_SCORE_ADJ_ZYGOTE: i32 = 500;
/// A retired build whose teardown has still not returned this long after the reaper's kill is
/// counted leaked (Owed 55, owner ruling 2026-10-19 "B"): its thread, pipeline and renderer stay
/// until the process restarts.
pub const TEARDOWN_LEAK_AFTER_MS: u64 = 3_000;
/// The process's private CEF profile directory, under `std::env::temp_dir()` (PR #32 review I1).
const PROFILE_DIR_PREFIX: &str = "seazn-cef-";
/// F-013: no spare renderer, so each browser has exactly one renderer and the reaper can tell whose
/// it is. Global to the process, read when CEF initialises.
pub const SPARE_RENDERER_OFF: &str = "disable-features=SpareRendererForSitePerProcess";
/// F-013: at CEF's default severity every console message is written with its source URL (the page
/// URL, token included, for an inline script) to `chrome_debug.log`. Global, read at CEF's start.
pub const CEF_LOG_SEVERITY: &str = "disable";
/// cefsrc's debug categories (gstcefsrc.cc).
const CONSOLE_CATEGORY: &str = "cefconsole";
const CEFSRC_CATEGORY: &str = "cefsrc";
/// What cefsrc logs in `OnRenderProcessTerminated` (gstcefsrc.cc), then it reloads the page.
const RENDERER_TERMINATED: &str = "Render subprocess terminated, reloading URL!";
/// cefsrc warns each time one of its global properties is set (gstcefsrc.cc:1214–1290).
const DEPRECATED_PROPERTY: &str = "property is deprecated and is global";
/// The page-line queue between the log handler and the dispatcher.
const NOTE_QUEUE_CAP: usize = 512;
/// Page console lines (not signals) logged per build; the rest are counted.
const PAGE_LINES_PER_BUILD: u32 = 20;
/// The renderer-exit storm after a zygote dies is logged at its first line and then every this many.
const STORM_LOG_EVERY: u64 = 1_000;

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn stderr_log() -> LogFn {
    // C12 replaces this with its `ts=… level=info msg=…` sink (`with_log`).
    Arc::new(|line: &str| eprintln!("graphics: {line}"))
}

// ---------------------------------------------------------------------------------------------
// Page lines

/// A console line of the page, as this adapter reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageSignal {
    Ready,
    Alive,
    ScoreStale(bool),
    Channel(PhaseChannel),
    /// A `seazn:` line that is none of the above: logged at WARN, mapped to nothing.
    Unknown,
    /// Not ours (the page's own console output).
    Other,
}

/// The exact lines F-001 and R2005 fixed. Nothing is trimmed or lowercased.
pub(crate) fn page_signal(message: &str) -> PageSignal {
    match message {
        "seazn:ready" => PageSignal::Ready,
        "seazn:alive" => PageSignal::Alive,
        "seazn:score-stale=true" => PageSignal::ScoreStale(true),
        "seazn:score-stale=false" => PageSignal::ScoreStale(false),
        "seazn:phase-channel=loopback" => PageSignal::Channel(PhaseChannel::Loopback),
        "seazn:phase-channel=web" => PageSignal::Channel(PhaseChannel::Web),
        m if m.starts_with("seazn:") => PageSignal::Unknown,
        _ => PageSignal::Other,
    }
}

/// A `cefconsole` text, `<source>:<line> <message>`, split at its first space: the source is a URL
/// (no raw space), so everything after it is the message. The source holds the page URL, token and
/// all, for an inline script: it is never logged unscrubbed.
pub(crate) fn split_console(text: &str) -> (&str, &str) {
    text.split_once(' ').unwrap_or((text, ""))
}

/// This adapter's element names, `cef-<instance>-<build>-<gen>`: how a debug line finds its build.
fn element_name(inst: u64, build: u64, gen: u64) -> String {
    format!("cef-{inst}-{build}-{gen}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ids {
    inst: u64,
    build: u64,
    gen: u64,
}

pub(crate) fn element_ids(name: &str) -> Option<Ids> {
    let mut it = name.strip_prefix("cef-")?.split('-');
    let mut next = || it.next().and_then(|s| s.parse::<u64>().ok());
    let ids = Ids {
        inst: next()?,
        build: next()?,
        gen: next()?,
    };
    it.next().is_none().then_some(ids)
}

/// What a URL carries that must never be printed: the URL itself and its `st` (job token) value.
pub(crate) fn secrets_of(url: &str) -> Vec<&str> {
    let mut out = vec![url];
    let query = url.split_once('?').map_or("", |(_, q)| q);
    let query = query.split_once('#').map_or(query, |(q, _)| q);
    for pair in query.split('&') {
        if let Some(("st", value)) = pair.split_once('=') {
            out.push(value);
        }
    }
    out
}

/// The `chrome-extra-flags` value: `SPARE_RENDERER_OFF`, then the environment's
/// `GST_CEF_CHROME_EXTRA_FLAGS` (the property replaces the variable, so it is carried over).
pub(crate) fn extra_flags(env: Option<&str>) -> String {
    match env.filter(|e| !e.is_empty()) {
        Some(e) => format!("{SPARE_RENDERER_OFF},{e}"),
        None => SPARE_RENDERER_OFF.to_owned(),
    }
}

// ---------------------------------------------------------------------------------------------
// Straight alpha

/// `T[a][c]`: premultiplied channel `c` at alpha `a`, as straight alpha, `c × 255 / a` rounded and
/// capped at 255. Row 0 is unused (alpha 0 has no colour).
fn unpremultiply_table() -> &'static [[u8; 256]; 256] {
    static TABLE: OnceLock<Box<[[u8; 256]; 256]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Box::new([[0u8; 256]; 256]);
        for (a, row) in t.iter_mut().enumerate().skip(1) {
            for (c, v) in row.iter_mut().enumerate() {
                *v = ((c * 255 + a / 2) / a).min(255) as u8;
            }
        }
        t
    })
}

/// BGRA premultiplied → BGRA straight, in place. Alpha 0 and 255 are already straight, and an
/// overlay is mostly those, so only the rest is touched (one frame: about 4 ms optimised, 25 ms in a
/// debug build; a two-buffer loop took 200 ms there, too slow for the tests' 30 fps page).
pub(crate) fn unpremultiply_bgra(px: &mut [u8]) {
    let t = unpremultiply_table();
    for p in px.chunks_exact_mut(4) {
        let a = p[3];
        if a != 0 && a != 255 {
            let row = &t[usize::from(a)];
            p[0] = row[usize::from(p[0])];
            p[1] = row[usize::from(p[1])];
            p[2] = row[usize::from(p[2])];
        }
    }
}

/// `sample` with straight alpha, its caps and timestamps kept: the programme's compositor blends
/// straight alpha. `None` for a sample that is not one whole 1920×1080 BGRA frame.
pub(crate) fn straight_alpha(sample: &gst::Sample) -> Option<gst::Sample> {
    let (w, h) = OVERLAY_SIZE;
    let buffer = sample.buffer()?;
    let caps = sample.caps_owned()?;
    let map = buffer.map_readable().ok()?;
    if map.len() != (w * h * 4) as usize {
        return None;
    }
    let mut out = map.as_slice().to_vec();
    drop(map);
    unpremultiply_bgra(&mut out);
    let mut straight = gst::Buffer::from_mut_slice(out);
    {
        let b = straight.get_mut()?;
        b.set_pts(buffer.pts());
        b.set_duration(buffer.duration());
    }
    Some(gst::Sample::builder().buffer(&straight).caps(&caps).build())
}

// ---------------------------------------------------------------------------------------------
// CEF's child processes (/proc)

/// One CEF child process, from `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CefChild {
    pub pid: i32,
    /// Its parent: the compositor, or the zygote that forked it.
    pub ppid: i32,
    /// Chromium's `--type=` (`renderer`, `zygote`, `gpu-process`, `utility`).
    pub kind: String,
    /// A renderer's `--renderer-client-id=`.
    pub client_id: Option<i64>,
}

/// The `oom_score_adj` each kind of CEF child is given.
pub fn oom_score_adj_for(kind: &str) -> i32 {
    match kind {
        "renderer" => CEF_OOM_SCORE_ADJ_RENDERER,
        "gpu-process" | "utility" => CEF_OOM_SCORE_ADJ_HELPER,
        _ => CEF_OOM_SCORE_ADJ_ZYGOTE,
    }
}

/// `/proc/<pid>/stat`: (ppid, nice). The command name sits in parentheses and may hold either, so the
/// fields are read after the LAST `)`.
pub(crate) fn parse_stat(stat: &str) -> Option<(i32, i32)> {
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name: state(0) ppid(1) … nice(16) (proc(5), fields 3, 4 and 19).
    let ppid = fields.get(1)?.parse().ok()?;
    let nice = fields.get(16)?.parse().ok()?;
    Some((ppid, nice))
}

/// `/proc/<pid>/cmdline` as arguments. Chromium rewrites its children's into one space-joined string,
/// so this splits on NUL and on whitespace.
pub(crate) fn split_cmdline(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw)
        .split(|c: char| c == '\0' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().find_map(|a| a.strip_prefix(flag))
}

/// The `Pss:` line of `/proc/<pid>/smaps_rollup`, in kB.
pub(crate) fn pss_kb(rollup: &str) -> Option<u64> {
    rollup
        .lines()
        .find_map(|l| l.strip_prefix("Pss:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
}

fn read_stat(pid: i32) -> Option<(i32, i32)> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Every descendant of this process whose command line carries `--type=` (every Chromium child does).
pub fn cef_children() -> Vec<CefChild> {
    let me = std::process::id() as i32;
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        if let Some((ppid, _)) = read_stat(pid) {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut out = Vec::new();
    let mut frontier = vec![me];
    while let Some(p) = frontier.pop() {
        for &kid in children.get(&p).map_or(&[][..], Vec::as_slice) {
            frontier.push(kid);
            let Ok(raw) = std::fs::read(format!("/proc/{kid}/cmdline")) else {
                continue;
            };
            let args = split_cmdline(&raw);
            if let Some(kind) = flag_value(&args, "--type=") {
                out.push(CefChild {
                    pid: kid,
                    ppid: p,
                    kind: kind.to_owned(),
                    client_id: flag_value(&args, "--renderer-client-id=").and_then(|v| v.parse().ok()),
                });
            }
        }
    }
    out.sort_by_key(|c| c.pid);
    out
}

/// What one poll did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Guarded {
    pub children: usize,
    pub threads: usize,
    pub reniced: usize,
    pub oom_written: usize,
    pub failures: usize,
    /// Every CEF child's PSS, summed.
    pub pss_mb: u64,
    /// The part the memory cap reads: every child but the renderers of other builds (a retiring
    /// or leaked one; PR #32 review G2).
    pub pss_counted_mb: u64,
    /// The same PSS reads by `--type=`, in kB (the soak's memory split): every child listed, and the
    /// ones whose PSS was read. A child whose PSS was not read counts 0 in the sums above, and here
    /// is listed but not read, so its kind's PSS reads unknown, never 0 (`CefPss::add`).
    pub pss_by_kind: CefPss,
}

/// The last child poll's number and its PSS by kind (the soak's memory split): the child poll writes
/// it, the metrics read it once a beat. Absent until the first poll.
#[derive(Debug, Clone, Default)]
pub struct CefMemory(Arc<Mutex<Option<CefPoll>>>);

impl CefMemory {
    pub fn read(&self) -> Option<CefPoll> {
        *locked(&self.0)
    }

    fn set(&self, p: CefPoll) {
        *locked(&self.0) = Some(p);
    }
}

/// Whether a child's memory counts against `current`'s cap: shared processes and unclaimed
/// renderers do; another build's renderer does not.
pub(crate) fn counts_against(kid: &CefChild, owner: Option<(u64, u64)>, current: (u64, u64)) -> bool {
    kid.kind != "renderer" || owner.is_none_or(|o| o == current)
}

/// Nice `CEF_NICE` on every thread and `oom_score_adj_for` its kind on every child, where it
/// differs; and the children's PSS sums.
fn guard(kids: &[CefChild], counts: impl Fn(&CefChild) -> bool) -> Guarded {
    let mut g = Guarded {
        children: kids.len(),
        ..Guarded::default()
    };
    let mut pss_kb_sum = 0u64;
    let mut counted_kb = 0u64;
    for kid in kids {
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{}/task", kid.pid)) {
            for task in tasks.flatten() {
                let Some(tid) = task.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                    continue;
                };
                let nice = std::fs::read_to_string(format!("/proc/{}/task/{tid}/stat", kid.pid))
                    .ok()
                    .and_then(|s| parse_stat(&s))
                    .map(|(_, n)| n);
                let Some(nice) = nice else { continue };
                g.threads += 1;
                if nice != CEF_NICE {
                    let set = rustix::process::Pid::from_raw(tid)
                        .map(|p| rustix::process::setpriority_process(Some(p), CEF_NICE));
                    match set {
                        Some(Ok(())) => g.reniced += 1,
                        _ => g.failures += 1,
                    }
                }
            }
        }
        let oom_path = format!("/proc/{}/oom_score_adj", kid.pid);
        let want = oom_score_adj_for(&kid.kind).to_string();
        if let Ok(now) = std::fs::read_to_string(&oom_path) {
            if now.trim() != want {
                match std::fs::write(&oom_path, &want) {
                    Ok(()) => g.oom_written += 1,
                    Err(_) => g.failures += 1,
                }
            }
        }
        let read = std::fs::read_to_string(format!("/proc/{}/smaps_rollup", kid.pid))
            .ok()
            .and_then(|r| pss_kb(&r));
        g.pss_by_kind.add(&kid.kind, read);
        let pss = read.unwrap_or(0);
        pss_kb_sum += pss;
        if counts(kid) {
            counted_kb += pss;
        }
    }
    g.pss_mb = pss_kb_sum / 1024;
    g.pss_counted_mb = counted_kb / 1024;
    g
}

// ---------------------------------------------------------------------------------------------
// Zygote death (Owed 56; owner ruling 2026-10-19 "B")

/// What the child polls have seen of CEF's zygotes (process-wide: CEF is one per process).
#[derive(Debug, Default)]
pub(crate) struct ZygoteWatch {
    /// Every zygote pid listed (a child of this process).
    seen: BTreeSet<i32>,
    /// Decided: CEF can start no renderer again in this process.
    dead: bool,
}

/// One listing's verdict (a child poll's or a ready's), true the first time any zygote seen is gone.
/// F-013 (CEF 139): one zygote parents the renderers, and its death makes every later build fail at
/// once (its orphaned renderer keeps painting, out of this process's tree). The other parents the
/// GPU process: its death looks harmless while that GPU process lives, but the next GPU-process
/// death is Chromium's FATAL "GPU process isn't usable. Goodbye." and the whole process dies of
/// SIGTRAP (the PR #32 confirm pass's N1, 3/3). Either way C11b's in-place restart must come first,
/// on its own terms.
///
/// A zygote missing from one poll's list counts only once `alive` says its process is gone: a
/// command line read that failed once must not end CEF for the process. A zygote is a child of
/// `root`, this process. A zygote's fresh fork reads `--type=zygote` until it is renamed a renderer
/// or a utility, and its later exit is no verdict: the likely cause of the one false verdict in the
/// confirm pass's 16 runs (no probe caught a fork in that state).
pub(crate) fn zygote_check(
    w: &mut ZygoteWatch,
    kids: &[CefChild],
    root: i32,
    alive: impl Fn(i32) -> bool,
) -> bool {
    if w.dead {
        return false;
    }
    let now: BTreeSet<i32> = kids
        .iter()
        .filter(|k| k.kind == "zygote" && k.ppid == root)
        .map(|k| k.pid)
        .collect();
    w.seen.extend(now.iter().copied());
    let gone = |p: &i32| !now.contains(p) && !alive(*p);
    w.dead = w.seen.iter().any(gone);
    w.dead
}

/// What the child poll thread has seen of CEF's GPU processes (Owed 61; process-wide like the
/// zygotes), from its polls and from the GPU watch's steps between them (Owed 70 (a)).
#[derive(Debug, Default)]
pub(crate) struct GpuWatch {
    /// GPU-process pids seen and not yet seen gone.
    live: BTreeSet<i32>,
    /// GPU-process deaths seen so far: a count over the process's life, never reset.
    deaths: usize,
    /// When the current wait for a GPU process ends, in us on `ProcTree::clock` (Owed 70 (a)):
    /// `GPU_RELAUNCH_WAIT_MS` after CEF's start (the watch's first step) and after each death's
    /// listing. `None` before the first step.
    wait_ends_us: Option<u64>,
    /// The earliest start of the watch's next listing while no GPU process is known (us): the last
    /// listing's start, paced by its cost (`GpuWatch::paced`).
    next_listing_us: u64,
    /// The current wait's steps' listings so far (the PR #55 review's I3): its INFO line's numbers.
    waited: WaitListings,
}

/// One wait's listings by the GPU watch's steps: how many, their CPU and clock time (us), the gap
/// the last one set before the next (us), whether any gap was held at `GPU_LISTING_GAP_MAX_MS`, when
/// the wait began (us), and, once its listings are over, the wait's time on the clock (us: from its
/// start to the instant they are over).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaitListings {
    listings: u64,
    cpu_us: u64,
    clock_us: u64,
    gap_us: u64,
    held: bool,
    began_us: u64,
    span_us: u64,
}

impl GpuWatch {
    /// The share of a core the watch's listings may use at `now` (us), as a percentage: none while a
    /// GPU process is known (a step only reads its liveness), `GPU_WAIT_SHARE_PCT` in a wait, and
    /// `GPU_LATE_SHARE_PCT` after a death's wait (none after CEF's start's).
    fn listing_share(&self, now: u64) -> Option<u64> {
        if !self.live.is_empty() {
            return None;
        }
        if self.in_wait(now) {
            return Some(GPU_WAIT_SHARE_PCT);
        }
        (self.deaths > 0).then_some(GPU_LATE_SHARE_PCT)
    }

    /// `now` (us) is inside the current wait.
    fn in_wait(&self, now: u64) -> bool {
        self.wait_ends_us.is_some_and(|end| now < end)
    }

    /// A wait's listing is due at `now`.
    fn listing_due(&self, now: u64) -> bool {
        self.listing_share(now).is_some() && now >= self.next_listing_us
    }

    /// A wait for a GPU process from `now`, which its numbers start from (a wait that has listed
    /// keeps its own start).
    fn arm(&mut self, now: u64) {
        self.wait_ends_us = Some(now + GPU_RELAUNCH_WAIT_MS * 1_000);
        if self.waited.listings == 0 {
            self.waited.began_us = now;
        }
    }

    /// After a step's listing that started at `start` and cost `cpu` us of the thread's CPU and
    /// `clock` us on the clock: the next starts `100 / share` times that CPU after it (the share in
    /// force at `start`), `GPU_WATCH_MS` after it at the soonest, and in a wait
    /// `GPU_LISTING_GAP_MAX_MS` after it at the latest. A wait's listing is added to its numbers.
    fn paced(&mut self, start: u64, cpu: u64, clock: u64) {
        let share = self.listing_share(start).unwrap_or(GPU_WAIT_SHARE_PCT);
        let pace = (GPU_WATCH_MS * 1_000).max(cpu * 100 / share);
        let ceiling = GPU_LISTING_GAP_MAX_MS * 1_000;
        let in_wait = self.in_wait(start);
        let gap = if in_wait { pace.min(ceiling) } else { pace };
        self.next_listing_us = start + gap;
        if in_wait {
            let w = &mut self.waited;
            w.listings += 1;
            w.cpu_us += cpu;
            w.clock_us += clock;
            w.gap_us = gap;
            w.held |= pace > ceiling;
        }
    }

    /// The current wait's numbers, once its listings are over at `now` (us): a GPU process is
    /// listed (by a step, or by a poll: the next step reads it), or the wait has ended. Once per
    /// wait that listed (none for a wait whose steps never listed), and the next wait's count from
    /// zero.
    fn wait_over(&mut self, now: u64) -> Option<WaitListings> {
        let over = !self.live.is_empty() || !self.in_wait(now);
        (over && self.waited.listings > 0).then(|| {
            let mut w = std::mem::take(&mut self.waited);
            w.span_us = now.saturating_sub(w.began_us);
            w
        })
    }

    /// How long the child poll thread sleeps after a step at `now` (us): `GPU_WATCH_MS`, or less when
    /// a listing is due sooner, so a gap is the pace (and the ceiling) rather than a whole number of
    /// steps.
    fn sleep_us(&self, now: u64) -> u64 {
        let step = GPU_WATCH_MS * 1_000;
        if self.listing_share(now).is_none() {
            return step;
        }
        self.next_listing_us.saturating_sub(now).min(step)
    }
}

/// One listing's new GPU-process deaths (C11b). F-013 (CEF 139; probe GR, 2 of 2, and the final C11
/// review's FD): Chromium relaunches a dead GPU process, and aborts the whole process at the 6th
/// death of its life, so the adapter counts them and the domain restarts the process short of it.
///
/// A GPU process missing from one listing counts only once `alive` says its process is gone, as
/// for a zygote. That also covers the final C11 review's G4: a GPU process whose zygote died is
/// reparented to pid 1 and leaves `cef_children()` while it lives on; it is no death (and with its
/// zygote gone the domain is already restarting the process, whose end ends it).
///
/// A GPU process no listing saw is not counted (Owed 70 (a)). Between child polls the GPU watch
/// (`gpu_step`) lists while no GPU process is known, paced by each listing's CPU, so what goes
/// unseen is a GPU process visible for less than the gap between two listings: in a death's wait
/// `GPU_WATCH_MS` at the least and `GPU_LISTING_GAP_MAX_MS` at the most, under F-026's
/// `GPU_RELAUNCH_LIFE_MIN_MS`; after it, the late share's pace; and one born and dead inside a
/// listing that stalls on the clock (before the watch it was any relaunch that lived under
/// `CHILD_POLL_MS`: F-026's fast loop counted 1 or 2 of 5). The threshold still leaves two
/// survivable deaths after it (`cef_gpu_deaths_to_restart`): one for such an unseen death, and one
/// for a death during the exit.
pub(crate) fn gpu_check(w: &mut GpuWatch, kids: &[CefChild], alive: impl Fn(i32) -> bool) -> usize {
    let now: BTreeSet<i32> = kids
        .iter()
        .filter(|k| k.kind == "gpu-process")
        .map(|k| k.pid)
        .collect();
    let gone: Vec<i32> = w
        .live
        .iter()
        .copied()
        .filter(|p| !now.contains(p) && !alive(*p))
        .collect();
    for p in &gone {
        w.live.remove(p);
    }
    w.live.extend(now);
    w.deaths += gone.len();
    gone.len()
}

/// One step of the GPU watch between two child polls (Owed 70 (a); F-026): CEF's children are
/// listed when a GPU process seen before is no longer `alive`, and while no GPU process is known,
/// when a listing is due (`GpuWatch::listing_due`: in the wait after CEF's start, the watch's first
/// step, or after a death, and after a death's wait; each listing paced by its CPU,
/// `GpuWatch::paced`). Any other
/// step costs one `stat` read per known GPU process. Each death is then counted at the first step
/// after it, and each relaunch is listed at the first listing after its birth. Only one adapter's
/// child poll thread counts (`CefWatch::claim_gpu`), and it steps between its own polls, so no
/// listing is ever applied after a newer one (which could count a death twice). Each wait that
/// listed is summed up in one INFO line once its listings are over (`GpuWatch::wait_over`). True
/// when it listed.
fn gpu_step(shared: &Shared) -> bool {
    let now = (shared.tree.clock)();
    let (live, due, over) = {
        let mut w = locked(&shared.watch);
        if !w.claim_gpu(shared.inst) {
            return false;
        }
        if w.gpu.wait_ends_us.is_none() {
            w.gpu.arm(now);
        }
        (
            w.gpu.live.iter().copied().collect::<Vec<i32>>(),
            w.gpu.listing_due(now),
            w.gpu.wait_over(now),
        )
    };
    log_wait(shared, over);
    if !due && live.iter().all(|p| (shared.tree.alive)(*p)) {
        return false;
    }
    let (start, cpu0) = ((shared.tree.clock)(), (shared.tree.cpu)());
    let kids = (shared.tree.list)();
    let cpu = (shared.tree.cpu)().saturating_sub(cpu0);
    let clock = (shared.tree.clock)().saturating_sub(start);
    let mut watch = locked(&shared.watch);
    let died = gpu_check(&mut watch.gpu, &kids, &*shared.tree.alive);
    if died > 0 {
        watch.gpu.arm(start);
    }
    watch.gpu.paced(start, cpu, clock);
    let over = watch.gpu.wait_over(start + clock);
    let deaths = watch.gpu.deaths;
    drop(watch);
    log_gpu_gone(shared, died, deaths);
    log_wait(shared, over);
    true
}

/// How long the child poll thread sleeps after a step (`GpuWatch::sleep_us`; `GPU_WATCH_MS` for an
/// adapter whose thread does not count the GPU processes).
fn gpu_sleep_us(shared: &Shared) -> u64 {
    let now = (shared.tree.clock)();
    let w = locked(&shared.watch);
    if w.gpu_lister == Some(shared.inst) {
        w.gpu.sleep_us(now)
    } else {
        GPU_WATCH_MS * 1_000
    }
}

/// One INFO line for a wait whose listings are over (none for `None`; the PR #55 review's I3): how
/// many listings over how long, their mean CPU and clock cost, the last gap, and the wait's share of
/// a core: all its listings' CPU over its time on the clock (the confirm pass's r2-m2; a death's
/// wait whose first listing finds the relaunch was that one listing, so it reads 100%), so a run's
/// log shows what the watch spent.
fn log_wait(shared: &Shared, over: Option<WaitListings>) {
    let Some(w) = over else { return };
    let n = w.listings.max(1);
    let (cpu, clock) = (w.cpu_us / n, w.clock_us / n);
    let (span_ms, gap_ms) = (w.span_us as f64 / 1_000.0, w.gap_us as f64 / 1_000.0);
    let pct = w.cpu_us as f64 * 100.0 / w.span_us.max(1) as f64;
    let held = if w.held {
        format!(", a gap held at the {GPU_LISTING_GAP_MAX_MS} ms ceiling")
    } else {
        String::new()
    };
    shared.log(&format!(
        "INFO: the GPU watch's wait listed {} time(s) in {span_ms:.1} ms at {cpu} us of CPU each \
         ({clock} us on the clock), the last {gap_ms:.1} ms apart: {pct:.1}% of a core{held} (Owed \
         70 (a))",
        w.listings
    ));
}

/// One WARN line for a listing that saw `died` GPU processes gone (none for 0).
fn log_gpu_gone(shared: &Shared, died: usize, deaths: usize) {
    if died == 0 {
        return;
    }
    shared.log(&format!(
        "WARN gen={}: {died} CEF GPU process(es) gone, {deaths} in this process; Chromium aborts it \
         at the 6th (Owed 61)",
        shared.newest_gen().unwrap_or(0)
    ));
}

/// The process's view of CEF: the zygotes, the verdict (`Some(gen)` once a CEF zygote is gone, the
/// newest build's generation then), and the GPU processes.
#[derive(Debug, Default)]
struct CefWatch {
    zygotes: ZygoteWatch,
    verdict: Option<u64>,
    gpu: GpuWatch,
    /// The adapter (`Shared::inst`) whose child poll thread counts the GPU processes (the PR #55
    /// review's m2): the first to claim it, until its thread ends.
    gpu_lister: Option<u64>,
}

impl CefWatch {
    /// True when `inst`'s child poll thread counts the GPU processes: it holds the claim, or takes
    /// it when no thread does.
    fn claim_gpu(&mut self, inst: u64) -> bool {
        *self.gpu_lister.get_or_insert(inst) == inst
    }

    /// `inst`'s child poll thread has ended: the next adapter's may count.
    fn release_gpu(&mut self, inst: u64) {
        if self.gpu_lister == Some(inst) {
            self.gpu_lister = None;
        }
    }
}

/// The process's one CEF watch, held by every adapter's `Shared` (CEF is one per process).
fn cef_watch() -> Arc<Mutex<CefWatch>> {
    static WATCH: OnceLock<Arc<Mutex<CefWatch>>> = OnceLock::new();
    Arc::clone(WATCH.get_or_init(Arc::default))
}

/// The process tree the CEF watch reads: this process's `/proc` in production (`ProcTree::live`), a
/// fake in a unit test.
struct ProcTree {
    /// This process's CEF children (`cef_children`).
    list: Box<dyn Fn() -> Vec<CefChild> + Send + Sync>,
    /// Whether a pid's process exists and is not a zombie (`pid_alive`).
    alive: Box<dyn Fn(i32) -> bool + Send + Sync>,
    /// This process: a zygote is its child (`zygote_check`).
    root: i32,
    /// A monotonic clock in microseconds (`monotonic_us`): the GPU watch's waits, its gaps, and each
    /// listing's time on the clock.
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
    /// The calling thread's CPU time in microseconds (`thread_cpu_us`): what each listing the GPU
    /// watch paces costs (the PR #55 review's I3).
    cpu: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl ProcTree {
    fn live() -> Self {
        Self {
            list: Box::new(cef_children),
            alive: Box::new(pid_alive),
            root: std::process::id() as i32,
            clock: Box::new(monotonic_us),
            cpu: Box::new(thread_cpu_us),
        }
    }
}

/// Microseconds since this process first read the clock.
fn monotonic_us() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    u64::try_from(ORIGIN.get_or_init(Instant::now).elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// The calling thread's CPU time in microseconds (`CLOCK_THREAD_CPUTIME_ID`): what a GPU watch
/// listing costs (the PR #55 review's I3), which a preempted or blocked listing does not inflate.
pub fn thread_cpu_us() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    let secs = u64::try_from(t.tv_sec).unwrap_or(0);
    let micros = u64::try_from(t.tv_nsec).unwrap_or(0) / 1_000;
    secs.saturating_mul(1_000_000).saturating_add(micros)
}

/// One listing read into the zygote watch: `Some(gen)` when it decides the verdict, which it
/// publishes with `gen`, the newest build's generation then (once for the process).
fn read_zygotes(shared: &Shared, watch: &mut CefWatch, kids: &[CefChild]) -> Option<u64> {
    if !zygote_check(&mut watch.zygotes, kids, shared.tree.root, &*shared.tree.alive) {
        return None;
    }
    let gen = shared.newest_gen().unwrap_or(0);
    watch.verdict = Some(gen);
    Some(gen)
}

fn log_zygote_gone(shared: &Shared, gen: u64) {
    shared.log(&format!(
        "ERROR gen={gen}: a CEF zygote is gone; no renderer may start again, and the next GPU-process \
         death aborts this process (Owed 56: restart in place)"
    ));
}

/// A ready's listing, read into the zygote watch before the ready is logged or relayed (image run
/// 40015873298: 1 of 44 probe kills). The child poll starts with the first build, before CEF has
/// forked a zygote, and lists every `CHILD_POLL_MS`, so the zygotes were first listed by its 2nd
/// poll, about 1 s in, while the page readied at about 0.72 s; a zygote killed in between was never
/// seen, so never missed (no verdict: Chromium's abort at the next GPU-process death, or a failed
/// next build). A ready means a renderer runs, so both zygotes exist (F-013), and this listing comes
/// before anything that hears of it can act on it.
///
/// Fewer than two zygotes in it is no verdict: two is F-013's measurement of CEF 139, not a
/// contract (a CEF with another count would restart every session at its first ready), and a zygote
/// never listed has no pid for `alive` to confirm gone, so a failed command-line read would read as
/// a death. A zygote that dies between CEF's start and the first ready stays unseen.
///
/// The GPU-process count belongs to one child poll thread alone (its polls, and the GPU watch's
/// steps between them; `CefWatch::claim_gpu`): a list applied after a newer one can put a dead GPU
/// process back among the live, and count its death twice.
fn watch_ready(shared: &Shared) {
    let kids = (shared.tree.list)();
    let verdict = read_zygotes(shared, &mut locked(&shared.watch), &kids);
    if let Some(gen) = verdict {
        log_zygote_gone(shared, gen);
    }
}

/// The child poll's listing read into the CEF watch, under one hold of its lock: the GPU-process
/// deaths (when this adapter's thread counts them, `CefWatch::claim_gpu`) and the zygotes, each
/// logged once the lock is let go.
fn watch_poll(shared: &Shared, kids: &[CefChild]) {
    let now = (shared.tree.clock)();
    let mut watch = locked(&shared.watch);
    let died = if watch.claim_gpu(shared.inst) {
        gpu_check(&mut watch.gpu, kids, &*shared.tree.alive)
    } else {
        0
    };
    if died > 0 {
        watch.gpu.arm(now);
    }
    let deaths = watch.gpu.deaths;
    let verdict = read_zygotes(shared, &mut watch, kids);
    drop(watch);
    log_gpu_gone(shared, died, deaths);
    if let Some(gen) = verdict {
        log_zygote_gone(shared, gen);
    }
}

// ---------------------------------------------------------------------------------------------
// The private profile (PR #32 review I1)

/// CEF writes its profile (History, Top Sites, the HTTP cache, `ukm_db`) with every page URL it
/// loads, job token included, to disk; incognito does not stop it (the review's P1). cefsrc's
/// `cef-cache-location` moves the whole profile (the fix loop's probe), but CEF reads it once, at
/// its start: one per process. So the process gets one private directory (mode 0700), set on every
/// element; it is removed when the last `CefGraphics` drops (the process's exit, in production), and
/// any left by a process that died first is removed at the next start.
///
/// It fails closed (the PR #32 confirm pass's N4): without the directory no build is made, so CEF
/// never falls back to HOME; the build fails as a graphics failure (`CefCrashed`), the programme
/// runs on without the overlay, and the domain's next restart tries the directory again.
#[derive(Default)]
struct Profile {
    path: Option<std::path::PathBuf>,
    live: usize,
}

fn profile() -> MutexGuard<'static, Profile> {
    static PROFILE: OnceLock<Mutex<Profile>> = OnceLock::new();
    locked(PROFILE.get_or_init(Mutex::default))
}

/// `<base>/seazn-cef-<pid>-<nanos>`, created with mode 0700.
pub(crate) fn make_profile_dir(base: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = base.join(format!("{PROFILE_DIR_PREFIX}{}-{nanos}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
}

/// Removes every `seazn-cef-<pid>-…` directory in `base` whose process is gone, or that carries
/// `own`, this process's pid. It runs before this process image makes its own directory, so one
/// with its pid is an earlier image's: an exec, or a restarted container that gave the compositor
/// the same pid again (the PR #32 confirm pass's N2). Returns how many.
pub(crate) fn sweep_stale_profiles(base: &std::path::Path, own: i32, alive: impl Fn(i32) -> bool) -> usize {
    let Ok(dir) = std::fs::read_dir(base) else {
        return 0;
    };
    let mut removed = 0;
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(PROFILE_DIR_PREFIX))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, _)| pid.parse::<i32>().ok())
        else {
            continue;
        };
        if (pid == own || !alive(pid)) && std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// `/proc/<pid>/stat` reads that failed with neither ENOENT nor ESRCH, since the process started.
static UNREAD_STATS: AtomicU64 = AtomicU64::new(0);

/// What one `/proc/<pid>/stat` read says of its process (the PR #55 review's m3): alive, or dead (a
/// zombie; gone: ENOENT at the open, ESRCH at the read), or nothing (any other error, such as
/// EMFILE or ENOMEM).
fn stat_alive(read: std::io::Result<String>) -> Option<bool> {
    match read {
        Ok(stat) => Some(
            !stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        ),
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
        {
            Some(false)
        }
        Err(_) => None,
    }
}

/// The process exists and is not a zombie (`read_alive`).
fn pid_alive(pid: i32) -> bool {
    read_alive(std::fs::read_to_string(format!("/proc/{pid}/stat")))
}

/// `stat_alive`, with a read that says nothing taken as alive and counted (`UNREAD_STATS`): a death
/// is counted only for a process known gone, so an error episode cannot pile up false GPU-process
/// deaths, or decide a false zygote verdict.
fn read_alive(read: std::io::Result<String>) -> bool {
    stat_alive(read).unwrap_or_else(|| {
        UNREAD_STATS.fetch_add(1, Ordering::Relaxed);
        true
    })
}

/// One WARN line when `UNREAD_STATS` has grown since `seen` (the child poll's, once a poll).
fn log_unread_stats(shared: &Shared, seen: &mut u64) {
    let n = UNREAD_STATS.load(Ordering::Relaxed);
    if n > *seen {
        shared.log(&format!(
            "WARN: {} /proc stat read(s) failed with neither ENOENT nor ESRCH: read as alive, not as \
             deaths (Owed 70 (a))",
            n - *seen
        ));
        *seen = n;
    }
}

/// One more live adapter's hold on the profile directory (made at its first build).
fn profile_lease() -> ProfileLease {
    profile().live += 1;
    ProfileLease { held: true }
}

/// The profile directory, made (after the sweep) or made again if it is missing; `Err` refuses the
/// build (N4). `log` gets the sweep's line and is called after the lock is let go.
fn profile_ensure(log: impl Fn(&str)) -> Result<std::path::PathBuf, String> {
    let mut lines = Vec::new();
    let out = {
        let mut p = profile();
        match p.path.clone() {
            None => {
                let base = std::env::temp_dir();
                let swept = sweep_stale_profiles(&base, std::process::id() as i32, pid_alive);
                if swept > 0 {
                    lines.push(format!(
                        "removed {swept} CEF profile directories of processes that are gone"
                    ));
                }
                make_profile_dir(&base).inspect(|path| p.path = Some(path.clone()))
            }
            Some(path) if path.exists() => Ok(path),
            Some(path) => {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .map(|()| path)
            }
        }
    };
    for l in &lines {
        log(l);
    }
    out.map_err(|e| {
        format!(
            "no private CEF profile directory ({e}); the page is not loaded, so CEF writes \
             nothing under HOME"
        )
    })
}

/// One live adapter's `profile_lease`, released on drop: the last one removes the profile
/// directory.
struct ProfileLease {
    held: bool,
}

impl Drop for ProfileLease {
    fn drop(&mut self) {
        if !self.held {
            return;
        }
        let mut p = profile();
        p.live = p.live.saturating_sub(1);
        if p.live == 0 {
            if let Some(path) = &p.path {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Renderer attribution (process-wide: CEF is one per process)

#[derive(Default)]
struct Claims {
    /// The build every newly seen renderer client id belongs to: (instance, build).
    newest: Option<(u64, u64)>,
    by_client: BTreeMap<i64, (u64, u64)>,
    /// Claim passes that found more than one unclaimed renderer (a spare renderer is back).
    ambiguous: u64,
}

fn claims() -> MutexGuard<'static, Claims> {
    static CLAIMS: OnceLock<Mutex<Claims>> = OnceLock::new();
    locked(CLAIMS.get_or_init(Mutex::default))
}

/// Claims every unclaimed renderer for the newest build. True when more than one was unclaimed.
fn claim(kids: &[CefChild]) -> bool {
    let mut c = claims();
    let Some(owner) = c.newest else { return false };
    let fresh: Vec<i64> = kids
        .iter()
        .filter(|k| k.kind == "renderer")
        .filter_map(|k| k.client_id)
        .filter(|id| !c.by_client.contains_key(id))
        .collect();
    for &id in &fresh {
        c.by_client.insert(id, owner);
    }
    // Client ids only grow; the oldest claims go first.
    while c.by_client.len() > 256 {
        c.by_client.pop_first();
    }
    let ambiguous = fresh.len() > 1;
    if ambiguous {
        c.ambiguous += 1;
    }
    ambiguous
}

/// The live renderer pids claimed by `(inst, build)`.
fn renderers_of(kids: &[CefChild], inst: u64, build: u64) -> Vec<i32> {
    let c = claims();
    kids.iter()
        .filter(|k| k.kind == "renderer")
        .filter(|k| k.client_id.and_then(|id| c.by_client.get(&id)) == Some(&(inst, build)))
        .map(|k| k.pid)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The process-wide log handler

#[derive(Default)]
struct Router {
    routes: Mutex<HashMap<u64, Arc<Shared>>>,
    /// Every secret any instance knows, for the lines forwarded to stderr.
    scrubber: Mutex<Scrubber>,
    /// `cefconsole` lines of no live instance's element: dropped, never printed.
    unrouted_console: AtomicU64,
    #[cfg(feature = "test-hooks")]
    forward: Mutex<Option<LogFn>>,
}

fn router() -> &'static Router {
    static ROUTER: OnceLock<Router> = OnceLock::new();
    ROUTER.get_or_init(Router::default)
}

impl Router {
    fn route(&self, inst: u64) -> Option<Arc<Shared>> {
        locked(&self.routes).get(&inst).cloned()
    }

    fn forward(&self, line: &str) {
        let clean = locked(&self.scrubber).scrub(line);
        #[cfg(feature = "test-hooks")]
        if let Some(sink) = locked(&self.forward).clone() {
            sink(&clean);
            return;
        }
        eprintln!("{clean}");
    }
}

/// Installs the handler once per process: it replaces GStreamer's default one, then raises the two
/// cefsrc thresholds this adapter needs (never lowering what `GST_DEBUG` set).
fn install_log_handler() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = router();
        let _ = gst::log::add_log_function(|cat, level, file, function, line, obj, msg| {
            let Some(text) = msg.get() else { return };
            let object = obj.map(|o| o.to_string());
            on_gst_log(
                cat.name(),
                level,
                file.as_str(),
                function.as_str(),
                line,
                object.as_deref(),
                text.as_str(),
            );
        });
        gst::log::remove_default_log_function();
        // cefsrc registers both categories in its class_init, which otherwise runs at the first
        // element: referencing the class registers them now, so the thresholds an operator's
        // GST_DEBUG gave them are read, and only ever raised. (A threshold set by name before the
        // category exists is a new pattern, which outranks GST_DEBUG's and would lower it.)
        let _class = gst::ElementFactory::find("cefsrc")
            .and_then(|f| f.load().ok())
            .and_then(|f| gst::glib::Class::<gst::Element>::from_type(f.element_type()));
        for (name, floor) in [
            (CONSOLE_CATEGORY, gst::DebugLevel::Info),
            (CEFSRC_CATEGORY, gst::DebugLevel::Warning),
        ] {
            match gst::DebugCategory::get(name) {
                Some(cat) if cat.threshold() >= floor => {}
                Some(cat) => cat.set_threshold(floor),
                None => gst::log::set_threshold_for_name(name, floor),
            }
        }
    });
}

/// Every GStreamer debug line of the process, on the thread that logged it (CEF's UI thread for page
/// lines): never blocks, never logs through GStreamer.
fn on_gst_log(
    category: &str,
    level: gst::DebugLevel,
    file: &str,
    function: &str,
    line: u32,
    object: Option<&str>,
    text: &str,
) {
    let r = router();
    let cef = category == CONSOLE_CATEGORY || category == CEFSRC_CATEGORY;
    if cef {
        if let Some(ids) = object.and_then(element_ids) {
            if let Some(shared) = r.route(ids.inst) {
                if let Some(note) = classify(category, level, ids, text, || {
                    default_layout(level, category, file, line, function, object, text)
                }) {
                    // A renderer's exit closes its build's gate here, before anything is queued
                    // (PR #32 review m3): a dispatcher blocked on a full channel must not hold it
                    // open while cefsrc's reload paints.
                    let note = match note {
                        Note::RendererGone { build, gen } if shared.decide_crash(build) => Note::Crashed {
                            gen,
                            why: "the renderer process exited (cefsrc reloads the page)".to_owned(),
                        },
                        other => other,
                    };
                    shared.push(note);
                }
            }
            return;
        }
        if category == CONSOLE_CATEGORY {
            r.unrouted_console.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    r.forward(&default_layout(
        level, category, file, line, function, object, text,
    ));
}

fn default_layout(
    level: gst::DebugLevel,
    category: &str,
    file: &str,
    line: u32,
    function: &str,
    object: Option<&str>,
    text: &str,
) -> String {
    static T0: OnceLock<Instant> = OnceLock::new();
    let t = T0.get_or_init(Instant::now).elapsed();
    format!(
        "{}.{:09} {:>6} {:<7} {:>20} {file}:{line}:{function}:<{}> {text}",
        t.as_secs(),
        t.subsec_nanos(),
        std::process::id(),
        format!("{level:?}").to_uppercase(),
        category,
        object.unwrap_or(""),
    )
}

/// A debug line of one of this adapter's elements, as the dispatcher's note (None: dropped).
fn classify(
    category: &str,
    level: gst::DebugLevel,
    ids: Ids,
    text: &str,
    layout: impl FnOnce() -> String,
) -> Option<Note> {
    let Ids { build, gen, .. } = ids;
    if category == CONSOLE_CATEGORY {
        let (_source, message) = split_console(text);
        return match page_signal(message) {
            // The page's own output: its warnings and errors are logged (`PAGE_LINES_PER_BUILD` a
            // build), its info and debug lines dropped.
            PageSignal::Other if level > gst::DebugLevel::Warning => None,
            signal => Some(Note::Page {
                build,
                gen,
                signal,
                text: message.to_owned(),
            }),
        };
    }
    if level > gst::DebugLevel::Warning {
        // Only an operator's GST_DEBUG asks for these: printed as asked, scrubbed, off this thread.
        return Some(Note::Forward(layout()));
    }
    if text == RENDERER_TERMINATED {
        return Some(Note::RendererGone { build, gen });
    }
    if text.contains(DEPRECATED_PROPERTY) {
        // This adapter sets those properties itself, on every build.
        return None;
    }
    Some(Note::Element {
        gen,
        text: format!("{level:?}: {text}"),
    })
}

// ---------------------------------------------------------------------------------------------
// The adapter

/// What the dispatcher relays, in order.
#[derive(Debug)]
enum Note {
    Page {
        build: u64,
        gen: u64,
        signal: PageSignal,
        text: String,
    },
    RendererGone {
        build: u64,
        gen: u64,
    },
    Element {
        gen: u64,
        text: String,
    },
    Forward(String),
    /// A crash already decided (the gate is closed): send the event.
    Crashed {
        gen: u64,
        why: String,
    },
}

impl Note {
    /// Which notes a full queue gives up first (PR #32 review m6): `Some(0)` only logs, `Some(1)`
    /// is a signal the page repeats or the domain outlives (a beat, a stale or channel value), and
    /// `None` is never dropped: a decided crash (its gate is already shut, so a lost note would
    /// freeze the overlay unannounced) and a ready (at most one queued per build, below).
    fn rank(&self) -> Option<u8> {
        match self {
            Note::Crashed { .. }
            | Note::Page {
                signal: PageSignal::Ready,
                ..
            } => None,
            Note::Page {
                signal: PageSignal::Alive | PageSignal::ScoreStale(_) | PageSignal::Channel(_),
                ..
            } => Some(1),
            Note::Page { .. } | Note::RendererGone { .. } | Note::Element { .. } | Note::Forward(_) => {
                Some(0)
            }
        }
    }

    fn ready_of(&self) -> Option<u64> {
        match self {
            Note::Page {
                build,
                signal: PageSignal::Ready,
                ..
            } => Some(*build),
            _ => None,
        }
    }
}

/// The dispatcher's queue: `NOTE_QUEUE_CAP` notes, where a full queue drops a log-only note first,
/// then a repeatable signal, and never a crash or a ready. A ready of a build that already has one
/// queued is dropped (the queued one stands for it), so the notes kept past the cap are at most two
/// a build.
#[derive(Debug, Default)]
pub(crate) struct NoteQueue {
    q: VecDeque<Note>,
    dropped: u64,
}

impl NoteQueue {
    fn push(&mut self, note: Note) {
        if let Some(b) = note.ready_of() {
            if self.q.iter().any(|n| n.ready_of() == Some(b)) {
                self.dropped += 1;
                return;
            }
        }
        if self.q.len() >= NOTE_QUEUE_CAP {
            let incoming = note.rank();
            let victim = (0..=1u8)
                .filter(|r| incoming.is_none_or(|i| *r <= i))
                .find_map(|r| self.q.iter().position(|n| n.rank() == Some(r)));
            match victim {
                Some(i) => {
                    self.q.remove(i);
                    self.dropped += 1;
                }
                // Nothing as cheap as the newcomer: it is the one dropped.
                None if incoming.is_some() => {
                    self.dropped += 1;
                    return;
                }
                // Only crashes and readies queued, and this is one too: kept, past the cap.
                None => {}
            }
        }
        self.q.push_back(note);
    }

    fn pop(&mut self) -> Option<Note> {
        self.q.pop_front()
    }
}

#[derive(Default)]
struct Gate {
    /// The current build and its generation (`None` after `stop`).
    current: Option<(u64, u64)>,
    /// The build whose samples may reach the slot.
    open: Option<u64>,
    /// Builds reported crashed: their gate never reopens.
    crashed: BTreeSet<u64>,
}

/// One build's sample counts.
#[derive(Debug, Default)]
struct SampleStats {
    delivered: AtomicU64,
    dropped: AtomicU64,
    written: AtomicU64,
    refused: AtomicU64,
}

/// A build's sample counts, as read by a test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Samples {
    /// Samples the appsink delivered.
    pub delivered: u64,
    /// Samples the gate dropped (before ready, after a retirement or a crash).
    pub dropped: u64,
    /// Samples written to the overlay slot.
    pub written: u64,
    /// Samples refused as not one 1920×1080 BGRA frame.
    pub refused: u64,
}

/// How a retired build's teardown ended, as the reaper's watch saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaperPath {
    /// `set_state(NULL)` returned within `REAPER_KILL_AFTER_MS`.
    TornDown { ms: u64 },
    /// It had not: these renderer pids were sent SIGKILL.
    Killed { pids: Vec<i32> },
}

/// What the adapter knows that the domain's events cannot say (owner ruling 2026-10-19 "B"): the
/// domain's own type, so the domain alone decides the in-place restart from it (C11b,
/// `CefHealth::restart_cause`). Read with `GraphicsRenderer::health`. A renderers' zygote's death
/// also leaves a live page painting from its orphaned renderer, out of reach of the guardrails and
/// the reaper; a leaked teardown is one whose `set_state(NULL)` had not returned
/// `TEARDOWN_LEAK_AFTER_MS` after the reaper's kill and still has not.
pub use domain::cef_health::CefHealth;

/// Test hook: a sample about to be written waits here until released (`CefGraphics::hold_next_write`).
#[cfg(feature = "test-hooks")]
#[derive(Debug, Default)]
pub struct WriteHold {
    /// 0 off, 1 armed, 2 holding a sample, 3 released.
    state: Mutex<u8>,
    cv: Condvar,
}

#[cfg(feature = "test-hooks")]
impl WriteHold {
    fn hold_here(&self) {
        let mut s = locked(&self.state);
        if *s != 1 {
            return;
        }
        *s = 2;
        self.cv.notify_all();
        let end = Instant::now() + Duration::from_secs(60);
        while *s == 2 && Instant::now() < end {
            s = self
                .cv
                .wait_timeout(s, Duration::from_millis(100))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *s = 0;
        self.cv.notify_all();
    }

    /// True once a sample is held (within `limit`).
    pub fn wait_holding(&self, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        let mut s = locked(&self.state);
        while *s != 2 {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            s = self
                .cv
                .wait_timeout(s, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Lets the held sample be written.
    pub fn release(&self) {
        let mut s = locked(&self.state);
        if *s == 2 {
            *s = 3;
        } else {
            *s = 0;
        }
        self.cv.notify_all();
    }
}

/// What the pipeline threads share with the adapter.
struct Shared {
    inst: u64,
    tx: Sender<Inbound>,
    frames: FrameStore,
    /// Swappable at any time (`with_log`; PR #32 review m7).
    log: Mutex<LogFn>,
    scrubber: Mutex<Scrubber>,
    gate: Mutex<Gate>,
    notes: Mutex<NoteQueue>,
    notes_cv: Condvar,
    retired: AtomicBool,
    rss_cap_mb: AtomicU64,
    /// Retired builds whose teardown has not returned.
    teardowns: AtomicUsize,
    /// Teardowns counted leaked (`TEARDOWN_LEAK_AFTER_MS` after the kill) and still not returned.
    leaked: AtomicUsize,
    /// Child polls run.
    polls: AtomicU64,
    /// Recent builds: their generation and sample counts.
    builds: Mutex<BTreeMap<u64, (u64, Arc<SampleStats>)>>,
    reports: Mutex<BTreeMap<u64, ReaperPath>>,
    last_guard: Mutex<Guarded>,
    /// The last child poll's number and PSS by kind, for the metrics' memory split (`memory`).
    memory: CefMemory,
    /// What the child poll and each ready list (`ProcTree::live` in production).
    tree: ProcTree,
    /// The process's CEF watch (`cef_watch`).
    watch: Arc<Mutex<CefWatch>>,
    #[cfg(feature = "test-hooks")]
    teardown_hold: Mutex<bool>,
    #[cfg(feature = "test-hooks")]
    teardown_cv: Condvar,
    #[cfg(feature = "test-hooks")]
    poll_hold: Mutex<bool>,
    #[cfg(feature = "test-hooks")]
    poll_cv: Condvar,
    #[cfg(feature = "test-hooks")]
    kill_after_ms: AtomicU64,
    #[cfg(feature = "test-hooks")]
    write_hold: Arc<WriteHold>,
    #[cfg(feature = "test-hooks")]
    js_signals: AtomicBool,
    /// Owed 82: retirements whose teardown is still to be held for good (`hold_next_teardowns`).
    #[cfg(feature = "test-hooks")]
    teardowns_to_hold: AtomicUsize,
}

impl Shared {
    fn log(&self, line: &str) {
        let clean = locked(&self.scrubber).scrub(line);
        let sink = Arc::clone(&*locked(&self.log));
        sink(&clean);
    }

    /// Queues a note for the dispatcher; never waits (`NoteQueue`'s policy when full).
    fn push(&self, note: Note) {
        locked(&self.notes).push(note);
        self.notes_cv.notify_one();
    }

    /// The reaper's wait before its kill.
    fn kill_after(&self) -> Duration {
        #[cfg(feature = "test-hooks")]
        return Duration::from_millis(self.kill_after_ms.load(Ordering::SeqCst));
        #[cfg(not(feature = "test-hooks"))]
        Duration::from_millis(REAPER_KILL_AFTER_MS)
    }

    /// The next note; `None` once the adapter is dropped and the queue is empty.
    fn next_note(&self) -> Option<Note> {
        let mut q = locked(&self.notes);
        loop {
            if let Some(n) = q.pop() {
                return Some(n);
            }
            if self.retired.load(Ordering::SeqCst) {
                return None;
            }
            q = self
                .notes_cv
                .wait_timeout(q, Duration::from_millis(200))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Decides `build` crashed, once: its gate closes for good. True for the first decision.
    fn decide_crash(&self, build: u64) -> bool {
        let mut g = locked(&self.gate);
        if !g.crashed.insert(build) {
            return false;
        }
        if g.open == Some(build) {
            g.open = None;
        }
        // Builds only grow; keep the recent ones.
        while g.crashed.len() > 64 {
            g.crashed.pop_first();
        }
        true
    }

    fn is_current(&self, build: u64) -> bool {
        locked(&self.gate).current.is_some_and(|(b, _)| b == build)
    }

    /// Only from this adapter's own threads (the dispatcher): it waits for room.
    fn send(&self, e: Event) {
        let _ = self.tx.send(Inbound::Event(e));
    }

    fn report(&self, build: u64, path: ReaperPath) {
        let mut r = locked(&self.reports);
        r.insert(build, path);
        while r.len() > 32 {
            r.pop_first();
        }
    }

    fn builds_of(&self, gen: u64) -> Vec<u64> {
        locked(&self.builds)
            .iter()
            .filter(|(_, (g, _))| *g == gen)
            .map(|(b, _)| *b)
            .collect()
    }

    /// The newest build's generation.
    fn newest_gen(&self) -> Option<u64> {
        locked(&self.builds).last_key_value().map(|(_, (gen, _))| *gen)
    }

    /// Test hook (`hold_polls`): the child poll waits here before each listing while the hold is on
    /// (at most 60 s, or until the adapter is dropped).
    #[cfg(feature = "test-hooks")]
    fn wait_poll_hold(&self) {
        let end = Instant::now() + Duration::from_secs(60);
        let mut held = locked(&self.poll_hold);
        while *held && Instant::now() < end && !self.retired.load(Ordering::SeqCst) {
            held = self
                .poll_cv
                .wait_timeout(held, Duration::from_millis(100))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Test hook (`hold_next_teardowns`): takes one held teardown from the budget, if any is left.
    /// `Some(n)`: this teardown is held, and `n` more will be.
    #[cfg(feature = "test-hooks")]
    fn take_teardown_hold(&self) -> Option<usize> {
        self.teardowns_to_hold
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .ok()
            .map(|was| was - 1)
    }
}

#[derive(Default)]
struct LifeState {
    stop: bool,
    took: Option<Duration>,
    leaked: bool,
}

/// One build's start, retirement and teardown, shared by its thread and its reaper watch.
#[derive(Default)]
pub(crate) struct Life {
    state: Mutex<LifeState>,
    cv: Condvar,
}

impl Life {
    fn stop(&self) {
        locked(&self.state).stop = true;
        self.cv.notify_all();
    }

    fn wait_stop(&self) {
        let mut s = locked(&self.state);
        while !s.stop {
            s = self.cv.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The teardown returned. True if it had been counted leaked: `leaked` is counted down, under
    /// the lock `leaked_after` counts it up under.
    pub(crate) fn finish(&self, took: Duration, leaked: &AtomicUsize) -> bool {
        let mut s = locked(&self.state);
        s.took = Some(took);
        let was = std::mem::take(&mut s.leaked);
        if was {
            leaked.fetch_sub(1, Ordering::SeqCst);
        }
        drop(s);
        self.cv.notify_all();
        was
    }

    /// Waits up to `limit` for the teardown; if it has not returned by then, marks it leaked and
    /// counts it in `leaked`, under the lock `finish` takes. Returns the new count.
    pub(crate) fn leaked_after(&self, limit: Duration, leaked: &AtomicUsize) -> Option<usize> {
        let _ = self.wait_done(limit);
        let mut s = locked(&self.state);
        if s.took.is_some() {
            return None;
        }
        s.leaked = true;
        Some(leaked.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// How long the teardown took, if it ended within `limit` of this call.
    fn wait_done(&self, limit: Duration) -> Option<Duration> {
        let end = Instant::now() + limit;
        let mut s = locked(&self.state);
        loop {
            if let Some(took) = s.took {
                return Some(took);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            s = self
                .cv
                .wait_timeout(s, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// One build's pipeline.
struct Live {
    gen: u64,
    build: u64,
    pipeline: gst::Pipeline,
    life: Arc<Life>,
    running: bool,
}

/// The live `GraphicsRenderer` (C11).
pub struct CefGraphics {
    shared: Arc<Shared>,
    fps: u32,
    current: Option<Live>,
    builds: u64,
    /// The dispatcher and the child poll run (they start at the first `restart`, after `with_log`).
    threads: bool,
    /// This adapter's hold on the process's profile directory (dropped after `drop`'s body).
    _lease: ProfileLease,
}

impl CefGraphics {
    /// `fps` is the programme's (`max-video-framerate`); `rss_cap_mb` is `Thresholds::cef_rss_cap_mb`.
    /// It loads cefsrc's plugin and installs the process's log handler, so build it before the loop.
    pub fn new(frames: FrameStore, tx: Sender<Inbound>, fps: u32, rss_cap_mb: u64) -> Self {
        static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
        let inst = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
        let lease = profile_lease();
        let shared = Arc::new(Shared {
            inst,
            tx,
            frames,
            log: Mutex::new(stderr_log()),
            scrubber: Mutex::new(Scrubber::default()),
            gate: Mutex::new(Gate::default()),
            notes: Mutex::new(NoteQueue::default()),
            notes_cv: Condvar::new(),
            retired: AtomicBool::new(false),
            rss_cap_mb: AtomicU64::new(rss_cap_mb),
            teardowns: AtomicUsize::new(0),
            leaked: AtomicUsize::new(0),
            polls: AtomicU64::new(0),
            builds: Mutex::new(BTreeMap::new()),
            reports: Mutex::new(BTreeMap::new()),
            last_guard: Mutex::new(Guarded::default()),
            memory: CefMemory::default(),
            tree: ProcTree::live(),
            watch: cef_watch(),
            #[cfg(feature = "test-hooks")]
            teardown_hold: Mutex::new(false),
            #[cfg(feature = "test-hooks")]
            teardown_cv: Condvar::new(),
            #[cfg(feature = "test-hooks")]
            poll_hold: Mutex::new(false),
            #[cfg(feature = "test-hooks")]
            poll_cv: Condvar::new(),
            #[cfg(feature = "test-hooks")]
            kill_after_ms: AtomicU64::new(REAPER_KILL_AFTER_MS),
            #[cfg(feature = "test-hooks")]
            write_hold: Arc::default(),
            #[cfg(feature = "test-hooks")]
            js_signals: AtomicBool::new(false),
            #[cfg(feature = "test-hooks")]
            teardowns_to_hold: AtomicUsize::new(0),
        });
        let g = Self {
            shared,
            fps: fps.max(1),
            current: None,
            builds: 0,
            threads: false,
            _lease: lease,
        };
        g.preload();
        g
    }

    fn preload(&self) {
        if let Err(e) = gst::init() {
            self.shared.log(&format!("gstreamer did not initialise: {e}"));
            return;
        }
        let loaded = gst::ElementFactory::find("cefsrc").is_some_and(|f| f.load().is_ok());
        if !loaded {
            self.shared
                .log("the cefsrc element did not load; every build will fail");
        }
        install_log_handler();
        if std::env::var_os("GST_CEF_LOG_SEVERITY").is_some() {
            self.shared.log(
                "WARN: GST_CEF_LOG_SEVERITY is set and overrides log-severity=disable: CEF then writes \
                 every console line with its source URL, job token included, to chrome_debug.log (F-013)",
            );
        }
    }

    /// Starts the dispatcher and the child poll, and registers with the log handler. Separate from
    /// `new` so `with_log` can swap the sink first.
    fn ensure_threads(&mut self) {
        if self.threads {
            return;
        }
        self.threads = true;
        locked(&router().routes).insert(self.shared.inst, Arc::clone(&self.shared));
        let shared = Arc::clone(&self.shared);
        if thread::Builder::new()
            .name("cef-dispatch".to_owned())
            .spawn(move || dispatch(&shared))
            .is_err()
        {
            self.shared
                .log("no thread for the page-line dispatcher; page signals are lost");
        }
        let shared = Arc::clone(&self.shared);
        if thread::Builder::new()
            .name("cef-children".to_owned())
            .spawn(move || poll_children(&shared))
            .is_err()
        {
            self.shared
                .log("no thread for the CEF child poll; nice, OOM order and the cap are off");
        }
    }

    /// Routes this adapter's log lines (each one scrubbed of every secret it knows) to `log` instead
    /// of stderr, from the next line on. C12 wires its log sink here.
    pub fn with_log(self, log: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        *locked(&self.shared.log) = log;
        self
    }

    /// Builds attempted so far (each `restart`).
    pub fn builds(&self) -> u64 {
        self.builds
    }

    /// Retired builds whose teardown has not returned yet.
    pub fn teardowns_in_flight(&self) -> usize {
        self.shared.teardowns.load(Ordering::SeqCst)
    }

    /// Notes the dispatcher's queue dropped (`NoteQueue`).
    pub fn notes_dropped(&self) -> u64 {
        locked(&self.shared.notes).dropped
    }

    /// The process's private CEF profile directory (`None`: not made yet, or it could not be made,
    /// and no build was made either).
    pub fn profile_dir(&self) -> Option<std::path::PathBuf> {
        profile().path.clone()
    }

    /// Child polls run so far.
    pub fn polls(&self) -> u64 {
        self.shared.polls.load(Ordering::SeqCst)
    }

    /// What the last child poll did.
    pub fn last_guard(&self) -> Guarded {
        *locked(&self.shared.last_guard)
    }

    /// The child polls' PSS by kind, for the metrics' memory split: a handle that outlives `self`'s
    /// move into the ports, read once a beat.
    pub fn memory(&self) -> CefMemory {
        self.shared.memory.clone()
    }

    /// The sample counts of generation `gen`'s builds, summed.
    pub fn samples(&self, gen: u64) -> Samples {
        let builds = locked(&self.shared.builds);
        let mut s = Samples::default();
        for (_, (g, st)) in builds.iter() {
            if *g == gen {
                s.delivered += st.delivered.load(Ordering::SeqCst);
                s.dropped += st.dropped.load(Ordering::SeqCst);
                s.written += st.written.load(Ordering::SeqCst);
                s.refused += st.refused.load(Ordering::SeqCst);
            }
        }
        s
    }

    /// The live renderer pids of generation `gen` (after a claim pass).
    pub fn renderer_pids(&self, gen: u64) -> Vec<i32> {
        let kids = cef_children();
        claim(&kids);
        let mut pids: Vec<i32> = self
            .shared
            .builds_of(gen)
            .into_iter()
            .flat_map(|b| renderers_of(&kids, self.shared.inst, b))
            .collect();
        pids.sort_unstable();
        pids
    }

    /// How the reaper saw generation `gen`'s teardown end, once it has.
    pub fn reaper_path(&self, gen: u64) -> Option<ReaperPath> {
        let builds = self.shared.builds_of(gen);
        let reports = locked(&self.shared.reports);
        builds.iter().rev().find_map(|b| reports.get(b).cloned())
    }

    /// Test hook: the memory cap, changed while a page runs.
    #[cfg(feature = "test-hooks")]
    pub fn set_rss_cap_mb(&self, mb: u64) {
        self.shared.rss_cap_mb.store(mb, Ordering::SeqCst);
    }

    /// Test hook: while on, every retired build's teardown waits before its `set_state(NULL)` (at
    /// most 60 s), standing in for a teardown that does not return, so a retired page keeps
    /// painting and the gate must drop it. Off lets the held ones go.
    #[cfg(feature = "test-hooks")]
    pub fn hold_teardowns(&self, on: bool) {
        *locked(&self.shared.teardown_hold) = on;
        self.shared.teardown_cv.notify_all();
    }

    /// Test hook (Owed 82; owner option A, 2026-10-24): the next `n` retired builds' teardowns never
    /// return, as a teardown stuck in `set_state(NULL)` does not: each build's thread parks for good
    /// before its `set_state(NULL)`, so the reaper's kill and its leak count run exactly as for a real
    /// one, and the process carries the leak until it exits (C14 case 16). Unlike `hold_teardowns`,
    /// no cap lets it go. A second call replaces what is left of the first; 0 holds none.
    ///
    /// The held build stays PLAYING, so the reaper's SIGKILL makes cefsrc reload its page under the
    /// same client id, and nothing kills it again: each held teardown leaves a live overlay page for
    /// the rest of the process (at `CEF_NICE`, its page's polls running, its ready and alive lines an
    /// old gen's, its frames off air), as a real leak leaves its renderer. The unit tests have no
    /// renderer, so they witness only that the hold comes before `set_state(NULL)` (the pipeline is
    /// still PLAYING at the leak count); that the kill reaches a renderer, at least one pid in each
    /// held teardown's reaper line, is C14 case 16's witness on the real image.
    #[cfg(feature = "test-hooks")]
    pub fn hold_next_teardowns(&self, n: usize) {
        self.shared.teardowns_to_hold.store(n, Ordering::SeqCst);
        self.shared.log(&format!(
            "WARN test hook (Owed 82): the next {n} CEF teardowns never return"
        ));
    }

    /// Test hook: while on, the child poll waits before each listing (at most 60 s), so the only
    /// listings CEF's watch gets are the ones at each ready. Set it before the first `restart`, or
    /// that restart's first poll has already listed. Off lets the poll go.
    #[cfg(feature = "test-hooks")]
    pub fn hold_polls(&self, on: bool) {
        *locked(&self.shared.poll_hold) = on;
        self.shared.poll_cv.notify_all();
    }

    /// Test hook: the reaper's wait before its kill (default `REAPER_KILL_AFTER_MS`).
    #[cfg(feature = "test-hooks")]
    pub fn set_reaper_kill_after(&self, after: Duration) {
        self.shared
            .kill_after_ms
            .store(after.as_millis() as u64, Ordering::SeqCst);
    }

    /// Test hook: the next sample about to be written (its gate open, converted) waits, holding
    /// the gate, until the returned handle releases it.
    #[cfg(feature = "test-hooks")]
    pub fn hold_next_write(&self) -> Arc<WriteHold> {
        let h = Arc::clone(&self.shared.write_hold);
        *locked(&h.state) = 1;
        h
    }

    /// Test hook: later builds use cefsrc's js-signal mode, F-001's hang: a page that never calls
    /// `gstSendMsg` holds `start()`, and so `set_state` either way, for good.
    #[cfg(feature = "test-hooks")]
    pub fn js_signal_builds(&self, on: bool) {
        self.shared.js_signals.store(on, Ordering::SeqCst);
    }

    /// Test hook: the current pipeline, to read how its elements were set.
    #[cfg(feature = "test-hooks")]
    pub fn pipeline(&self) -> Option<gst::Pipeline> {
        self.current.as_ref().map(|l| l.pipeline.clone())
    }

    /// Test hook: the lines the process's log handler prints (scrubbed) go to `sink` instead of
    /// stderr; `None` restores stderr.
    #[cfg(feature = "test-hooks")]
    pub fn forward_gst_lines(sink: Option<LogFn>) {
        *locked(&router().forward) = sink;
    }

    fn learn(&self, url: &str) {
        let secrets = secrets_of(url);
        let mut own = locked(&self.shared.scrubber);
        let mut all = locked(&router().scrubber);
        for s in secrets {
            own.add(s);
            all.add(s);
        }
    }

    /// Closes the gate and tells the current build's thread to tear its pipeline down, with the
    /// reaper's watch behind it. Returns at once.
    fn retire_current(&mut self) {
        {
            let mut g = locked(&self.shared.gate);
            g.open = None;
            g.current = None;
        }
        if let Some(live) = self.current.take() {
            if live.running {
                // Counted before the flag is set: the build's thread counts down once it sees it.
                self.shared.teardowns.fetch_add(1, Ordering::SeqCst);
                live.life.stop();
                let shared = Arc::clone(&self.shared);
                let life = Arc::clone(&live.life);
                let (build, gen) = (live.build, live.gen);
                if thread::Builder::new()
                    .name("cef-reaper".to_owned())
                    .spawn(move || reap(&shared, &life, build, gen))
                    .is_err()
                {
                    self.shared.log(&format!(
                        "gen={gen}: no thread for the reaper's watch; no kill fallback"
                    ));
                }
            }
        }
    }

    fn build(&self, gen: u64, build: u64, url: &str) -> Result<Live, String> {
        gst::init().map_err(|e| format!("gstreamer did not initialise: {e}"))?;
        // No private profile directory, no build (N4): CEF's default is under HOME.
        let profile = profile_ensure(|l| self.shared.log(l))?
            .to_str()
            .ok_or("the CEF profile directory's path is not UTF-8")?
            .to_owned();
        let pipeline = gst::Pipeline::with_name(&format!("graphics-{gen}"));
        let src = gst::ElementFactory::make("cefsrc")
            .name(element_name(self.shared.inst, build, gen))
            .build()
            .map_err(|_| "the cefsrc element is missing".to_owned())?;
        src.set_property("url", url);
        #[cfg(feature = "test-hooks")]
        let js = self.shared.js_signals.load(Ordering::SeqCst);
        #[cfg(not(feature = "test-hooks"))]
        let js = false;
        // F-001: the js-signal mode holds start() and set_state(NULL); readiness is the console line.
        src.set_property("listen-for-js-signals", js);
        src.set_property("max-video-framerate", gst::Fraction::new(self.fps as i32, 1));
        let env_flags = std::env::var("GST_CEF_CHROME_EXTRA_FLAGS").ok();
        src.set_property("chrome-extra-flags", extra_flags(env_flags.as_deref()));
        src.set_property_from_str("log-severity", CEF_LOG_SEVERITY);
        // Global, read at CEF's start (from the first element): the whole profile goes there.
        src.set_property("cef-cache-location", profile.as_str());
        let (w, h) = OVERLAY_SIZE;
        let caps = gst::ElementFactory::make("capsfilter")
            .name("caps")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "BGRA")
                    .field("width", w as i32)
                    .field("height", h as i32)
                    .build(),
            )
            .build()
            .map_err(|e| format!("graphics capsfilter: {e}"))?;
        let sink = gst_app::AppSink::builder()
            .name("sink")
            .max_buffers(1)
            .drop(true)
            .sync(false)
            .build();
        pipeline
            .add_many([&src, &caps, sink.upcast_ref()])
            .map_err(|e| format!("graphics: {e}"))?;
        gst::Element::link_many([&src, &caps, sink.upcast_ref()])
            .map_err(|e| format!("graphics link: {e}"))?;
        let stats = Arc::new(SampleStats::default());
        {
            let mut builds = locked(&self.shared.builds);
            builds.insert(build, (gen, Arc::clone(&stats)));
            while builds.len() > 32 {
                builds.pop_first();
            }
        }
        self.wire_sink(&sink, build, stats);
        self.sync_handler(&pipeline, build, gen)?;
        Ok(Live {
            gen,
            build,
            pipeline,
            life: Arc::new(Life::default()),
            running: false,
        })
    }

    fn wire_sink(&self, sink: &gst_app::AppSink, build: u64, stats: Arc<SampleStats>) {
        let shared = Arc::clone(&self.shared);
        let refused_logged = AtomicBool::new(false);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    stats.delivered.fetch_add(1, Ordering::SeqCst);
                    // One check, held through the conversion and the write: a gate that closes
                    // (restart, stop, a crash) waits out at most one conversion, and no sample of
                    // a closed build is written after it.
                    let gate = locked(&shared.gate);
                    if gate.open != Some(build) {
                        drop(gate);
                        stats.dropped.fetch_add(1, Ordering::SeqCst);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    let Some(straight) = straight_alpha(&sample) else {
                        drop(gate);
                        stats.refused.fetch_add(1, Ordering::SeqCst);
                        if !refused_logged.swap(true, Ordering::SeqCst) {
                            shared.log("a CEF sample is not one 1920x1080 BGRA frame; refused");
                        }
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    #[cfg(feature = "test-hooks")]
                    shared.write_hold.hold_here();
                    shared.frames.set_overlay(straight);
                    // Counted under the gate too: a stop that returns has seen every write
                    // counted (the confirm pass: counted after the unlock, a stop between the two
                    // read one write short).
                    stats.written.fetch_add(1, Ordering::SeqCst);
                    drop(gate);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    /// Runs on the posting thread: an error decides the build's crash there and then, so no sample
    /// after it reaches the slot. Every message is dropped here, so the bus never fills.
    fn sync_handler(&self, pipeline: &gst::Pipeline, build: u64, gen: u64) -> Result<(), String> {
        let bus = pipeline.bus().ok_or("graphics pipeline has no bus")?;
        let shared = Arc::clone(&self.shared);
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(e) => {
                    let why = format!("error: {} ({})", e.error(), e.debug().as_deref().unwrap_or(""));
                    if shared.decide_crash(build) {
                        shared.push(Note::Crashed { gen, why });
                    }
                }
                gst::MessageView::Warning(w) => shared.push(Note::Element {
                    gen,
                    text: format!("warning: {} ({})", w.error(), w.debug().as_deref().unwrap_or("")),
                }),
                _ => {}
            }
            gst::BusSyncReply::Drop
        });
        Ok(())
    }

    /// Hands the build to a thread of its own (`run_build`). With no thread, the build is a crash.
    fn start(&mut self, live: &mut Live) {
        let shared = Arc::clone(&self.shared);
        let pipeline = live.pipeline.clone();
        let life = Arc::clone(&live.life);
        let (build, gen) = (live.build, live.gen);
        let spawned = thread::Builder::new()
            .name("cef-build".to_owned())
            .spawn(move || run_build(&shared, &pipeline, &life, build, gen));
        match spawned {
            Ok(_) => live.running = true,
            Err(_) => {
                if self.shared.decide_crash(build) {
                    self.shared.push(Note::Crashed {
                        gen,
                        why: "no thread to start it".to_owned(),
                    });
                }
            }
        }
    }
}

/// One build's thread: the start, the wait for its retirement, then the teardown, counted in
/// `teardowns_in_flight` until it returns. A start that fails posts its error on the bus.
fn run_build(shared: &Shared, pipeline: &gst::Pipeline, life: &Life, build: u64, gen: u64) {
    if pipeline.set_state(gst::State::Playing).is_err() && shared.decide_crash(build) {
        shared.push(Note::Crashed {
            gen,
            why: "the pipeline did not start".to_owned(),
        });
    }
    life.wait_stop();
    #[cfg(feature = "test-hooks")]
    if let Some(left) = shared.take_teardown_hold() {
        shared.log(&format!(
            "WARN test hook (Owed 82) gen={gen}: this CEF teardown never returns ({left} more to hold)"
        ));
        park_for_good();
    }
    #[cfg(feature = "test-hooks")]
    {
        let end = Instant::now() + Duration::from_secs(60);
        let mut held = locked(&shared.teardown_hold);
        while *held && Instant::now() < end {
            held = shared
                .teardown_cv
                .wait_timeout(held, Duration::from_millis(100))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
    let t0 = Instant::now();
    if pipeline.set_state(gst::State::Null).is_err() {
        shared.log(&format!("gen={gen}: teardown to NULL failed"));
    }
    let took = t0.elapsed();
    if life.finish(took, &shared.leaked) {
        shared.log(&format!(
            "gen={gen}: a teardown counted leaked returned after all ({} ms)",
            took.as_millis()
        ));
    }
    shared.teardowns.fetch_sub(1, Ordering::SeqCst);
}

/// Test hook (`hold_next_teardowns`): the calling thread never returns, and never runs again.
#[cfg(feature = "test-hooks")]
fn park_for_good() -> ! {
    loop {
        thread::park();
    }
}

/// The reaper's watch on one retired build: the kill fallback after `REAPER_KILL_AFTER_MS`, then
/// the leak count `TEARDOWN_LEAK_AFTER_MS` after the kill.
fn reap(shared: &Shared, life: &Life, build: u64, gen: u64) {
    if let Some(took) = life.wait_done(shared.kill_after()) {
        shared.report(
            build,
            ReaperPath::TornDown {
                ms: took.as_millis() as u64,
            },
        );
        return;
    }
    let kids = cef_children();
    claim(&kids);
    let pids = renderers_of(&kids, shared.inst, build);
    let mut killed = Vec::new();
    for &pid in &pids {
        let sent = rustix::process::Pid::from_raw(pid)
            .is_some_and(|p| rustix::process::kill_process(p, rustix::process::Signal::KILL).is_ok());
        if sent {
            killed.push(pid);
        }
    }
    shared.log(&format!(
        "WARN gen={gen}: teardown has not returned after {} ms; SIGKILL sent to renderer pids \
         {killed:?} (of {pids:?})",
        shared.kill_after().as_millis()
    ));
    shared.report(build, ReaperPath::Killed { pids: killed });
    if let Some(n) = life.leaked_after(Duration::from_millis(TEARDOWN_LEAK_AFTER_MS), &shared.leaked) {
        shared.log(&format!(
            "WARN gen={gen}: teardown still has not returned {TEARDOWN_LEAK_AFTER_MS} ms after the \
             kill: its thread, pipeline and renderer stay until the process restarts ({n} leaked; \
             Owed 55)"
        ));
    }
}

/// The dispatcher: relays page signals and crashes in order, and logs, off CEF's UI thread.
fn dispatch(shared: &Shared) {
    let mut page_lines: HashMap<u64, u32> = HashMap::new();
    let mut storms: HashMap<u64, u64> = HashMap::new();
    while let Some(note) = shared.next_note() {
        match note {
            Note::Page {
                build,
                gen,
                signal,
                text,
            } => match &signal {
                PageSignal::Ready => {
                    watch_ready(shared);
                    let opened = {
                        let mut g = locked(&shared.gate);
                        let current = g.current.is_some_and(|(b, _)| b == build);
                        let ok = current && !g.crashed.contains(&build);
                        if ok {
                            g.open = Some(build);
                        }
                        ok
                    };
                    shared.log(&format!(
                        "gen={gen}: ready{}",
                        if opened {
                            ""
                        } else {
                            " (retired or crashed: its frames stay off air)"
                        }
                    ));
                    shared.send(Event::CefReady { gen });
                }
                PageSignal::Alive => shared.send(Event::CefAlive { gen }),
                PageSignal::ScoreStale(v) if shared.is_current(build) => {
                    shared.log(&format!("gen={gen}: score stale={v}"));
                    shared.send(Event::ScoreStale(*v));
                }
                PageSignal::Channel(c) if shared.is_current(build) => {
                    shared.log(&format!("gen={gen}: phase channel {c:?}"));
                    shared.send(Event::PhaseChannel(*c));
                }
                PageSignal::ScoreStale(_) | PageSignal::Channel(_) => {
                    shared.log(&format!("gen={gen}: `{text}` from a retired page; not relayed"));
                }
                PageSignal::Unknown | PageSignal::Other => {
                    let n = page_lines.entry(build).or_insert(0);
                    *n += 1;
                    if *n <= PAGE_LINES_PER_BUILD {
                        let what = if signal == PageSignal::Unknown {
                            "WARN: not a page signal, ignored"
                        } else {
                            "page console"
                        };
                        shared.log(&format!("gen={gen}: {what}: {}", clip(&text)));
                    }
                }
            },
            // A renderer exit after its build's crash was decided (the log handler decides the
            // first, where it arrives): the storm after a zygote's death, counted.
            Note::RendererGone { build, gen } => {
                let n = storms.entry(build).or_insert(0);
                *n += 1;
                if (*n).is_multiple_of(STORM_LOG_EVERY) {
                    shared.log(&format!("gen={gen}: {n} more renderer exits"));
                }
            }
            Note::Element { gen, text } => shared.log(&format!("gen={gen}: cefsrc {}", clip(&text))),
            Note::Forward(line) => router().forward(&line),
            Note::Crashed { gen, why } => {
                shared.log(&format!("gen={gen}: crashed: {}", clip(&why)));
                shared.send(Event::CefCrashed { gen });
            }
        }
        if page_lines.len() > 64 {
            page_lines.clear();
        }
        if storms.len() > 64 {
            storms.clear();
        }
    }
}

/// At most 200 characters of a page's text.
fn clip(text: &str) -> String {
    match text.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_owned(),
    }
}

/// The child poll: guardrails, renderer claims and the memory cap, every `CHILD_POLL_MS`; between
/// two polls, the GPU watch's step every `GPU_WATCH_MS`, or sooner when its next listing is due
/// sooner (`gpu_step`, `gpu_sleep_us`; Owed 70 (a)).
fn poll_children(shared: &Shared) {
    let mut failures_logged = false;
    let mut ambiguity_logged = false;
    let mut unread_seen = 0;
    let mut next_poll = Instant::now();
    while !shared.retired.load(Ordering::SeqCst) {
        #[cfg(feature = "test-hooks")]
        shared.wait_poll_hold();
        if Instant::now() < next_poll {
            gpu_step(shared);
            thread::sleep(Duration::from_micros(gpu_sleep_us(shared)));
            continue;
        }
        let kids = (shared.tree.list)();
        if claim(&kids) && !ambiguity_logged {
            ambiguity_logged = true;
            shared.log(
                "WARN: more than one new renderer at once (a spare renderer?): the reaper may not \
                 tell one build's renderer from the next's (F-013)",
            );
        }
        let current = locked(&shared.gate).current;
        let owners: HashMap<i32, (u64, u64)> = {
            let c = claims();
            kids.iter()
                .filter_map(|k| Some((k.pid, *c.by_client.get(&k.client_id?)?)))
                .collect()
        };
        let mine = current.map(|(b, _)| (shared.inst, b));
        let g = guard(&kids, |k| {
            mine.is_some_and(|m| counts_against(k, owners.get(&k.pid).copied(), m))
        });
        if g.failures > 0 && !failures_logged {
            failures_logged = true;
            shared.log(&format!(
                "WARN: {} nice or oom_score_adj writes on CEF's children failed",
                g.failures
            ));
        }
        *locked(&shared.last_guard) = g;
        // This thread alone counts the polls, so this one is the count so far plus one.
        shared.memory.set(CefPoll {
            poll: shared.polls.load(Ordering::SeqCst) + 1,
            pss: g.pss_by_kind,
        });
        watch_poll(shared, &kids);
        log_unread_stats(shared, &mut unread_seen);
        let cap = shared.rss_cap_mb.load(Ordering::SeqCst);
        if let Some((build, gen)) = current {
            if g.pss_counted_mb > cap && shared.decide_crash(build) {
                shared.push(Note::Crashed {
                    gen,
                    why: format!(
                        "CEF's child processes use {} MB (PSS, of {} MB in all), over the {cap} MB cap",
                        g.pss_counted_mb, g.pss_mb
                    ),
                });
            }
        }
        shared.polls.fetch_add(1, Ordering::SeqCst);
        next_poll = Instant::now() + Duration::from_millis(CHILD_POLL_MS);
        thread::sleep(Duration::from_millis(GPU_WATCH_MS));
    }
    locked(&shared.watch).release_gpu(shared.inst);
}

impl GraphicsRenderer for CefGraphics {
    fn restart(&mut self, gen: u64, url: &str) {
        self.ensure_threads();
        self.learn(url);
        // The retiring build's renderer, if no poll has claimed it yet, is claimed before the new
        // build becomes the newest.
        claim(&cef_children());
        self.retire_current();
        let build = self.builds + 1;
        self.builds = build;
        match self.build(gen, build, url) {
            Ok(mut live) => {
                self.shared
                    .log(&format!("gen={gen}: loading {}", redacted_origin(url)));
                locked(&self.shared.gate).current = Some((build, gen));
                claims().newest = Some((self.shared.inst, build));
                self.start(&mut live);
                if live.running {
                    self.current = Some(live);
                }
            }
            Err(text) => {
                self.shared.log(&format!("gen={gen}: {text}"));
                if self.shared.decide_crash(build) {
                    self.shared.push(Note::Crashed { gen, why: text });
                }
            }
        }
    }

    fn stop(&mut self) {
        self.retire_current();
    }

    /// Counters only: the process's CEF watch (under its lock, held by a poll for a list's checks)
    /// and the leak gauge (C11b).
    fn health(&self) -> CefHealth {
        let watch = locked(&self.shared.watch);
        CefHealth {
            unrecoverable: watch.verdict,
            leaked_teardowns: self.shared.leaked.load(Ordering::SeqCst),
            gpu_process_deaths: watch.gpu.deaths,
        }
    }
}

impl Drop for CefGraphics {
    /// Returns at once (C11 amendment 2026-10-19: within 500 ms): the gate closes, the build's thread
    /// is told to tear its pipeline down with the reaper's watch behind it, the route is removed, and
    /// no thread is joined.
    fn drop(&mut self) {
        self.retire_current();
        self.shared.retired.store(true, Ordering::SeqCst);
        // Let go of the route after the lock: nothing may drop under it (the log handler takes it).
        let route = locked(&router().routes).remove(&self.shared.inst);
        drop(route);
        self.shared.notes_cv.notify_all();
    }
}

impl fmt::Debug for CefGraphics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CefGraphics")
            .field("builds", &self.builds)
            .field("gen", &self.current.as_ref().map(|l| l.gen))
            .field("fps", &self.fps)
            .field("rss_cap_mb", &self.shared.rss_cap_mb.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::ports::KindPss;

    #[test]
    fn page_signals_match_exactly() {
        let cases: &[(&str, PageSignal)] = &[
            ("seazn:ready", PageSignal::Ready),
            ("seazn:alive", PageSignal::Alive),
            ("seazn:score-stale=true", PageSignal::ScoreStale(true)),
            ("seazn:score-stale=false", PageSignal::ScoreStale(false)),
            (
                "seazn:phase-channel=loopback",
                PageSignal::Channel(PhaseChannel::Loopback),
            ),
            ("seazn:phase-channel=web", PageSignal::Channel(PhaseChannel::Web)),
            // Near misses: each is a seazn: line that is no signal.
            ("seazn:alive-x", PageSignal::Unknown),
            ("seazn:ready ", PageSignal::Unknown),
            ("seazn:READY", PageSignal::Unknown),
            ("seazn:score-stale=yes", PageSignal::Unknown),
            ("seazn:phase-channel=bogus", PageSignal::Unknown),
            ("seazn:phase-channel=Web", PageSignal::Unknown),
            ("seazn:probe=ok:{}", PageSignal::Unknown),
            // Not ours at all.
            (" seazn:ready", PageSignal::Other),
            ("ready", PageSignal::Other),
            ("", PageSignal::Other),
        ];
        for (line, want) in cases {
            assert_eq!(&page_signal(line), want, "{line:?}");
        }
        eprintln!("page lines checked: {}", cases.len());
    }

    #[test]
    fn a_console_text_splits_at_its_first_space() {
        let text = "file:///w/p.html?host=compositor&st=abc:87 seazn:score-stale=true";
        assert_eq!(
            split_console(text),
            (
                "file:///w/p.html?host=compositor&st=abc:87",
                "seazn:score-stale=true"
            )
        );
        assert_eq!(split_console(":0 two words"), (":0", "two words"));
        assert_eq!(split_console("nospace"), ("nospace", ""));
    }

    #[test]
    fn element_names_round_trip_and_refuse_other_names() {
        let name = element_name(3, 17, 9);
        assert_eq!(
            element_ids(&name),
            Some(Ids {
                inst: 3,
                build: 17,
                gen: 9
            })
        );
        let refused = [
            "cefsrc0",
            "cef-1-2",
            "cef-1-2-3-4",
            "cef-a-2-3",
            "graphics-1",
            "",
            "cef---",
        ];
        for n in refused {
            assert_eq!(element_ids(n), None, "{n:?}");
        }
        eprintln!("names refused: {}", refused.len());
    }

    #[test]
    fn a_url_yields_itself_and_its_token() {
        let url = "https://app.example/overlay/fixtures/f1?host=compositor&sid=s1&st=TOKEN123456#frag";
        assert_eq!(secrets_of(url), vec![url, "TOKEN123456"]);
        assert_eq!(secrets_of("file:///p.html"), vec!["file:///p.html"]);
    }

    #[test]
    fn the_spare_renderer_flag_leads_any_flags_from_the_environment() {
        assert_eq!(extra_flags(None), SPARE_RENDERER_OFF);
        assert_eq!(extra_flags(Some("")), SPARE_RENDERER_OFF);
        assert_eq!(
            extra_flags(Some("enable-logging")),
            format!("{SPARE_RENDERER_OFF},enable-logging")
        );
    }

    // Straight from premultiplied: c × 255 / a, rounded (the definition of premultiplied alpha). F-001
    // read the page's rgba(255,255,255,0.5) as (128,128,128,128).
    #[test]
    fn unpremultiply_restores_straight_colour() {
        let cases: &[([u8; 4], [u8; 4])] = &[
            ([128, 128, 128, 128], [255, 255, 255, 128]),
            ([64, 32, 0, 128], [128, 64, 0, 128]),
            ([1, 0, 1, 2], [128, 0, 128, 2]),
            ([0, 0, 255, 255], [0, 0, 255, 255]),
            ([0, 0, 0, 0], [0, 0, 0, 0]),
            // Invalid input (a channel above alpha) is capped, never wrapped.
            ([200, 10, 10, 100], [255, 26, 26, 100]),
        ];
        for (premul, straight) in cases {
            let mut px = *premul;
            unpremultiply_bgra(&mut px);
            assert_eq!(&px, straight, "{premul:?}");
        }
        // Several pixels at once, each converted on its own.
        let mut row: Vec<u8> = cases.iter().flat_map(|(p, _)| *p).collect();
        unpremultiply_bgra(&mut row);
        let want: Vec<u8> = cases.iter().flat_map(|(_, s)| *s).collect();
        assert_eq!(row, want);
        eprintln!("pixels checked: {}", cases.len());
    }

    #[test]
    fn a_sample_that_is_not_one_overlay_frame_is_refused() {
        gst::init().expect("gstreamer");
        let small = gst::Sample::builder()
            .buffer(&gst::Buffer::from_mut_slice(vec![0u8; 16]))
            .caps(&gst::Caps::builder("video/x-raw").build())
            .build();
        assert!(straight_alpha(&small).is_none());
        let (w, h) = OVERLAY_SIZE;
        let mut data = vec![0u8; (w * h * 4) as usize];
        data[..4].copy_from_slice(&[128, 128, 128, 128]);
        let mut buffer = gst::Buffer::from_mut_slice(data);
        buffer
            .get_mut()
            .expect("unique")
            .set_pts(gst::ClockTime::from_mseconds(40));
        let whole = gst::Sample::builder()
            .buffer(&buffer)
            .caps(&gst::Caps::builder("video/x-raw").build())
            .build();
        let out = straight_alpha(&whole).expect("a whole frame converts");
        let b = out.buffer().expect("buffer");
        assert_eq!(b.pts(), Some(gst::ClockTime::from_mseconds(40)));
        assert_eq!(&b.map_readable().expect("map")[..4], &[255, 255, 255, 128]);
    }

    #[test]
    fn proc_stat_is_read_after_the_last_parenthesis() {
        // A command name may hold spaces and parentheses (`comm` is the process's own choice).
        let stat = "4242 (gstcef sub) (x)) S 77 4242 1 0 -1 4194560 100 0 0 0 1 2 0 0 20 10 9 0 123 \
                    456 789";
        assert_eq!(parse_stat(stat), Some((77, 10)));
        assert_eq!(parse_stat("1 (init) S"), None);
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn a_chromium_command_line_splits_on_nul_and_on_spaces() {
        let raw = b"/opt/gstcefsubprocess --type=renderer --renderer-client-id=7 --x\0\0";
        let args = split_cmdline(raw);
        assert_eq!(flag_value(&args, "--type="), Some("renderer"));
        assert_eq!(flag_value(&args, "--renderer-client-id="), Some("7"));
        let raw = b"/opt/gstcefsubprocess\0--type=zygote\0";
        assert_eq!(flag_value(&split_cmdline(raw), "--type="), Some("zygote"));
        assert_eq!(
            flag_value(&split_cmdline(b"/usr/bin/python3\0x.py\0"), "--type="),
            None
        );
    }

    // The process's log handler, driven directly (no cefsrc needed): page lines of an element that is
    // not this adapter's are dropped, never printed, and what it prints is scrubbed.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn the_log_handler_drops_foreign_page_lines_and_scrubs_what_it_prints() {
        let secret = "unitTOKENmadeup0123456";
        locked(&router().scrubber).add(secret);
        let got: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&got);
        CefGraphics::forward_gst_lines(Some(Arc::new(move |l: &str| locked(&sink).push(l.to_owned()))));
        let unrouted = router().unrouted_console.load(Ordering::SeqCst);
        let page_line = format!("file:///p.html?st={secret}:1 seazn:ready");
        let info = gst::DebugLevel::Info;
        // Another element's, no element's, and a dropped instance's page lines.
        for object in [Some("cefsrc0"), None, Some("cef-999999-1-1")] {
            on_gst_log(
                CONSOLE_CATEGORY,
                info,
                "gstcefsrc.cc",
                "OnConsoleMessage",
                1,
                object,
                &page_line,
            );
        }
        // Another category's line, carrying the secret.
        let other = format!("pipeline url {secret}");
        on_gst_log(
            "GST_STATES",
            gst::DebugLevel::Debug,
            "gstbin.c",
            "f",
            2,
            Some("pipeline0"),
            &other,
        );
        // Other adapters' credentials, which this handler does not know by value: `redact` masks
        // them by their shape (Owed 45 and 47's lines: srtobject's parameters, rtmp2sink's location).
        let srt = "Opening SRT socket with parameters: passphrase=(string)srtPASSmadeup01, \
                   streamid=(string)play=srtSTREAMmadeup02";
        on_gst_log(
            "srtobject",
            gst::DebugLevel::Debug,
            "srt.c",
            "f",
            3,
            Some("srtsrc0"),
            srt,
        );
        let rtmp = "location=rtmps://live.example:443/live/rtmpKEYmadeup03";
        on_gst_log(
            "rtmp2sink",
            gst::DebugLevel::Debug,
            "rtmp.c",
            "f",
            4,
            Some("sink"),
            rtmp,
        );
        CefGraphics::forward_gst_lines(None);
        let got = locked(&got).clone();
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(
            got[0].contains("GST_STATES") && got[0].contains("pipeline url"),
            "{got:?}"
        );
        assert!(
            got[1].contains("srtobject") && got[2].contains("rtmp2sink"),
            "{got:?}"
        );
        for s in [secret, "srtPASSmadeup01", "srtSTREAMmadeup02", "rtmpKEYmadeup03"] {
            assert!(got.iter().all(|l| !l.contains(s)), "printed unscrubbed: {got:?}");
        }
        // The two lines of no adapter's element are counted; the dropped instance's is its own.
        assert_eq!(router().unrouted_console.load(Ordering::SeqCst) - unrouted, 2);
    }

    // PR #32 review G1: the renderer, which CEF replaces by itself, is the OOM killer's first CEF
    // victim; the zygote, whose death ends CEF for the process (Owed 56), its last; every one of them
    // still ahead of the compositor (0) and its encoder (−900). The renderer's 1000 is spec §6's.
    #[test]
    fn oom_score_adj_is_graded_renderer_first_zygote_last() {
        let kinds = ["renderer", "gpu-process", "utility", "zygote", "broker"];
        let renderer = oom_score_adj_for("renderer");
        assert_eq!(renderer, 1_000);
        let zygote = oom_score_adj_for("zygote");
        for kind in kinds {
            let adj = oom_score_adj_for(kind);
            // Above the compositor (0), and so above its encoder (−900) too.
            assert!(adj > 0, "{kind}: {adj}");
            assert!(adj <= renderer && adj >= zygote, "{kind}: {adj}");
            if kind != "renderer" {
                assert!(adj < renderer, "{kind} ties the renderer: {adj}");
            }
        }
        assert!(oom_score_adj_for("gpu-process") > zygote);
        eprintln!("kinds checked: {}", kinds.len());
    }

    fn kid(pid: i32, ppid: i32, kind: &str, client_id: Option<i64>) -> CefChild {
        CefChild {
            pid,
            ppid,
            kind: kind.to_owned(),
            client_id,
        }
    }

    // PR #32 review G2: a retiring or leaked build's renderer is not the current build's memory.
    #[test]
    fn another_builds_renderer_does_not_count_against_the_cap() {
        let current = (1, 7);
        let cases = [
            (kid(10, 1, "zygote", None), None, true),
            (kid(11, 10, "gpu-process", None), None, true),
            (kid(12, 10, "renderer", Some(5)), None, true),
            (kid(13, 10, "renderer", Some(6)), Some((1, 7)), true),
            (kid(14, 10, "renderer", Some(4)), Some((1, 6)), false),
            (kid(15, 10, "renderer", Some(3)), Some((2, 7)), false),
        ];
        for (k, owner, want) in &cases {
            assert_eq!(
                counts_against(k, *owner, current),
                *want,
                "{k:?} owned by {owner:?}"
            );
        }
        eprintln!("children checked: {}", cases.len());
    }

    // F-013 (CEF 139): two zygotes. The renderers' zygote's death fails every later build at once;
    // the GPU process's zygote's death turns the next GPU-process death into Chromium's FATAL "GPU
    // process isn't usable", an abort of the whole process (the PR #32 confirm pass's N1, probe CG).
    // Either is the verdict, decided once.
    #[test]
    fn any_dead_zygote_ends_cef_for_the_process() {
        let tree = |gpu_zygote: bool, renderer_zygote: bool| {
            let mut kids = vec![kid(1581, 1544, "utility", None)];
            if gpu_zygote {
                kids.push(kid(1556, 1544, "zygote", None));
                kids.push(kid(1578, 1556, "gpu-process", None));
            }
            if renderer_zygote {
                kids.push(kid(1557, 1544, "zygote", None));
                kids.push(kid(1589, 1557, "utility", None));
                kids.push(kid(1622, 1557, "renderer", Some(5)));
            }
            kids
        };
        let mut checked = 0;
        // Healthy, then the GPU zygote gone: decided, once, never again.
        let mut w = ZygoteWatch::default();
        assert!(!zygote_check(&mut w, &tree(true, true), 1544, |_| false));
        assert!(zygote_check(&mut w, &tree(false, true), 1544, |_| false));
        assert!(!zygote_check(&mut w, &tree(false, true), 1544, |_| false));
        assert!(!zygote_check(&mut w, &tree(false, false), 1544, |_| false));
        checked += 4;
        // The renderers' zygote alone, the GPU one alive: decided.
        let mut w = ZygoteWatch::default();
        assert!(!zygote_check(&mut w, &tree(true, true), 1544, |_| false));
        assert!(zygote_check(&mut w, &tree(true, false), 1544, |_| false));
        checked += 2;
        // A zygote missing from one list while its process lives (a failed cmdline read): no verdict,
        // for either zygote.
        for gone in [1556, 1557] {
            let mut w = ZygoteWatch::default();
            assert!(!zygote_check(&mut w, &tree(true, true), 1544, |_| false));
            let partial = tree(gone != 1556, gone != 1557);
            assert!(
                !zygote_check(&mut w, &partial, 1544, |p| p == gone),
                "zygote {gone}"
            );
            checked += 2;
        }
        // Every zygote gone before any renderer was seen.
        let mut w = ZygoteWatch::default();
        let early = vec![kid(1556, 1544, "zygote", None), kid(1557, 1544, "zygote", None)];
        assert!(!zygote_check(&mut w, &early, 1544, |_| false));
        assert!(zygote_check(&mut w, &[], 1544, |_| false));
        checked += 2;
        // Nothing seen yet (CEF not started): nothing decided.
        let mut w = ZygoteWatch::default();
        assert!(!zygote_check(&mut w, &[], 1544, |_| false));
        checked += 1;
        // A zygote's fresh fork reads `--type=zygote` (its parent a zygote, not this process):
        // seen once, then gone as the renderer or utility it became exits. No verdict (the likely
        // cause of 1 false verdict in 16 runs of the parent test process, the confirm pass).
        let mut w = ZygoteWatch::default();
        let mut forking = tree(true, true);
        forking.push(kid(1640, 1557, "zygote", None));
        assert!(!zygote_check(&mut w, &forking, 1544, |_| false));
        assert!(!zygote_check(&mut w, &tree(true, true), 1544, |_| false));
        checked += 2;
        eprintln!("polls checked: {checked}");
    }

    // Owed 61 (F-013): every GPU-process death of the process's life, counted once, never decayed;
    // a GPU process missing from a list while it lives (a failed cmdline read, or the final C11
    // review's G4: reparented to pid 1 once its zygote died) is no death until it is gone.
    #[test]
    fn every_gpu_process_death_is_counted_once_for_the_life_of_the_process() {
        let gpu = |pid: i32| kid(pid, 1556, "gpu-process", None);
        let others = || {
            vec![
                kid(1556, 1544, "zygote", None),
                kid(1622, 1557, "renderer", Some(5)),
            ]
        };
        let with = |pids: &[i32]| {
            let mut k = others();
            k.extend(pids.iter().map(|p| gpu(*p)));
            k
        };
        let dead = |_: i32| false;
        let mut polls = 0;
        // The empty case: none seen, nothing counted, however many polls.
        let mut w = GpuWatch::default();
        for _ in 0..3 {
            assert_eq!(gpu_check(&mut w, &others(), dead), 0);
            assert_eq!(gpu_check(&mut w, &[], dead), 0);
            polls += 2;
        }
        assert_eq!(w.deaths, 0);
        // Six deaths, each relaunched under a new pid: counted one by one to 6 (a count, not a rate:
        // the polls are what the clock is here, and none decays it).
        let mut w = GpuWatch::default();
        assert_eq!(gpu_check(&mut w, &with(&[2000]), dead), 0);
        polls += 1;
        for k in 1..=6 {
            let next = 2000 + k;
            assert_eq!(gpu_check(&mut w, &with(&[next]), dead), 1, "death {k}");
            assert_eq!(gpu_check(&mut w, &with(&[next]), dead), 0, "death {k} again");
            assert_eq!(w.deaths, k as usize);
            polls += 2;
        }
        // Gone with no relaunch yet, then the relaunch: one death.
        let mut w = GpuWatch::default();
        assert_eq!(gpu_check(&mut w, &with(&[3000]), dead), 0);
        assert_eq!(gpu_check(&mut w, &others(), dead), 1);
        assert_eq!(gpu_check(&mut w, &others(), dead), 0);
        assert_eq!(gpu_check(&mut w, &with(&[3001]), dead), 0);
        assert_eq!(w.deaths, 1);
        polls += 4;
        // Two gone in one poll: two.
        let mut w = GpuWatch::default();
        assert_eq!(gpu_check(&mut w, &with(&[4000, 4001]), dead), 0);
        assert_eq!(gpu_check(&mut w, &with(&[4002]), dead), 2);
        assert_eq!(w.deaths, 2);
        polls += 2;
        // Missing while alive (G4's orphan, or a failed read): no death, for as long as it lives;
        // then its death counts.
        let mut w = GpuWatch::default();
        assert_eq!(gpu_check(&mut w, &with(&[5000]), dead), 0);
        for _ in 0..3 {
            assert_eq!(gpu_check(&mut w, &others(), |p| p == 5000), 0);
            polls += 1;
        }
        assert_eq!(w.deaths, 0);
        assert_eq!(gpu_check(&mut w, &others(), dead), 1);
        assert_eq!(w.deaths, 1);
        polls += 2;
        // Another kind's death is no GPU-process death.
        let mut w = GpuWatch::default();
        assert_eq!(gpu_check(&mut w, &with(&[6000]), dead), 0);
        assert_eq!(gpu_check(&mut w, &[gpu(6000)], dead), 0);
        assert_eq!(w.deaths, 0);
        polls += 2;
        eprintln!("GPU polls checked: {polls}");
        assert_eq!(polls, 32);
    }

    /// A fake CEF process table on a fake clock (Owed 70 (a)): the two zygotes (children of
    /// `FAKE_ROOT`) and the GPU processes (children of the GPU zygote), each listed and alive from its
    /// birth until its death. The pids are above the kernel's PID_MAX_LIMIT (2^22), so a child poll's
    /// guardrails over them touch no real process.
    #[derive(Debug, Default)]
    struct FakeCef {
        /// The clock, in us.
        now_us: u64,
        /// (pid, born, died) for each GPU process, in us.
        gpus: Vec<(i32, u64, Option<u64>)>,
        /// (pid, from, in us): a GPU process reparented out of the tree from then on (G4): still
        /// alive while it lives, never listed.
        orphans: Vec<(i32, u64)>,
        /// Listings so far.
        listings: usize,
        /// What one listing costs on the fake clock, in us.
        list_cost_us: u64,
        /// What one listing costs in the listing thread's CPU, in us (`None`: its clock cost).
        list_cpu_us: Option<u64>,
        /// (listing, us): the listing with that number (from 1) takes that much longer on the clock,
        /// and no more CPU (a preempted or blocked listing).
        stalls: Vec<(usize, u64)>,
        /// The fake clock's time spent listing since `now_us` was set, in us.
        spent_us: u64,
        /// The CPU every listing so far has cost, in us (the fake `ProcTree::cpu`).
        cpu_us: u64,
        /// When each listing started, on the fake clock (us).
        listed_at_us: Vec<u64>,
    }

    const FAKE_ROOT: i32 = 90_000_000;
    const FAKE_GPU_ZYGOTE: i32 = FAKE_ROOT + 12;
    const FAKE_RENDERER_ZYGOTE: i32 = FAKE_ROOT + 13;

    impl FakeCef {
        /// The GPU processes' (pid, born, died) in ms.
        fn with(gpus: Vec<(i32, u64, Option<u64>)>) -> Arc<Mutex<Self>> {
            Self::with_us(
                gpus.into_iter()
                    .map(|(p, born, died)| (p, born * 1_000, died.map(|d| d * 1_000)))
                    .collect(),
            )
        }

        /// The GPU processes' (pid, born, died) in us.
        fn with_us(gpus: Vec<(i32, u64, Option<u64>)>) -> Arc<Mutex<Self>> {
            Arc::new(Mutex::new(Self {
                gpus,
                ..Self::default()
            }))
        }

        fn lives(&self, born: u64, died: Option<u64>) -> bool {
            born <= self.now_us && died.is_none_or(|d| self.now_us < d)
        }

        /// The fake clock: `now_us`, plus what the listings at `now_us` cost.
        fn clock_us(&self) -> u64 {
            self.now_us + self.spent_us
        }

        fn list(&mut self) -> Vec<CefChild> {
            self.listings += 1;
            self.listed_at_us.push(self.clock_us());
            let stall: u64 = self
                .stalls
                .iter()
                .filter(|(n, _)| *n == self.listings)
                .map(|(_, us)| us)
                .sum();
            self.spent_us += self.list_cost_us + stall;
            self.cpu_us += self.list_cpu_us.unwrap_or(self.list_cost_us);
            let mut kids = vec![
                kid(FAKE_GPU_ZYGOTE, FAKE_ROOT, "zygote", None),
                kid(FAKE_RENDERER_ZYGOTE, FAKE_ROOT, "zygote", None),
            ];
            for &(pid, born, died) in &self.gpus {
                let orphaned = self
                    .orphans
                    .iter()
                    .any(|&(p, from)| p == pid && from <= self.now_us);
                if self.lives(born, died) && !orphaned {
                    kids.push(kid(pid, FAKE_GPU_ZYGOTE, "gpu-process", None));
                }
            }
            kids
        }

        fn alive(&self, pid: i32) -> bool {
            pid == FAKE_GPU_ZYGOTE
                || pid == FAKE_RENDERER_ZYGOTE
                || self
                    .gpus
                    .iter()
                    .any(|&(p, born, died)| p == pid && self.lives(born, died))
        }

        /// The GPU-process deaths so far: the kernel's count, which the adapter's must equal.
        fn deaths(&self) -> usize {
            self.gpus
                .iter()
                .filter(|(_, _, d)| d.is_some_and(|d| d <= self.now_us))
                .count()
        }
    }

    fn fake_tree(cef: &Arc<Mutex<FakeCef>>) -> ProcTree {
        let clock: Box<dyn Fn() -> u64 + Send + Sync> = Box::new({
            let cef = Arc::clone(cef);
            move || locked(&cef).clock_us()
        });
        fake_tree_on(cef, clock)
    }

    /// The fake's tree on another clock (the wall clock, for the real poll thread).
    fn fake_tree_on(cef: &Arc<Mutex<FakeCef>>, clock: Box<dyn Fn() -> u64 + Send + Sync>) -> ProcTree {
        ProcTree {
            list: Box::new({
                let cef = Arc::clone(cef);
                move || locked(&cef).list()
            }),
            alive: Box::new({
                let cef = Arc::clone(cef);
                move |p| locked(&cef).alive(p)
            }),
            root: FAKE_ROOT,
            clock,
            cpu: Box::new({
                let cef = Arc::clone(cef);
                move || locked(&cef).cpu_us
            }),
        }
    }

    /// GPU process 0, born at 0 and killed at `first`; then each relaunch born `relaunch` after the
    /// death before it and killed `life` after its birth: `deaths` deaths in all, and the last
    /// relaunch lives on. In ms for `FakeCef::with`, in us for `FakeCef::with_us`.
    fn crash_loop(first: u64, relaunch: u64, life: u64, deaths: usize) -> Vec<(i32, u64, Option<u64>)> {
        let mut gpus = vec![(FAKE_ROOT + 100, 0, Some(first))];
        let mut died = first;
        for k in 1..=deaths {
            let born = died + relaunch;
            let end = (k < deaths).then_some(born + life);
            gpus.push((FAKE_ROOT + 100 + k as i32, born, end));
            died = end.unwrap_or(died);
        }
        gpus
    }

    /// The child poll thread's order of work on the fake clock, from `from` to `to` ms: a child
    /// poll's watch (`watch_poll`) every `CHILD_POLL_MS`, and the GPU watch's step (`gpu_step`) every
    /// `GPU_WATCH_MS` between (an instant's work takes no time here). `each` runs after every
    /// instant, with it.
    fn drive(shared: &Shared, cef: &Mutex<FakeCef>, from: u64, to: u64, mut each: impl FnMut(u64)) {
        let mut at = from;
        while at <= to {
            {
                let mut c = locked(cef);
                c.now_us = at * 1_000;
                c.spent_us = 0;
            }
            if at.is_multiple_of(CHILD_POLL_MS) {
                watch_poll(shared, &(shared.tree.list)());
            } else {
                gpu_step(shared);
            }
            each(at);
            at += GPU_WATCH_MS;
        }
    }

    /// The child poll thread's loop (`poll_children`) on the fake clock, from 0 to `to` us, the PR
    /// #55 review's I3: a child poll's watch once one is due (`CHILD_POLL_MS` after the end of the
    /// one before, then a `GPU_WATCH_MS` sleep), and otherwise the GPU watch's step and its sleep
    /// (`gpu_sleep_us`: up to `GPU_WATCH_MS`, less when a listing is due sooner). An instant's
    /// listings move the fake clock on by their cost. `each` runs after every instant, with it (us)
    /// and whether it polled. The polls' instants, in us.
    fn drive_paced(
        shared: &Shared,
        cef: &Mutex<FakeCef>,
        to: u64,
        mut each: impl FnMut(u64, bool),
    ) -> Vec<u64> {
        let (mut at, mut next_poll, mut polls) = (0, 0, Vec::new());
        while at <= to {
            {
                let mut c = locked(cef);
                c.now_us = at;
                c.spent_us = 0;
            }
            let poll = at >= next_poll;
            if poll {
                watch_poll(shared, &(shared.tree.list)());
                polls.push(at);
            } else {
                gpu_step(shared);
            }
            each(at, poll);
            let end = (shared.tree.clock)();
            if poll {
                next_poll = end + CHILD_POLL_MS * 1_000;
            }
            at = end
                + if poll {
                    GPU_WATCH_MS * 1_000
                } else {
                    gpu_sleep_us(shared)
                };
        }
        polls
    }

    /// The steps' listings in `listed` (us): every listing at none of the `polls` instants.
    fn step_listings(listed: &[u64], polls: &[u64]) -> Vec<u64> {
        listed.iter().copied().filter(|t| !polls.contains(t)).collect()
    }

    /// The adapter's log lines, as its `Shared` writes them from now on.
    fn capture_log(shared: &Shared) -> Arc<Mutex<Vec<String>>> {
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        *locked(&shared.log) = {
            let lines = Arc::clone(&lines);
            Arc::new(move |l: &str| locked(&lines).push(l.to_owned()))
        };
        lines
    }

    /// The GPU watch's wait lines among `lines`, as (listings, CPU each, last gap in tenths of a
    /// ms, held at the ceiling).
    fn wait_lines(lines: &[String]) -> Vec<(u64, u64, u64, bool)> {
        wait_parts(lines)
            .into_iter()
            .map(|w| (w.0, w.1, w.2, w.3))
            .collect()
    }

    /// The GPU watch's wait lines among `lines`, as `wait_lines` and then the wait's time on the
    /// clock (ms) and its share of a core (%), as the line prints them.
    fn wait_parts(lines: &[String]) -> Vec<(u64, u64, u64, bool, String, String)> {
        lines
            .iter()
            .filter_map(|l| {
                let rest = l.strip_prefix("INFO: the GPU watch's wait listed ")?;
                let (n, rest) = rest.split_once(" time(s) in ")?;
                let (span, rest) = rest.split_once(" ms at ")?;
                let (cpu, rest) = rest.split_once(" us of CPU each (")?;
                let (_, rest) = rest.split_once("clock), the last ")?;
                let (gap, rest) = rest.split_once(" ms apart: ")?;
                let (pct, _) = rest.split_once("% of a core")?;
                let tenths = gap.replace('.', "").parse().ok()?;
                let held = l.contains(&format!("held at the {GPU_LISTING_GAP_MAX_MS} ms ceiling"));
                let (n, cpu) = (n.parse().ok()?, cpu.parse().ok()?);
                Some((n, cpu, tenths, held, span.to_owned(), pct.to_owned()))
            })
            .collect()
    }

    // Owed 70 (a), F-026 (C14 case 15's fast loop, 9 legs): Chromium relaunched each killed GPU
    // process, and the loop killed it again 69 to 88 ms later, so its 5 kills fell between two child
    // polls; the 1 s poll counted 1 or 2 and nothing restarted. The GPU watch counts each death at the
    // step it happens, so the adapter's count is the kernel's at every step, and the restart cause
    // comes at exactly `cef_gpu_deaths_to_restart`, never at one fewer. C14's three loops: the fast
    // one (`CHROMIUM_GPU_DEATHS_ABORT` - 1 deaths), the threshold one (kills 2 s apart) and the
    // one-fewer one. Each starts between two polls, after a GPU process has lived for seconds. The
    // fast one also with a 1 ms listing, whose 20 ms pace (`GPU_WAIT_SHARE_PCT`) the ceiling holds to
    // `GPU_LISTING_GAP_MAX_MS` (here on a whole number of steps): each relaunch lives 60 ms, so each
    // is still listed and counted. (The shortest life is the next test's.)
    #[test]
    fn a_gpu_crash_loop_faster_than_a_poll_is_counted_death_by_death() {
        use domain::cef_health::RestartCause;
        use domain::config::{Thresholds, CHROMIUM_GPU_DEATHS_ABORT};
        let t = Thresholds::default();
        let n = t.cef_gpu_deaths_to_restart;
        // (case, first kill, relaunch, life, deaths) in ms, and a listing's cost in us.
        let cases = [
            ("fast", 2_500, 20, 60, CHROMIUM_GPU_DEATHS_ABORT - 1, 0),
            (
                "fast, a 1 ms listing",
                2_500,
                20,
                60,
                CHROMIUM_GPU_DEATHS_ABORT - 1,
                1_000,
            ),
            ("threshold", 2_500, 80, 2_000, n, 0),
            ("one fewer", 2_500, 80, 2_000, n - 1, 0),
        ];
        let (mut steps, mut at_one_fewer, mut at_threshold) = (0, 0, 0);
        for (case, first, relaunch, life, deaths, cost) in cases {
            let cef = FakeCef::with(crash_loop(first, relaunch, life, deaths));
            locked(&cef).list_cost_us = cost;
            let (tx, _rx) = crossbeam_channel::bounded(8);
            let g = unit_adapter(unit_shared(tx, fake_tree(&cef)));
            let shared = Arc::clone(&g.shared);
            let lines: Arc<Mutex<Vec<String>>> = Arc::default();
            *locked(&shared.log) = {
                let lines = Arc::clone(&lines);
                Arc::new(move |l: &str| locked(&lines).push(l.to_owned()))
            };
            let end = first + (relaunch + life) * deaths as u64 + 2 * CHILD_POLL_MS;
            drive(&shared, &cef, 0, end, |at| {
                let truth = locked(&cef).deaths();
                let h = g.health();
                assert_eq!(h.gpu_process_deaths, truth, "{case}, at {at} ms: {h:?}");
                let want = (truth >= n).then_some(RestartCause::GpuProcessDeaths { count: truth });
                assert_eq!(h.restart_cause(&t), want, "{case}, at {at} ms");
                at_one_fewer += usize::from(truth + 1 == n);
                at_threshold += usize::from(truth == n);
                steps += 1;
            });
            assert_eq!(g.health().gpu_process_deaths, deaths, "{case}");
            // One WARN line per death, each seen alone, with the running count.
            let warned: Vec<String> = locked(&lines)
                .iter()
                .filter(|l| l.contains("GPU process(es) gone"))
                .cloned()
                .collect();
            let want: Vec<String> = (1..=deaths)
                .map(|k| format!("WARN gen=0: 1 CEF GPU process(es) gone, {k} in this process"))
                .collect();
            let got: Vec<String> = warned
                .iter()
                .map(|l| l.split(';').next().unwrap_or("").to_owned())
                .collect();
            assert_eq!(got, want, "{case}");
        }
        eprintln!("steps checked: {steps}, at one fewer: {at_one_fewer}, at the threshold: {at_threshold}");
        assert!(steps > 0 && at_one_fewer > 0 && at_threshold > 0);
    }

    // Owed 70 (a), the PR #55 review's I3: C14's fast loop at F-026's shortest estimated relaunch
    // life (`GPU_RELAUNCH_LIFE_MIN_MS`), on the child poll thread's own loop (`drive_paced`: a step's
    // sleep ends when the next listing is due). GPU process 0 dies between two polls, after the
    // start's wait; each relaunch is born a while after the death before it (swept across a gap, so
    // births land all over the listings' phase) and dies `GPU_RELAUNCH_LIFE_MIN_MS` after its birth:
    // `CHROMIUM_GPU_DEATHS_ABORT` - 1 deaths. Every one is counted at the instant it happens, so the
    // adapter's count is the kernel's at every instant, and the restart cause comes at exactly
    // `cef_gpu_deaths_to_restart`. A listing's CPU: none (the `GPU_WATCH_MS` floor); 250 us, where
    // the share's pace meets the floor; 375 us, a 7.5 ms pace inside the ceiling; and 1 439 us, the
    // image's debug listing on the clock (run 40080601547; 632 us of CPU in run 40085739077, and 370
    // us on the release profile), whose 28.8 ms pace
    // `GPU_LISTING_GAP_MAX_MS` holds down. Each death's wait is summed up by one INFO line once its
    // relaunch is listed: its listings, their CPU, its gap, and whether the ceiling held it.
    #[test]
    fn a_crash_loop_at_the_shortest_relaunch_life_is_counted_death_by_death() {
        use domain::cef_health::RestartCause;
        use domain::config::{Thresholds, CHROMIUM_GPU_DEATHS_ABORT};
        let t = Thresholds::default();
        let n = t.cef_gpu_deaths_to_restart;
        let deaths = CHROMIUM_GPU_DEATHS_ABORT - 1;
        let (first, life) = (2_500_000, GPU_RELAUNCH_LIFE_MIN_MS * 1_000);
        let (mut instants, mut at_threshold, mut counted, mut runs) = (0, 0, 0, 0);
        let mut single = 0;
        for cost in [0, 250, 375, 1_439] {
            let paced = (GPU_WATCH_MS * 1_000).max(cost * 100 / GPU_WAIT_SHARE_PCT);
            let gap = paced.min(GPU_LISTING_GAP_MAX_MS * 1_000);
            let mut took_all = Vec::new();
            for relaunch in [300, 1_700, 3_100, 4_900, 6_300, 7_900] {
                let case = format!("a {cost} us listing, each relaunch {relaunch} us after its death");
                let gpus = crash_loop(first, relaunch, life, deaths);
                let killed: Vec<u64> = gpus.iter().filter_map(|g| g.2).collect();
                let cef = FakeCef::with_us(gpus);
                locked(&cef).list_cost_us = cost;
                let (tx, _rx) = crossbeam_channel::bounded(8);
                let g = unit_adapter(unit_shared(tx, fake_tree(&cef)));
                let shared = Arc::clone(&g.shared);
                let lines = capture_log(&shared);
                // When the adapter first counted each death (us).
                let mut seen: Vec<Option<u64>> = vec![None; deaths];
                let end = first + (relaunch + life) * deaths as u64 + 2 * CHILD_POLL_MS * 1_000;
                let polls = drive_paced(&shared, &cef, end, |at, _| {
                    let truth = locked(&cef).deaths();
                    let h = g.health();
                    assert_eq!(h.gpu_process_deaths, truth, "{case}, at {at} us: {h:?}");
                    let want = (truth >= n).then_some(RestartCause::GpuProcessDeaths { count: truth });
                    assert_eq!(h.restart_cause(&t), want, "{case}, at {at} us");
                    for s in seen.iter_mut().take(h.gpu_process_deaths) {
                        s.get_or_insert(at);
                    }
                    at_threshold += usize::from(truth == n);
                    instants += 1;
                });
                assert_eq!(g.health().gpu_process_deaths, deaths, "{case}");
                // Each death counted at the first step after it: one liveness read away at most.
                let took: Vec<u64> = seen
                    .iter()
                    .zip(&killed)
                    .map(|(s, d)| s.expect("counted") - d)
                    .collect();
                assert!(
                    took.iter().all(|us| *us <= GPU_WATCH_MS * 1_000),
                    "{case}: counted {took:?} us after each kill"
                );
                counted += took.len();
                took_all.push(took);
                // One INFO line per death's wait, which together count every step's listing.
                let waits = wait_lines(&locked(&lines));
                let listed = step_listings(&locked(&cef).listed_at_us, &polls);
                assert_eq!(waits.len(), deaths, "{case}: {:?}", locked(&lines));
                let summed: u64 = waits.iter().map(|w| w.0).sum();
                assert_eq!(
                    summed,
                    listed.len() as u64,
                    "{case}: the listings the lines count"
                );
                for (k, w) in waits.iter().enumerate() {
                    let held = paced > GPU_LISTING_GAP_MAX_MS * 1_000;
                    assert_eq!((w.1, w.2, w.3), (cost, gap / 100, held), "{case}: wait {k}");
                }
                // A death's wait whose first listing (the one that saw the death) found the
                // relaunch: that listing is the wait's whole time on the clock, so all of a core
                // (none when a listing costs nothing), whatever the gap after it would have been.
                for w in wait_parts(&locked(&lines)).iter().filter(|w| w.0 == 1) {
                    let share = if cost > 0 { "100.0" } else { "0.0" };
                    let span = format!("{:.1}", cost as f64 / 1_000.0);
                    assert_eq!((&w.4, &w.5), (&span, &share.to_owned()), "{case}: {w:?}");
                    single += 1;
                }
                runs += 1;
            }
            eprintln!(
                "a {cost} us listing, listings {gap} us apart in a wait: each death counted \
                 {took_all:?} us after its kill (by relaunch offset)"
            );
        }
        eprintln!(
            "runs checked: {runs}, instants: {instants}, at the threshold: {at_threshold}, deaths \
             counted: {counted}, one-listing waits: {single}"
        );
        assert!(runs == 24 && instants > 0 && at_threshold > 0 && counted == 24 * deaths);
        assert!(single > 0, "no wait found its relaunch at its first listing");
    }

    // Owed 70 (a), when the watch lists: between two child polls it lists CEF's children only while
    // no GPU process is known, and then only for `GPU_RELAUNCH_WAIT_MS` after CEF's start (its first
    // step) or after a death, and on after a death's wait until the relaunch is listed (the PR #55
    // review's m1). A listing here costs nothing, so it lists at every step (the pace is the next
    // test's). Every other step only reads whether the GPU processes it knows are alive. Five cases:
    // no CEF child ever (the empty case: the start's wait only); a GPU process that lives on; a death
    // relaunched 80 ms later (inside F-026's 69 to 88 ms), after which the listing stops; a death
    // with no relaunch within the wait, then a late relaunch that is listed and dies too; G4's orphan
    // (out of the tree, alive), then its death, after which no relaunch comes.
    #[test]
    fn the_gpu_watch_lists_only_while_a_death_waits_for_its_relaunch() {
        let gpu = FAKE_ROOT + 100;
        let next = FAKE_ROOT + 101;
        let wait = GPU_RELAUNCH_WAIT_MS;
        // (case, the GPU processes, the orphans, the deaths, the instants between which every step
        // lists (both included), the end), in ms.
        type Case = (
            &'static str,
            Vec<(i32, u64, Option<u64>)>,
            Vec<(i32, u64)>,
            Vec<u64>,
            Vec<(u64, u64)>,
            u64,
        );
        let cases: [Case; 5] = [
            (
                "no CEF child",
                vec![],
                vec![],
                vec![],
                vec![(GPU_WATCH_MS, wait)],
                4_000,
            ),
            (
                "a GPU process that lives on",
                vec![(gpu, 0, None)],
                vec![],
                vec![],
                vec![],
                4_000,
            ),
            (
                "a death relaunched 80 ms later",
                vec![(gpu, 0, Some(2_500)), (next, 2_580, None)],
                vec![],
                vec![2_500],
                vec![(2_500, 2_580)],
                4_000,
            ),
            (
                "no relaunch, then a late one",
                vec![(gpu, 0, Some(2_500)), (next, 5_500, Some(6_200))],
                vec![],
                vec![2_500, 6_200],
                vec![(2_500, 5_500), (6_200, 9_500)],
                9_500,
            ),
            (
                "G4's orphan",
                vec![(gpu, 0, Some(3_500))],
                vec![(gpu, 2_500)],
                vec![3_500],
                vec![(3_500, 6_500)],
                6_500,
            ),
        ];
        let (mut instants, mut steps_listing, mut want_steps_listing) = (0, 0, 0);
        for (case, gpus, orphans, deaths, lists, end) in cases {
            let cef = FakeCef::with(gpus);
            locked(&cef).orphans = orphans.iter().map(|&(p, from)| (p, from * 1_000)).collect();
            let (tx, _rx) = crossbeam_channel::bounded(8);
            let g = unit_adapter(unit_shared(tx, fake_tree(&cef)));
            let shared = Arc::clone(&g.shared);
            let waits = |at: u64| lists.iter().any(|(from, to)| *from <= at && at <= *to);
            let mut before = 0;
            drive(&shared, &cef, 0, end, |at| {
                let listings = locked(&cef).listings;
                let listed = listings - before;
                before = listings;
                let poll = at.is_multiple_of(CHILD_POLL_MS);
                assert_eq!(listed, usize::from(poll || waits(at)), "{case}, at {at} ms");
                let counted = deaths.iter().filter(|d| **d <= at).count();
                assert_eq!(g.health().gpu_process_deaths, counted, "{case}, at {at} ms");
                instants += 1;
                steps_listing += usize::from(listed == 1 && !poll);
                want_steps_listing += usize::from(waits(at) && !poll);
            });
        }
        eprintln!("instants checked: {instants}, steps that listed: {steps_listing}");
        assert!(instants > 0 && want_steps_listing > 0);
        assert_eq!(steps_listing, want_steps_listing);
    }

    // Owed 70 (a)'s residual: a GPU process born after one listing and dead before the next is never
    // listed, so never counted. With a listing that costs nothing the watch lists at every step, so
    // that is a GPU process that lives under `GPU_WATCH_MS`; one that lives `GPU_WATCH_MS` across a
    // step is listed and counted. The boundary pair, in one loop.
    #[test]
    fn a_gpu_process_that_lives_between_two_listings_is_not_counted() {
        let cef = FakeCef::with(vec![
            (FAKE_ROOT + 100, 0, Some(2_500)),
            // Born after the 2 500 ms step's listing, dead before the 2 505 ms one.
            (FAKE_ROOT + 101, 2_501, Some(2_504)),
            // `GPU_WATCH_MS` long, across the 2 510 ms step.
            (FAKE_ROOT + 102, 2_507, Some(2_507 + GPU_WATCH_MS)),
            (FAKE_ROOT + 103, 2_520, None),
        ]);
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let g = unit_adapter(unit_shared(tx, fake_tree(&cef)));
        let shared = Arc::clone(&g.shared);
        let mut seen = Vec::new();
        drive(&shared, &cef, 0, 4_000, |at| {
            if (2_500..=2_520).contains(&at) {
                seen.push((at, g.health().gpu_process_deaths));
            }
        });
        let want: Vec<(u64, usize)> = vec![(2_500, 1), (2_505, 1), (2_510, 1), (2_515, 2), (2_520, 2)];
        assert_eq!(seen, want);
        assert_eq!(locked(&cef).deaths(), 3, "the kernel's count");
        assert_eq!(g.health().gpu_process_deaths, 2, "one under a step: not counted");
        eprintln!("instants checked: {}", seen.len());
    }

    // Owed 70 (a)'s residual in a paced wait (the PR #55 review's I3): a relaunch born just after one
    // of a wait's listings and dead just before the next is never listed, so never counted; one born
    // at the same instant that lives `GPU_RELAUNCH_LIFE_MIN_MS` is listed by the next and counted,
    // since a wait's gap is `GPU_LISTING_GAP_MAX_MS` at the most. The pair at a 375 us listing (a
    // 7.5 ms pace) and at 1 439 us (the image's debug listing on the clock; the ceiling's gap). The
    // listings' instants come from a first run with no relaunch: a process no listing sees changes
    // nothing.
    #[test]
    fn a_relaunch_between_two_paced_listings_is_missed_and_one_at_the_shortest_life_is_not() {
        let (death, life) = (2_500_000, GPU_RELAUNCH_LIFE_MIN_MS * 1_000);
        let mut pairs = 0;
        for cost in [375, 1_439] {
            let run = |gpus: Vec<(i32, u64, Option<u64>)>| {
                let cef = FakeCef::with_us(gpus);
                locked(&cef).list_cost_us = cost;
                let (tx, _rx) = crossbeam_channel::bounded(8);
                let g = unit_adapter(unit_shared(tx, fake_tree(&cef)));
                let shared = Arc::clone(&g.shared);
                let polls = drive_paced(&shared, &cef, death + 1_000_000, |_, _| {});
                let listed = step_listings(&locked(&cef).listed_at_us, &polls);
                let kernel = locked(&cef).deaths();
                (g.health().gpu_process_deaths, kernel, listed)
            };
            let p0 = (FAKE_ROOT + 100, 0, Some(death));
            let (_, _, listed) = run(vec![p0]);
            // The death's listing, then the wait's next two.
            let (a, b) = (listed[1], listed[2]);
            assert!(
                listed[0] >= death && b - a <= GPU_LISTING_GAP_MAX_MS * 1_000,
                "{listed:?}"
            );
            let short = run(vec![p0, (FAKE_ROOT + 101, a + 1, Some(b - 1))]);
            assert_eq!(
                (short.0, short.1),
                (1, 2),
                "a {cost} us listing: under the gap, not counted"
            );
            let shortest = run(vec![p0, (FAKE_ROOT + 101, a + 1, Some(a + 1 + life))]);
            assert_eq!(
                (shortest.0, shortest.1),
                (2, 2),
                "a {cost} us listing: {life} us from {a} us, listed at {b} us and counted"
            );
            eprintln!(
                "a {cost} us listing: listings at {a} and {b} us; a {} us life missed, a {life} us one \
                 counted",
                b - a - 2
            );
            pairs += 1;
        }
        eprintln!("pairs checked: {pairs}");
        assert_eq!(pairs, 2);
    }

    // Owed 70 (a), the wait's cost (the coordinator's budget on the PR #55 review's I2, and its
    // ceiling on I3): a listing walks every process, so its cost is the host's. On the child poll
    // thread's own loop (`drive_paced`), each listing sets the next `100 / GPU_WAIT_SHARE_PCT` times
    // its CPU after its start while a wait lasts (`GPU_WATCH_MS` at the soonest, and
    // `GPU_LISTING_GAP_MAX_MS` at the latest), and `100 / GPU_LATE_SHARE_PCT` times after the wait
    // (no ceiling); a listing longer than that is followed as it returns. So a death's wait spends at
    // most `GPU_WAIT_SHARE_PCT` of a core while that pace is within the ceiling, lists every
    // `GPU_LISTING_GAP_MAX_MS` past it, and ends `GPU_RELAUNCH_WAIT_MS` after the death on the clock
    // (400 listings at 8.1 ms each had made it 3.2 s on CI's runner); its one INFO line counts its
    // listings. One death, which a step sees (at 2 500 ms) or the poll does (at a poll's instant: the
    // poll lists, then reads), and no relaunch until a late one well after the wait. The costs: none
    // (the floor); 250 us, where the share's pace meets the floor; 400 us, where it meets the
    // ceiling; 1 ms; 3 075 us, CI's measured walk (check run 40077609130); 10 ms, longer than the
    // ceiling itself.
    #[test]
    fn a_wait_lists_at_its_share_of_a_core_and_never_further_apart_than_the_ceiling() {
        let (step, wait) = (GPU_WATCH_MS * 1_000, GPU_RELAUNCH_WAIT_MS * 1_000);
        let ceiling = GPU_LISTING_GAP_MAX_MS * 1_000;
        let in_wait = |cost: u64| step.max(cost * 100 / GPU_WAIT_SHARE_PCT).min(ceiling);
        let late_pace = |cost: u64| step.max(cost * 100 / GPU_LATE_SHARE_PCT);
        let (mut gaps, mut exact, mut runs, mut shares) = (0, 0, 0, 0);
        for seen_by in ["a step", "the poll"] {
            for cost in [0, 250, 400, 1_000, 3_075, 10_000] {
                let case = format!("{seen_by}, a {cost} us listing");
                let run = |death: u64| {
                    let late = death + (GPU_RELAUNCH_WAIT_MS + 1_234) * 1_000;
                    let cef = FakeCef::with_us(vec![
                        (FAKE_ROOT + 100, 0, Some(death)),
                        (FAKE_ROOT + 101, late, None),
                    ]);
                    locked(&cef).list_cost_us = cost;
                    let (tx, _rx) = crossbeam_channel::bounded(8);
                    let g = unit_adapter(unit_shared(tx, fake_tree(&cef)));
                    let shared = Arc::clone(&g.shared);
                    let lines = capture_log(&shared);
                    let to = late + 2 * CHILD_POLL_MS * 1_000;
                    // The steps' instants (us): each starts with `gpu_step`'s reading of the clock.
                    let mut stepped = Vec::new();
                    let polls = drive_paced(&shared, &cef, to, |at, poll| {
                        if !poll {
                            stepped.push(at);
                        }
                    });
                    let all = locked(&cef).listed_at_us.clone();
                    let waits = wait_parts(&locked(&lines));
                    let counted = g.health().gpu_process_deaths;
                    (counted, polls, all, waits, late, stepped)
                };
                // The poll's death: at the instant of the fourth poll, about 3 s in (a first run's,
                // which a death there does not move).
                let death = if seen_by == "a step" {
                    2_500_000
                } else {
                    run(10_000_000).1[3]
                };
                let (counted, polls, all, waits, late, stepped) = run(death);
                let by_poll = polls.contains(&death);
                assert_eq!(by_poll, seen_by == "the poll", "{case}: polls at {polls:?}");
                let steps: Vec<u64> = step_listings(&all, &polls)
                    .into_iter()
                    .filter(|t| *t >= death)
                    .collect();
                // The death's own listing, or the step after the poll's; the wait from that reading.
                let (first, wait_ends) = if by_poll {
                    (death + cost + step, death + cost + wait)
                } else {
                    assert!(
                        steps[0] - death < step,
                        "{case}: the death listed at {}",
                        steps[0]
                    );
                    (steps[0], steps[0] + wait)
                };
                assert_eq!(steps.first(), Some(&first), "{case}: the first listing");
                // Each gap: the pace of the share in force when it began (or the listing's own
                // time, when longer); a poll between two listings adds its listing and a step.
                for w in steps.windows(2) {
                    let want = if w[0] < wait_ends {
                        in_wait(cost)
                    } else {
                        late_pace(cost)
                    };
                    let (gap, polled) = (w[1] - w[0], polls.iter().any(|p| w[0] < *p && *p < w[1]));
                    let most = want.max(cost) + if polled { cost + step } else { 0 };
                    assert!(
                        want <= gap && gap <= most,
                        "{case}: listings at {} and {} us, {gap} us apart, not {want} to {most}",
                        w[0],
                        w[1]
                    );
                    if !polled {
                        assert_eq!(
                            gap,
                            want.max(cost),
                            "{case}: listings at {} and {} us",
                            w[0],
                            w[1]
                        );
                        exact += 1;
                    }
                    gaps += 1;
                }
                // The late listings (after the wait, before the late relaunch): some, or the late
                // share's gaps above checked none.
                let after = steps.iter().filter(|t| **t >= wait_ends && **t < late).count();
                assert!(after > 0, "{case}: no listing after the wait");
                // The wait's CPU: its share of the wait and one listing over, while the share's pace
                // is within the ceiling.
                let in_wait_n = steps.iter().filter(|t| **t < wait_ends).count() as u64;
                if cost * 100 / GPU_WAIT_SHARE_PCT <= ceiling {
                    assert!(
                        in_wait_n * cost <= GPU_WAIT_SHARE_PCT * wait / 100 + cost,
                        "{case}: {in_wait_n} listings in the wait"
                    );
                }
                // Its one INFO line: those listings, at this cost, the wait's gap, whether the
                // ceiling held it, and the wait's share of a core (the confirm pass's r2-m2): their
                // CPU over the wait's time on the clock, from its start (the death's reading) to
                // the instant the watch saw it over (the end of its last listing, when that ends
                // past the wait, or else the first step after it). Past the ceiling, the share is
                // more than `GPU_WAIT_SHARE_PCT`, and back to back (a listing longer than the
                // ceiling) it is the whole core.
                let held = cost * 100 / GPU_WAIT_SHARE_PCT > ceiling;
                let last = *steps.iter().rfind(|t| **t < wait_ends).expect("a wait listing");
                let over = if last + cost >= wait_ends {
                    last + cost
                } else {
                    *stepped
                        .iter()
                        .find(|t| **t >= wait_ends)
                        .expect("a step after the wait")
                };
                let span = over - (wait_ends - wait);
                let share = (in_wait_n * cost) as f64 * 100.0 / span as f64;
                let want = (span as f64 / 1_000.0, share);
                let want = (format!("{:.1}", want.0), format!("{:.1}", want.1));
                eprintln!(
                    "{case}: {in_wait_n} listings in {} ms, {}% of a core",
                    want.0, want.1
                );
                assert_eq!(
                    waits,
                    vec![(in_wait_n, cost, in_wait(cost) / 100, held, want.0, want.1)],
                    "{case}"
                );
                if cost * 100 / GPU_WAIT_SHARE_PCT <= ceiling {
                    assert!(share <= GPU_WAIT_SHARE_PCT as f64 + 0.1, "{case}: {share}%");
                } else if cost > ceiling {
                    // The rest of the core: the polls' listings and the step after each.
                    assert!(share > 95.0, "{case}: back to back at {share}%");
                } else {
                    assert!(share > GPU_WAIT_SHARE_PCT as f64, "{case}: held at {share}%");
                }
                shares += 1;
                // The late relaunch: listed by the first listing after its birth (a step's or a
                // poll's), after which no step lists.
                let found = all
                    .iter()
                    .copied()
                    .find(|t| *t >= late)
                    .expect("the late relaunch listed");
                assert!(
                    found <= late + late_pace(cost).max(cost) + cost + step,
                    "{case}: the late relaunch listed at {found} us"
                );
                assert!(
                    steps.iter().all(|t| *t <= found),
                    "{case}: a step listed after the relaunch was listed"
                );
                assert_eq!(counted, 1, "{case}");
                runs += 1;
            }
        }
        eprintln!(
            "runs checked: {runs}, gaps checked: {gaps} ({exact} at exactly the pace), shares: \
             {shares}"
        );
        assert!(runs == 12 && gaps > 0 && exact > 0 && shares == 12);
    }

    // Owed 70 (a), the PR #55 review's I3: a listing paces the next on its CPU, not on its time on the
    // clock, so preemption or blocking defers no listing. In a wait, every listing preempted (250 us
    // of CPU over 350 us on the clock): the gaps are the CPU's pace (`GPU_WATCH_MS`, where the share
    // meets the floor), not the clock's 7 ms. After the wait (`GPU_LATE_SHARE_PCT`, no ceiling), one
    // listing stalled 100 ms on the clock for 300 us of CPU: the next starts as it returns, and the
    // ones after keep the CPU's 30 ms pace; on the clock's, the next would come 10 s later.
    #[test]
    fn a_listing_paces_the_next_on_its_cpu_not_on_its_time_on_the_clock() {
        let (death, step) = (2_500_000, GPU_WATCH_MS * 1_000);
        let run = |clock: u64, cpu: u64, stalled: Option<(usize, u64)>| {
            let cef = FakeCef::with_us(vec![(FAKE_ROOT + 100, 0, Some(death))]);
            {
                let mut c = locked(&cef);
                (c.list_cost_us, c.list_cpu_us) = (clock, Some(cpu));
                c.stalls = stalled.into_iter().collect();
            }
            let (tx, _rx) = crossbeam_channel::bounded(8);
            let g = unit_adapter(unit_shared(tx, fake_tree(&cef)));
            let shared = Arc::clone(&g.shared);
            let polls = drive_paced(&shared, &cef, death + 4 * CHILD_POLL_MS * 1_000, |_, _| {});
            let all = locked(&cef).listed_at_us.clone();
            let steps: Vec<u64> = step_listings(&all, &polls)
                .into_iter()
                .filter(|t| *t >= death)
                .collect();
            (polls, all, steps)
        };
        let unpolled = |polls: &[u64], a: u64, b: u64| !polls.iter().any(|p| a < *p && *p < b);
        // In a wait, every listing preempted.
        let (polls, _, steps) = run(350, 250, None);
        let wait_ends = steps[0] + GPU_RELAUNCH_WAIT_MS * 1_000;
        let pace = step.max(250 * 100 / GPU_WAIT_SHARE_PCT);
        let in_wait: Vec<u64> = steps
            .windows(2)
            .filter(|w| w[1] < wait_ends && unpolled(&polls, w[0], w[1]))
            .map(|w| w[1] - w[0])
            .collect();
        assert!(in_wait.iter().all(|g| *g == pace), "in the wait: {in_wait:?}");
        // After the wait, the first late listing stalls (its number among all listings, from a run
        // without the stall, which moves none before it).
        let stall = 100_000;
        let (_, all, steps) = run(300, 300, None);
        let wait_ends = steps[0] + GPU_RELAUNCH_WAIT_MS * 1_000;
        let late = *steps.iter().find(|t| **t >= wait_ends).expect("a late listing");
        let n = all.iter().position(|t| *t == late).expect("listed") + 1;
        let (polls, _, steps) = run(300, 300, Some((n, stall)));
        let after: Vec<u64> = steps.iter().copied().filter(|t| *t >= late).take(4).collect();
        assert!(
            after.windows(2).all(|w| unpolled(&polls, w[0], w[1])),
            "{after:?}"
        );
        let gaps: Vec<u64> = after.windows(2).map(|w| w[1] - w[0]).collect();
        let late_pace = step.max(300 * 100 / GPU_LATE_SHARE_PCT);
        assert_eq!(
            gaps,
            vec![stall + 300, late_pace, late_pace],
            "after the stall at {late} us"
        );
        eprintln!(
            "gaps checked: {} in the wait ({pace} us each), {} after the wait ({gaps:?} us)",
            in_wait.len(),
            gaps.len()
        );
        assert!(!in_wait.is_empty() && gaps.len() == 3);
    }

    // The PR #55 review's I3: the GPU watch paces on the thread's CPU clock (`thread_cpu_us`), which a
    // sleep does not move and work does: across a 200 ms sleep it moves under a tenth of that, and a
    // spin moves it by 200 ms within no less than that on the clock.
    #[test]
    fn the_thread_cpu_clock_counts_work_and_not_sleep() {
        let nap = Duration::from_millis(200);
        let c0 = thread_cpu_us();
        thread::sleep(nap);
        let slept = thread_cpu_us() - c0;
        assert!(slept < 20_000, "{slept} us of CPU across a {nap:?} sleep");
        let (c1, t1) = (thread_cpu_us(), Instant::now());
        let mut spins = 0_u64;
        while thread_cpu_us() - c1 < 200_000 && t1.elapsed() < Duration::from_secs(10) {
            spins = spins.wrapping_add(1);
        }
        let (worked, took) = (thread_cpu_us() - c1, t1.elapsed());
        assert!(worked >= 200_000, "{worked} us of CPU in {took:?} of spinning");
        assert!(
            took.as_micros() as u64 + 1_000 >= worked,
            "{worked} us of CPU in {took:?}"
        );
        eprintln!("thread CPU: {slept} us across a {nap:?} sleep, {worked} us in {took:?} ({spins} reads)");
    }

    // The PR #55 review's m2: CEF's watch is the process's, and each adapter has its own child poll
    // thread, so a second adapter's listing applied after a newer one could put a dead GPU process
    // back among the live and count its death twice (PR #51's stale list). One adapter's thread
    // counts the GPU processes (`CefWatch::claim_gpu`); the other's polls still read the zygotes, but
    // its steps never list and its polls never count. The claim is let go when that thread ends, and
    // the next adapter's takes it.
    #[test]
    fn only_one_adapters_poll_thread_counts_the_gpu_processes() {
        let (p, q) = (FAKE_ROOT + 100, FAKE_ROOT + 101);
        let cef = FakeCef::with(vec![(p, 0, Some(2_500)), (q, 2_600, None)]);
        let watch: Arc<Mutex<CefWatch>> = Arc::default();
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let a = unit_adapter(unit_shared_with(
            tx.clone(),
            fake_tree(&cef),
            Arc::clone(&watch),
            1,
        ));
        let b = unit_adapter(unit_shared_with(tx, fake_tree(&cef), Arc::clone(&watch), 2));
        let (sa, sb) = (Arc::clone(&a.shared), Arc::clone(&b.shared));
        let at = |t: u64| {
            let mut c = locked(&cef);
            c.now_us = t * 1_000;
            c.spent_us = 0;
        };
        let mut checked = 0;
        // Both polls list P; A's polled first.
        at(0);
        watch_poll(&sa, &(sa.tree.list)());
        watch_poll(&sb, &(sb.tree.list)());
        // B lists while P lives, A's step counts P's death, then B applies its stale listing.
        at(2_495);
        let stale = (sb.tree.list)();
        at(2_500);
        assert!(gpu_step(&sa), "A's step lists at P's death");
        assert_eq!(a.health().gpu_process_deaths, 1);
        watch_poll(&sb, &stale);
        // B's steps never list; A's count P once and list Q once born.
        for t in (2_505..=2_700).step_by(GPU_WATCH_MS as usize) {
            at(t);
            assert!(!gpu_step(&sb), "B's step listed at {t} ms");
            gpu_step(&sa);
            assert_eq!(b.health().gpu_process_deaths, 1, "at {t} ms: P counted once");
            checked += 1;
        }
        assert!(locked(&watch).gpu.live.contains(&q), "Q listed");
        // A's thread ends and lets the claim go; B's next step counts Q's death.
        let poller = {
            let sa = Arc::clone(&sa);
            thread::spawn(move || poll_children(&sa))
        };
        assert!(
            wait_until(Duration::from_millis(2 * CHILD_POLL_MS), || sa
                .polls
                .load(Ordering::SeqCst)
                >= 1),
            "A's poll thread never polled"
        );
        sa.retired.store(true, Ordering::SeqCst);
        poller.join().expect("A's poll thread panicked");
        for gpu in locked(&cef).gpus.iter_mut().filter(|x| x.0 == q) {
            gpu.2 = Some(3_000_000);
        }
        at(3_000);
        assert!(gpu_step(&sb), "B's step lists once A's thread is gone");
        assert_eq!(b.health().gpu_process_deaths, 2, "Q's death, counted by B");
        assert_eq!(locked(&cef).deaths(), 2, "the kernel's count");
        checked += 1;
        eprintln!("instants checked: {checked}");
        assert!(checked > 1);
    }

    // The PR #55 review's m3: a `/proc/<pid>/stat` read that fails is a death only when the process
    // is gone (ENOENT at the open, ESRCH at the read). Any other error (EMFILE, ENOMEM) says nothing
    // of the process: it reads as alive, so an error episode at one step per `GPU_WATCH_MS` cannot
    // pile up false deaths into a false restart (or a false zygote verdict). Each is counted, and the
    // child poll logs the count once per poll when it grew.
    #[test]
    fn a_stat_read_that_fails_otherwise_is_no_death() {
        use rustix::io::Errno;
        use std::io::{Error, ErrorKind};
        fn os(e: Errno) -> Error {
            Error::from_raw_os_error(e.raw_os_error())
        }
        type Read = fn() -> std::io::Result<String>;
        let cases: [(&str, Read, Option<bool>); 8] = [
            ("running", || Ok("12 (cef) S 1 12".to_owned()), Some(true)),
            (
                "a ')' in its name",
                || Ok("12 (a) Z) S 1 12".to_owned()),
                Some(true),
            ),
            ("a zombie", || Ok("12 (cef) Z 1 12".to_owned()), Some(false)),
            (
                "gone at the open",
                || Err(Error::from(ErrorKind::NotFound)),
                Some(false),
            ),
            ("gone at the read", || Err(os(Errno::SRCH)), Some(false)),
            ("too many open files", || Err(os(Errno::MFILE)), None),
            ("out of memory", || Err(os(Errno::NOMEM)), None),
            ("refused", || Err(os(Errno::ACCESS)), None),
        ];
        let mut checked = 0;
        for (case, read, want) in cases {
            assert_eq!(stat_alive(read()), want, "{case}");
            // The watch's reading: nothing said is alive, and counted.
            let before = UNREAD_STATS.load(Ordering::Relaxed);
            assert_eq!(read_alive(read()), want.unwrap_or(true), "{case}: read as");
            let counted = UNREAD_STATS.load(Ordering::Relaxed) - before;
            assert_eq!(counted, u64::from(want.is_none()), "{case}: counted");
            checked += 1;
        }
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let shared = unit_shared(tx, fake_tree(&FakeCef::with(vec![])));
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        *locked(&shared.log) = {
            let lines = Arc::clone(&lines);
            Arc::new(move |l: &str| locked(&lines).push(l.to_owned()))
        };
        let mut seen = UNREAD_STATS.load(Ordering::Relaxed);
        log_unread_stats(&shared, &mut seen);
        assert!(locked(&lines).is_empty(), "no line with no new failure");
        UNREAD_STATS.fetch_add(2, Ordering::Relaxed);
        log_unread_stats(&shared, &mut seen);
        log_unread_stats(&shared, &mut seen);
        assert_eq!(
            *locked(&lines),
            vec![
                "WARN: 2 /proc stat read(s) failed with neither ENOENT nor ESRCH: read as alive, not \
                 as deaths (Owed 70 (a))"
                    .to_owned()
            ]
        );
        checked += 1;
        eprintln!("reads checked: {checked}");
        assert_eq!(checked, 9);
    }

    fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        cond()
    }

    // The soak's memory split, through the real child poll thread: absent before the first poll;
    // then each poll's number and its PSS by kind. The fake's children are pids no process has, so
    // no PSS is read: each kind is listed with none read (n 0), never a 0 PSS read and never absent
    // (PR #82 review I2: a poll that lists children and reads none must say so).
    #[test]
    fn the_child_poll_hands_the_memory_split_its_number_and_pss_by_kind() {
        let cef = FakeCef::with(vec![(FAKE_ROOT + 100, 0, None)]);
        locked(&cef).now_us = 1_000;
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let g = unit_adapter(unit_shared(tx, fake_tree_on(&cef, Box::new(monotonic_us))));
        let memory = g.memory();
        assert_eq!(memory.read(), None, "no poll yet: absent");
        let shared = Arc::clone(&g.shared);
        let poller = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || poll_children(&shared))
        };
        let polls = || shared.polls.load(Ordering::SeqCst);
        assert!(
            wait_until(Duration::from_millis(3 * CHILD_POLL_MS), || polls() >= 2),
            "no second child poll"
        );
        let first = memory.read().expect("polled: the split has the poll");
        assert!(
            first.poll >= 2,
            "the poll's number: {first:?} after {} polls",
            polls()
        );
        assert_eq!(
            first.pss,
            CefPss {
                zygote: Some(KindPss {
                    kb: 0,
                    n: 0,
                    listed: 2
                }),
                gpu_process: Some(KindPss {
                    kb: 0,
                    n: 0,
                    listed: 1
                }),
                ..CefPss::default()
            },
            "no PSS read: two zygotes and a GPU process listed, none read"
        );
        let listed = g.last_guard().children;
        assert_eq!(listed, 3, "two zygotes and a GPU process listed");
        assert!(
            wait_until(Duration::from_millis(3 * CHILD_POLL_MS), || polls() > first.poll),
            "no later child poll"
        );
        let later = memory.read().expect("still polled");
        assert!(later.poll > first.poll, "{later:?} after {first:?}");
        shared.retired.store(true, Ordering::SeqCst);
        assert!(
            wait_until(Duration::from_millis(2 * CHILD_POLL_MS), || poller.is_finished()),
            "the poll thread did not stop"
        );
        poller.join().expect("the poll thread panicked");
        eprintln!(
            "children listed: {listed}, polls read: {} then {}",
            first.poll, later.poll
        );
    }

    // The memory split's PSS by kind, from the guard's own reads: a real child (a `sleep` this test
    // starts, listed as a renderer) has its PSS under `renderer`, counted once, and the same kB the
    // guard's sum has; a child whose rollup is unreadable leaves its kind absent. Linux: `/proc`.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_guard_buckets_each_childs_pss_under_its_type() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep starts");
        let me = std::process::id() as i32;
        let pid = child.id() as i32;
        let kids = [
            kid(pid, me, "renderer", None),
            kid(FAKE_ROOT + 1, me, "gpu-process", None),
        ];
        let g = guard(&kids, |_| true);
        let _ = child.kill();
        let _ = child.wait();
        let renderer = g.pss_by_kind.renderer.expect("the sleep's PSS was read");
        assert_eq!((renderer.n, renderer.listed), (1, 1), "{g:?}");
        assert!(renderer.kb > 0, "a live process has a PSS: {g:?}");
        assert_eq!(g.pss_mb, renderer.kb / 1024, "the same reads as the sum: {g:?}");
        assert_eq!(
            g.pss_by_kind.gpu_process,
            Some(KindPss {
                kb: 0,
                n: 0,
                listed: 1
            }),
            "unread: listed, none read, never a 0 read: {g:?}"
        );
        assert_eq!(g.children, 2);
    }

    // The seam (Owed 70 (a)): the child poll's own thread runs the GPU watch between its polls. A
    // crash loop on the wall clock, between two polls, of C14's fast leg's length
    // (`CHROMIUM_GPU_DEATHS_ABORT` - 1 deaths): each relaunch is born `RELAUNCH_MS` after a death and
    // killed `LIFE_MS` later, far under `CHILD_POLL_MS`. Each death is counted within half a poll,
    // once; the restart cause comes at exactly `cef_gpu_deaths_to_restart`; and the thread still runs
    // a child poll only every `CHILD_POLL_MS`.
    #[test]
    fn the_child_poll_thread_counts_a_crash_loop_between_two_polls() {
        use domain::cef_health::RestartCause;
        use domain::config::{Thresholds, CHROMIUM_GPU_DEATHS_ABORT};
        const RELAUNCH_MS: u64 = 20;
        const LIFE_MS: u64 = 100;
        let t = Thresholds::default();
        let n = t.cef_gpu_deaths_to_restart;
        let kills = CHROMIUM_GPU_DEATHS_ABORT - 1;
        // The clock stays at 1 ms: a kill is a death at 1 ms, a relaunch a birth at 0.
        let cef = FakeCef::with(vec![(FAKE_ROOT + 100, 0, None)]);
        locked(&cef).now_us = 1_000;
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let g = unit_adapter(unit_shared(tx, fake_tree_on(&cef, Box::new(monotonic_us))));
        let shared = Arc::clone(&g.shared);
        let poller = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || poll_children(&shared))
        };
        let polls = || shared.polls.load(Ordering::SeqCst);
        assert!(
            wait_until(Duration::from_millis(2 * CHILD_POLL_MS), || polls() >= 1),
            "no child poll"
        );
        let (started, polls_before) = (Instant::now(), polls());
        let mut current = FAKE_ROOT + 100;
        for k in 1..=kills {
            for gpu in locked(&cef).gpus.iter_mut().filter(|x| x.0 == current) {
                gpu.2 = Some(1_000);
            }
            let counted = wait_until(Duration::from_millis(CHILD_POLL_MS / 2), || {
                g.health().gpu_process_deaths >= k
            });
            let h = g.health();
            assert!(
                counted,
                "death {k} (pid {current}) not counted in half a poll: {h:?}"
            );
            assert_eq!(h.gpu_process_deaths, k, "death {k}: counted once");
            assert_eq!(locked(&cef).deaths(), k, "death {k}: the kernel's count");
            let want = (k >= n).then_some(RestartCause::GpuProcessDeaths { count: k });
            assert_eq!(h.restart_cause(&t), want, "death {k}");
            thread::sleep(Duration::from_millis(RELAUNCH_MS));
            current = FAKE_ROOT + 100 + k as i32;
            locked(&cef).gpus.push((current, 0, None));
            thread::sleep(Duration::from_millis(LIFE_MS));
        }
        let elapsed = started.elapsed().as_millis() as u64;
        let ran = polls() - polls_before;
        shared.retired.store(true, Ordering::SeqCst);
        assert!(
            wait_until(Duration::from_millis(CHILD_POLL_MS), || poller.is_finished()),
            "the poll thread did not stop"
        );
        poller.join().expect("the poll thread panicked");
        eprintln!("crash loop: {kills} deaths counted in {elapsed} ms, {ran} child polls");
        assert_eq!(g.health().gpu_process_deaths, kills);
        assert!(
            ran <= elapsed / CHILD_POLL_MS + 1,
            "{ran} child polls in {elapsed} ms"
        );
    }

    // Owed 70 (a): the GPU watch's cost, measured on the host that runs the tests (CI's runner, whose
    // `/proc` holds more processes than the container's; an unoptimised build). A step reads one GPU
    // process's `stat` (`pid_alive`); a listing walks every process's `stat` and CEF's children's
    // command lines (`cef_children`, here with no CEF child).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_gpu_watch_cost_is_measured() {
        const READS: u32 = 2_000;
        const WALKS: u32 = 200;
        let me = std::process::id() as i32;
        let t0 = Instant::now();
        let alive = (0..READS).filter(|_| pid_alive(me)).count();
        let read_us = t0.elapsed().as_secs_f64() * 1e6 / f64::from(READS);
        let procs = std::fs::read_dir("/proc").map_or(0, |d| {
            d.flatten()
                .filter(|e| e.file_name().to_str().is_some_and(|s| s.parse::<i32>().is_ok()))
                .count()
        });
        let (t1, c1) = (Instant::now(), thread_cpu_us());
        let listed: usize = (0..WALKS).map(|_| cef_children().len()).sum();
        let walk_us = t1.elapsed().as_secs_f64() * 1e6 / f64::from(WALKS);
        let cpu_us = (thread_cpu_us() - c1) as f64 / f64::from(WALKS);
        let paced_ms = cpu_us * 100.0 / GPU_WAIT_SHARE_PCT as f64 / 1_000.0;
        let pace_ms = paced_ms.clamp(GPU_WATCH_MS as f64, GPU_LISTING_GAP_MAX_MS as f64);
        eprintln!(
            "gpu watch cost: {READS} liveness reads at {read_us:.1} us each ({:.2}% of a core at one \
             per {GPU_WATCH_MS} ms); {WALKS} walks over {procs} processes at {cpu_us:.0} us of CPU \
             each ({walk_us:.0} us on the clock); {listed} CEF children listed; a wait lists every \
             {pace_ms:.1} ms at that cost ({:.1}% of a core; the share's pace {paced_ms:.1} ms)",
            read_us / (GPU_WATCH_MS as f64 * 10.0),
            cpu_us / (pace_ms * 10.0)
        );
        assert_eq!(alive, READS as usize, "this process reads as alive");
        assert!(procs > 0, "no process in /proc");
    }

    fn page_note(build: u64, signal: PageSignal) -> Note {
        Note::Page {
            build,
            gen: build,
            signal,
            text: String::new(),
        }
    }

    // PR #32 review m6: a full queue gives up log lines first, repeatable signals next, and never a
    // decided crash or a build's ready.
    #[test]
    fn a_full_note_queue_keeps_every_crash_and_ready() {
        let log = |n: u64| Note::Element {
            gen: 1,
            text: format!("line {n}"),
        };
        let crashed = |gen: u64| Note::Crashed {
            gen,
            why: "test".to_owned(),
        };
        let count = |q: &NoteQueue, f: &dyn Fn(&Note) -> bool| q.q.iter().filter(|n| f(n)).count();
        let is_crash = |n: &Note| matches!(n, Note::Crashed { .. });
        let is_ready = |n: &Note| n.ready_of().is_some();
        let is_beat = |n: &Note| {
            matches!(
                n,
                Note::Page {
                    signal: PageSignal::Alive,
                    ..
                }
            )
        };
        let mut checked = 0;

        // A crash and a ready, then a flood of log lines: both kept, the log lines rotate.
        let mut q = NoteQueue::default();
        q.push(crashed(1));
        q.push(page_note(1, PageSignal::Ready));
        for n in 0..(NOTE_QUEUE_CAP as u64 * 3) {
            q.push(log(n));
        }
        assert_eq!(q.q.len(), NOTE_QUEUE_CAP);
        assert_eq!((count(&q, &is_crash), count(&q, &is_ready)), (1, 1));
        checked += 1;

        // A queue of beats takes a crash in place of the oldest beat, and drops a log line outright.
        let mut q = NoteQueue::default();
        for _ in 0..NOTE_QUEUE_CAP {
            q.push(page_note(2, PageSignal::Alive));
        }
        q.push(crashed(2));
        assert_eq!((q.q.len(), count(&q, &is_crash)), (NOTE_QUEUE_CAP, 1));
        let dropped = q.dropped;
        q.push(log(0));
        assert_eq!(q.dropped, dropped + 1);
        assert_eq!(count(&q, &is_beat), NOTE_QUEUE_CAP - 1);
        checked += 2;

        // A second ready of a build with one queued is dropped; another build's is kept.
        let mut q = NoteQueue::default();
        q.push(page_note(3, PageSignal::Ready));
        q.push(page_note(3, PageSignal::Ready));
        q.push(page_note(4, PageSignal::Ready));
        assert_eq!(count(&q, &is_ready), 2);
        checked += 1;

        // Only crashes and readies queued: one more is kept past the cap.
        let mut q = NoteQueue::default();
        for b in 0..NOTE_QUEUE_CAP as u64 {
            q.push(page_note(b, PageSignal::Ready));
        }
        q.push(crashed(9));
        assert_eq!(q.q.len(), NOTE_QUEUE_CAP + 1);
        assert_eq!(count(&q, &is_crash), 1);
        checked += 1;
        // The order is kept: the crash comes out last.
        let mut last = None;
        while let Some(n) = q.pop() {
            last = Some(n);
        }
        assert!(matches!(last, Some(Note::Crashed { gen: 9, .. })));
        checked += 1;
        eprintln!("queue cases checked: {checked}");
    }

    // PR #32 review m7: `with_log` once the dispatcher holds the shared state (after the first
    // `restart`) used to be a silent no-op.
    /// An adapter's shared state with no CEF behind it: `tree` is what its watch lists, and the watch
    /// is its own (not the process's).
    fn unit_shared(tx: Sender<Inbound>, tree: ProcTree) -> Arc<Shared> {
        unit_shared_with(tx, tree, Arc::default(), u64::MAX)
    }

    /// `unit_shared` over `watch` (two adapters in one process share the process's), as `inst`.
    fn unit_shared_with(
        tx: Sender<Inbound>,
        tree: ProcTree,
        watch: Arc<Mutex<CefWatch>>,
        inst: u64,
    ) -> Arc<Shared> {
        Arc::new(Shared {
            inst,
            tx,
            frames: FrameStore::default(),
            log: Mutex::new(stderr_log()),
            scrubber: Mutex::new(Scrubber::default()),
            gate: Mutex::new(Gate::default()),
            notes: Mutex::new(NoteQueue::default()),
            notes_cv: Condvar::new(),
            retired: AtomicBool::new(false),
            rss_cap_mb: AtomicU64::new(600),
            teardowns: AtomicUsize::new(0),
            leaked: AtomicUsize::new(0),
            polls: AtomicU64::new(0),
            builds: Mutex::new(BTreeMap::new()),
            reports: Mutex::new(BTreeMap::new()),
            last_guard: Mutex::new(Guarded::default()),
            memory: CefMemory::default(),
            tree,
            watch,
            #[cfg(feature = "test-hooks")]
            teardown_hold: Mutex::new(false),
            #[cfg(feature = "test-hooks")]
            teardown_cv: Condvar::new(),
            #[cfg(feature = "test-hooks")]
            poll_hold: Mutex::new(false),
            #[cfg(feature = "test-hooks")]
            poll_cv: Condvar::new(),
            #[cfg(feature = "test-hooks")]
            kill_after_ms: AtomicU64::new(REAPER_KILL_AFTER_MS),
            #[cfg(feature = "test-hooks")]
            write_hold: Arc::default(),
            #[cfg(feature = "test-hooks")]
            js_signals: AtomicBool::new(false),
            #[cfg(feature = "test-hooks")]
            teardowns_to_hold: AtomicUsize::new(0),
        })
    }

    /// An adapter over `shared` that never started a thread or a build (its drop retires `shared`).
    fn unit_adapter(shared: Arc<Shared>) -> CefGraphics {
        CefGraphics {
            shared,
            fps: 30,
            current: None,
            builds: 0,
            threads: true,
            _lease: ProfileLease { held: false },
        }
    }

    #[test]
    fn with_log_swaps_the_sink_after_the_threads_hold_the_state() {
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let shared = unit_shared(tx, ProcTree::live());
        locked(&shared.scrubber).add("unitSECRETmadeup0042");
        // The dispatcher's clone.
        let dispatcher = Arc::clone(&shared);
        let g = unit_adapter(shared);
        let got: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&got);
        let g = g.with_log(Arc::new(move |l: &str| locked(&sink).push(l.to_owned())));
        dispatcher.log("after the swap unitSECRETmadeup0042");
        drop(g);
        assert_eq!(*locked(&got), vec!["after the swap ****".to_owned()]);
    }

    // The zygote watch's blind window (image run 40015873298, 1 of 44 probe kills). The child poll
    // starts with gen 1's restart, before CEF has forked a zygote, then lists every `CHILD_POLL_MS`:
    // the zygotes were first listed by the 2nd poll (restart + ~1.01 s), while the page readied at
    // about restart + 0.72 s. A zygote killed in between was never seen, so never missed: no verdict,
    // no in-place restart, then Chromium's abort (133) at the next GPU-process death, or `CefCrashed`
    // at the next build (F-013). The ready lists the children before anyone hears of it. Either
    // zygote; the control (no kill); a second ready after the verdict; a ready that lists nothing.
    // This pins the adapter's verdict and the domain's cause; the supervisor's exit 75 on that cause
    // is the domain's and app's tests (C11b).
    #[test]
    fn a_zygote_killed_between_the_ready_and_the_next_poll_is_the_restart_cause() {
        use domain::cef_health::RestartCause;
        use domain::config::Thresholds;
        const ROOT: i32 = 1544;
        // F-013's tree (CEF 139): the GPU process's zygote, and the renderers'.
        let started = || {
            vec![
                kid(1556, ROOT, "zygote", None),
                kid(1578, 1556, "gpu-process", None),
                kid(1557, ROOT, "zygote", None),
                kid(1589, 1557, "utility", None),
                kid(1622, 1557, "renderer", Some(5)),
            ]
        };
        let event = |rx: &crossbeam_channel::Receiver<Inbound>| match rx.try_recv() {
            Ok(Inbound::Event(e)) => Some(e),
            _ => None,
        };
        // (case, CEF's children appear, the zygote killed and the child its death orphans: reparented
        // to pid 1, out of this process's tree, alive)
        let cases = [
            ("the GPU process's zygote", true, Some((1556, 1578))),
            ("the renderers' zygote", true, Some((1557, 1622))),
            ("no kill", true, None),
            ("nothing listed", false, None),
        ];
        let mut checked = 0;
        for (case, cef, kill) in cases {
            let listed: Arc<Mutex<Vec<CefChild>>> = Arc::default();
            let dead: Arc<Mutex<BTreeSet<i32>>> = Arc::default();
            // At each listing: how many events the domain could already read.
            let heard: Arc<Mutex<Vec<usize>>> = Arc::default();
            let (tx, rx) = crossbeam_channel::bounded(8);
            let tree = ProcTree {
                list: Box::new({
                    let (listed, heard, rx) = (Arc::clone(&listed), Arc::clone(&heard), rx.clone());
                    move || {
                        locked(&heard).push(rx.len());
                        locked(&listed).clone()
                    }
                }),
                alive: Box::new({
                    let dead = Arc::clone(&dead);
                    move |p| !locked(&dead).contains(&p)
                }),
                root: ROOT,
                clock: Box::new(monotonic_us),
                cpu: Box::new(thread_cpu_us),
            };
            let g = unit_adapter(unit_shared(tx, tree));
            let shared = Arc::clone(&g.shared);
            let lines: Arc<Mutex<Vec<String>>> = Arc::default();
            *locked(&shared.log) = {
                let lines = Arc::clone(&lines);
                Arc::new(move |l: &str| locked(&lines).push(l.to_owned()))
            };
            // Every verdict line, whatever generation it names (a duplicate at gen 2 counts too).
            let errors = || {
                let all: Vec<String> = locked(&lines)
                    .iter()
                    .filter(|l| l.contains("a CEF zygote is gone"))
                    .cloned()
                    .collect();
                assert!(all.iter().all(|l| l.starts_with("ERROR gen=1:")), "{all:?}");
                all.len()
            };
            // The dispatcher returns once its queue is empty.
            shared.retired.store(true, Ordering::SeqCst);
            // Gen 1's restart: its build is the newest, and the first poll finds no CEF child yet.
            locked(&shared.builds).insert(1, (1, Arc::default()));
            locked(&shared.gate).current = Some((1, 1));
            watch_poll(&shared, &(shared.tree.list)());
            // CEF forks its zygotes and the page readies, well before the next poll.
            if cef {
                *locked(&listed) = started();
            }
            shared.push(page_note(1, PageSignal::Ready));
            dispatch(&shared);
            assert_eq!(event(&rx), Some(Event::CefReady { gen: 1 }), "{case}");
            // The kill, at once. In run 40015873298 the probe's fastest ready-to-stop gaps (on other
            // exits) were 0.100–0.157 s; its kill most likely came before the 2nd poll.
            if let Some((zygote, orphan)) = kill {
                locked(&dead).insert(zygote);
                locked(&listed).retain(|k| k.pid != zygote && k.pid != orphan);
            }
            // The poll that first listed the zygotes before this fix.
            watch_poll(&shared, &(shared.tree.list)());
            let h = g.health();
            let cause = h.restart_cause(&Thresholds::default());
            if kill.is_some() {
                assert_eq!(h.unrecoverable, Some(1), "{case}: {h:?}");
                assert_eq!(cause, Some(RestartCause::ZygoteGone { gen: 1 }), "{case}");
                assert_eq!(errors(), 1, "{case}: {:?}", locked(&lines));
            } else {
                assert_eq!(h, CefHealth::default(), "{case}");
                assert_eq!(cause, None, "{case}");
                assert_eq!(errors(), 0, "{case}: {:?}", locked(&lines));
            }
            // Three listings: the two polls and the ready's, made before its event was sent.
            assert_eq!(*locked(&heard), vec![0, 0, 0], "{case}");
            // A second ready (gen 2's): listed too; the verdict stands, decided and logged once.
            locked(&shared.builds).insert(2, (2, Arc::default()));
            locked(&shared.gate).current = Some((2, 2));
            shared.push(page_note(2, PageSignal::Ready));
            dispatch(&shared);
            assert_eq!(event(&rx), Some(Event::CefReady { gen: 2 }), "{case}");
            assert_eq!(g.health(), h, "{case}: the second ready");
            assert_eq!(errors(), usize::from(kill.is_some()), "{case}: the second ready");
            assert_eq!(locked(&heard).len(), 4, "{case}: the second ready lists");
            checked += 1;
        }
        eprintln!("cases checked: {checked}");
        assert_eq!(checked, cases.len());
    }

    // The verdict decided by a ready's own listing (the PR #51 review's m1): a poll has listed the
    // zygotes, one dies, and a ready (gen 2's) comes before the next poll: a rebuild's after the GPU
    // process's zygote (F-013: the next build still signalled ready), or one already queued when the
    // renderers' zygote died. The ready decides: the verdict names the newest generation, its ERROR
    // line is logged once and before the ready is sent, and the next poll neither logs it again nor
    // stamps it anew. Either zygote.
    #[test]
    fn a_zygote_death_a_ready_lists_first_is_decided_and_logged_by_the_ready() {
        use domain::cef_health::RestartCause;
        use domain::config::Thresholds;
        const ROOT: i32 = 1544;
        let started = || {
            vec![
                kid(1556, ROOT, "zygote", None),
                kid(1578, 1556, "gpu-process", None),
                kid(1557, ROOT, "zygote", None),
                kid(1589, 1557, "utility", None),
                kid(1622, 1557, "renderer", Some(5)),
            ]
        };
        let cases = [
            ("the GPU process's zygote", 1556, 1578),
            ("the renderers' zygote", 1557, 1622),
        ];
        let mut checked = 0;
        for (case, zygote, orphan) in cases {
            let listed = Arc::new(Mutex::new(started()));
            let dead: Arc<Mutex<BTreeSet<i32>>> = Arc::default();
            let (tx, rx) = crossbeam_channel::bounded(8);
            let tree = ProcTree {
                list: Box::new({
                    let listed = Arc::clone(&listed);
                    move || locked(&listed).clone()
                }),
                alive: Box::new({
                    let dead = Arc::clone(&dead);
                    move |p| !locked(&dead).contains(&p)
                }),
                root: ROOT,
                clock: Box::new(monotonic_us),
                cpu: Box::new(thread_cpu_us),
            };
            let g = unit_adapter(unit_shared(tx, tree));
            let shared = Arc::clone(&g.shared);
            // Each line, with how many events the domain could already read when it was logged.
            let lines: Arc<Mutex<Vec<(usize, String)>>> = Arc::default();
            *locked(&shared.log) = {
                let (lines, rx) = (Arc::clone(&lines), rx.clone());
                Arc::new(move |l: &str| locked(&lines).push((rx.len(), l.to_owned())))
            };
            let verdicts = || -> Vec<(usize, String)> {
                locked(&lines)
                    .iter()
                    .filter(|(_, l)| l.contains("a CEF zygote is gone"))
                    .cloned()
                    .collect()
            };
            shared.retired.store(true, Ordering::SeqCst);
            locked(&shared.builds).insert(1, (1, Arc::default()));
            locked(&shared.gate).current = Some((1, 1));
            // A poll lists both zygotes: healthy.
            watch_poll(&shared, &(shared.tree.list)());
            assert_eq!(g.health(), CefHealth::default(), "{case}");
            // The zygote dies; gen 2's build readies before the next poll.
            locked(&dead).insert(zygote);
            locked(&listed).retain(|k| k.pid != zygote && k.pid != orphan);
            locked(&shared.builds).insert(2, (2, Arc::default()));
            locked(&shared.gate).current = Some((2, 2));
            shared.push(page_note(2, PageSignal::Ready));
            dispatch(&shared);
            let h = g.health();
            assert_eq!(h.unrecoverable, Some(2), "{case}: decided by the ready: {h:?}");
            assert_eq!(
                h.restart_cause(&Thresholds::default()),
                Some(RestartCause::ZygoteGone { gen: 2 }),
                "{case}"
            );
            let v = verdicts();
            assert_eq!(v.len(), 1, "{case}: {:?}", locked(&lines));
            assert!(v[0].1.starts_with("ERROR gen=2:"), "{case}: {v:?}");
            assert_eq!(v[0].0, 0, "{case}: logged after the ready was sent");
            match rx.try_recv() {
                Ok(Inbound::Event(e)) => assert_eq!(e, Event::CefReady { gen: 2 }, "{case}"),
                other => panic!("{case}: no ready relayed: {other:?}"),
            }
            // The next poll: no second line, no new stamp.
            watch_poll(&shared, &(shared.tree.list)());
            assert_eq!(g.health(), h, "{case}: the next poll");
            assert_eq!(verdicts().len(), 1, "{case}: the next poll");
            checked += 1;
        }
        eprintln!("cases checked: {checked}");
        assert_eq!(checked, cases.len());
    }

    // Owed 55: a teardown is counted leaked once, and counted down if it returns after all, in
    // either order of the two threads.
    #[test]
    fn a_leaked_teardown_is_counted_once_and_down_when_it_returns() {
        let leaked = AtomicUsize::new(0);
        let life = Life::default();
        assert_eq!(life.leaked_after(Duration::from_millis(10), &leaked), Some(1));
        assert_eq!(leaked.load(Ordering::SeqCst), 1);
        assert!(life.finish(Duration::from_millis(5), &leaked));
        assert_eq!(leaked.load(Ordering::SeqCst), 0);
        // Returned first: never counted.
        let life = Life::default();
        assert!(!life.finish(Duration::from_millis(5), &leaked));
        assert_eq!(life.leaked_after(Duration::from_millis(10), &leaked), None);
        assert_eq!(leaked.load(Ordering::SeqCst), 0);
    }

    // Owed 82 (owner option A, 2026-10-24): the held-teardown hook's budget. Each held teardown takes
    // one, none is taken past it, a second arming replaces what is left (it does not add), 0 holds
    // none, and every arming says so in one line with its count.
    #[test]
    fn test_hook_the_held_teardown_budget_is_taken_once_each() {
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let shared = unit_shared(tx, ProcTree::live());
        let lines = capture_log(&shared);
        let g = unit_adapter(Arc::clone(&shared));
        let mut checked = 0;
        assert_eq!(shared.take_teardown_hold(), None, "unarmed: no teardown is held");
        checked += 1;
        // C14 case 16's two legs (`two`, `three`), then one over a budget left unspent.
        for n in [2usize, 3] {
            g.hold_next_teardowns(n);
            for left in (0..n).rev() {
                assert_eq!(shared.take_teardown_hold(), Some(left), "n={n}");
                checked += 1;
            }
            assert_eq!(shared.take_teardown_hold(), None, "n={n}: past the budget");
            checked += 1;
        }
        g.hold_next_teardowns(3);
        g.hold_next_teardowns(1);
        assert_eq!(shared.take_teardown_hold(), Some(0), "re-armed at 1 over 3 left");
        assert_eq!(shared.take_teardown_hold(), None, "re-armed at 1: one, not four");
        g.hold_next_teardowns(2);
        g.hold_next_teardowns(0);
        assert_eq!(shared.take_teardown_hold(), None, "0 holds none");
        checked += 3;
        let armed: Vec<String> = locked(&lines)
            .iter()
            .filter(|l| l.contains("test hook (Owed 82)"))
            .cloned()
            .collect();
        let want: Vec<String> = [2, 3, 3, 1, 2, 0]
            .iter()
            .map(|n| format!("WARN test hook (Owed 82): the next {n} CEF teardowns never return"))
            .collect();
        assert_eq!(armed, want);
        checked += 1;
        drop(g);
        eprintln!("held-teardown budget checks: {checked}");
        assert_eq!(checked, 12, "anti-vacuity");
    }

    /// Owed 82: one build on an empty pipeline, started on its own thread (`run_build`) as `restart`
    /// starts a page's, then retired (`retire_current`) with the reaper's watch behind it. CI's
    /// GStreamer has no cefsrc; the path under test, from the retirement to the leak count, is the
    /// same with one. The pipeline is returned too, so the caller can read the state it was left in.
    fn retire_one(g: &mut CefGraphics, gen: u64) -> (Arc<Life>, gst::Pipeline) {
        gst::init().expect("gstreamer");
        let mut live = Live {
            gen,
            build: gen,
            pipeline: gst::Pipeline::new(),
            life: Arc::new(Life::default()),
            running: false,
        };
        g.start(&mut live);
        assert!(live.running, "setup: gen={gen}'s thread started");
        let life = Arc::clone(&live.life);
        let pipeline = live.pipeline.clone();
        g.current = Some(live);
        g.retire_current();
        (life, pipeline)
    }

    /// Owed 82: `held` of `builds` retirements held by the hook, one at a time (the hook takes its
    /// budget in the order the teardowns start), each held one's line seen before the next. Then
    /// the leak count settles: the lines, the gauge and the domain's cause. A held build's pipeline is
    /// still PLAYING at its leak count, as a teardown stuck inside `set_state(NULL)` leaves cefsrc
    /// live for the reaper's kill (the PR #62 review's I2: a hold after the NULL would close the
    /// browser first, and the reaper would kill nothing); a returned one's is NULL.
    fn held_teardowns_run(held: usize, builds: u64) -> (CefHealth, Vec<String>) {
        let (tx, _rx) = crossbeam_channel::bounded(64);
        let shared = unit_shared(tx, ProcTree::live());
        let lines = capture_log(&shared);
        let mut g = unit_adapter(Arc::clone(&shared));
        // 1 s, so a returning build's thread under a loaded `cargo test` wakes and runs its empty NULL
        // well inside the reaper's wait (the PR #62 review's m2: at 100 ms it could be reported killed).
        let kill_after = Duration::from_secs(1);
        g.set_reaper_kill_after(kill_after);
        g.hold_next_teardowns(held);
        let mut lives = Vec::new();
        for gen in 1..=builds {
            lives.push(retire_one(&mut g, gen));
            let holds = |l: &Vec<String>| l.iter().filter(|l| l.contains("never returns (")).count();
            if (gen as usize) <= held {
                assert!(
                    wait_until(Duration::from_secs(5), || holds(&locked(&lines)) == gen as usize),
                    "gen={gen}: its teardown was not held"
                );
            }
        }
        // Every reaper's watch is over by its kill plus `TEARDOWN_LEAK_AFTER_MS`; wait for both
        // outcomes, the held ones counted and the others torn down (a returned teardown's thread
        // counts itself down just after the reaper sees it return).
        let settle = kill_after + Duration::from_millis(TEARDOWN_LEAK_AFTER_MS + 5_000);
        assert!(
            wait_until(settle, || locked(&shared.reports).len() == builds as usize
                && shared.leaked.load(Ordering::SeqCst) == held
                && shared.teardowns.load(Ordering::SeqCst) == held),
            "the reaper's watches did not settle: {:?}",
            locked(&lines)
        );
        let mut checked = 0;
        for (i, (life, pipeline)) in lives.iter().enumerate() {
            let returned = life.wait_done(Duration::ZERO).is_some();
            assert_eq!(returned, i >= held, "gen={}: returned={returned}", i + 1);
            // The reaper's kill reached the held ones only (no renderer here, so no pid: on the real
            // image, C14 case 16 requires at least one per held teardown, Owed 82).
            let path = locked(&shared.reports).get(&(i as u64 + 1)).cloned();
            let killed = matches!(path, Some(ReaperPath::Killed { .. }));
            assert_eq!(killed, i < held, "gen={}: {path:?}", i + 1);
            let want = if i < held {
                gst::State::Playing
            } else {
                gst::State::Null
            };
            assert_eq!(
                pipeline.current_state(),
                want,
                "gen={}: a held teardown parks before its set_state(NULL); a returned one ran it",
                i + 1
            );
            checked += 3;
        }
        assert!(
            checked > 0 && checked == 3 * builds as usize,
            "anti-vacuity: every build read ({checked})"
        );
        assert_eq!(g.teardowns_in_flight(), held, "the held ones never count down");
        let h = g.health();
        let out = locked(&lines).clone();
        drop(g);
        (h, out)
    }

    /// The domain's decision on `h`, from a supervisor on air (booted, Running).
    fn decided_on_air(h: CefHealth) -> Vec<domain::supervisor::Effect> {
        use domain::config::Thresholds;
        use domain::supervisor::Supervisor;
        let s = Supervisor::new(Thresholds::default()).decide(Event::Tick, 0).next;
        let s = s
            .decide(
                Event::BootstrapOk {
                    heartbeat_interval_ms: 5_000,
                    deadline_at: 3_600_000,
                },
                1,
            )
            .next;
        s.decide(Event::CefHealth(h), 2).effects
    }

    // Owed 82 through the real path, C14 case 16's `two` leg: owner ruling 4A (2026-10-20) accepts
    // one or two leaked teardowns until the session ends. One under `cef_leaks_to_restart` held, and
    // a retirement past the budget that returns: each held one is killed after, counted leaked with
    // its WARN and count, and the domain asks no restart of the gauge.
    #[test]
    fn test_hook_two_held_teardowns_leak_two_and_restart_nothing() {
        use domain::config::Thresholds;
        let t = Thresholds::default();
        assert_eq!(t.cef_leaks_to_restart, 3, "owner ruling 4A: restart at 3");
        let held = t.cef_leaks_to_restart - 1;
        let (h, lines) = held_teardowns_run(held, held as u64 + 1);
        let mut checked = 0;
        for k in 1..=held {
            let gen_held = format!("WARN test hook (Owed 82) gen={k}: this CEF teardown never returns");
            assert!(
                lines.iter().any(|l| l.starts_with(&gen_held)),
                "{gen_held}: {lines:?}"
            );
            let counted = format!("({k} leaked; Owed 55)");
            assert_eq!(
                lines
                    .iter()
                    .filter(|l| l.starts_with("WARN gen=") && l.ends_with(&counted))
                    .count(),
                1,
                "{counted}: {lines:?}"
            );
            checked += 2;
        }
        let past = format!("gen={}: this CEF teardown never returns", held + 1);
        assert!(
            !lines.iter().any(|l| l.contains(&past)),
            "held past the budget: {lines:?}"
        );
        assert_eq!(h.leaked_teardowns, held);
        assert_eq!(h.restart_cause(&t), None, "4A: no restart under 3");
        let effects = decided_on_air(h);
        assert!(
            effects.is_empty(),
            "on air, the gauge at {held} decides {effects:?}"
        );
        checked += 4;
        eprintln!("held-teardown checks (two): {checked}");
        assert_eq!(checked, 2 * held + 4, "anti-vacuity");
    }

    // Owed 82 through the real path, C14 case 16's `three` leg: `cef_leaks_to_restart` held. The
    // third leak is the domain's restart in place, exit 75 (EX_TEMPFAIL, C11b) with its declared
    // detail on the final beat.
    #[test]
    fn test_hook_three_held_teardowns_restart_in_place_with_exit_75() {
        use domain::cef_health::RestartCause;
        use domain::config::Thresholds;
        use domain::health::BeatState;
        use domain::lifecycle::ExitReason;
        use domain::supervisor::Effect;
        let t = Thresholds::default();
        assert_eq!(t.cef_leaks_to_restart, 3, "owner ruling 4A: restart at 3");
        let held = t.cef_leaks_to_restart;
        let (h, lines) = held_teardowns_run(held, held as u64);
        let mut checked = 0;
        for k in 1..=held {
            let counted = format!("({k} leaked; Owed 55)");
            assert_eq!(
                lines
                    .iter()
                    .filter(|l| l.starts_with("WARN gen=") && l.ends_with(&counted))
                    .count(),
                1,
                "{counted}: {lines:?}"
            );
            checked += 1;
        }
        assert_eq!(h.leaked_teardowns, 3);
        assert_eq!(
            h.restart_cause(&t),
            Some(RestartCause::LeakedTeardowns { live: 3 })
        );
        let effects = decided_on_air(h);
        assert_eq!(effects.len(), 2, "{effects:?}");
        let Effect::SendHeartbeat(beat) = &effects[0] else {
            panic!("the final beat first: {effects:?}");
        };
        assert_eq!(beat.state, BeatState::Stopped);
        let exit = beat.exit.as_ref().expect("the final beat carries the exit");
        assert_eq!(exit.reason, ExitReason::Restart);
        assert_eq!(
            exit.detail.as_ref().map(|d| d.as_str()),
            Some("restart in place: 3 CEF teardowns leaked")
        );
        assert_eq!(effects[1], Effect::Exit(75), "EX_TEMPFAIL");
        checked += 7;
        eprintln!("held-teardown checks (three): {checked}");
        assert_eq!(checked, held + 7, "anti-vacuity");
    }

    // PR #32 review I1: the profile directory is private, and a dead process's is swept. The
    // confirm pass's N2: so is one carrying this process's own pid, an earlier image's (the sweep
    // runs before this image makes its own).
    #[test]
    fn the_profile_directory_is_private_and_stale_ones_are_swept() {
        use std::os::unix::fs::PermissionsExt;
        let own = std::process::id() as i32;
        let base = std::env::temp_dir().join(format!("c11-unit-profiles-{own}"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("base");
        let dead = base.join("seazn-cef-2147483646-1");
        let earlier_image = base.join(format!("seazn-cef-{own}-1"));
        let live_other = base.join("seazn-cef-2147483645-1");
        let not_ours = [base.join("seazn-cef-notapid-2"), base.join("other-3")];
        for d in [&dead, &earlier_image] {
            std::fs::create_dir_all(d.join("Default")).expect("stale");
            std::fs::write(d.join("Default").join("History"), b"stale").expect("History");
        }
        std::fs::create_dir_all(&live_other).expect("live");
        for d in &not_ours {
            std::fs::create_dir_all(d).expect("dir");
        }
        // Every pid alive but the dead one: the earlier image's goes for its pid alone.
        let alive = |pid: i32| pid != 2_147_483_646;
        assert_eq!(sweep_stale_profiles(&base, own, alive), 2);
        assert!(!dead.exists(), "the dead process's was kept");
        assert!(!earlier_image.exists(), "this pid's earlier image's was kept");
        assert!(live_other.exists(), "a live process's was removed");
        assert!(not_ours.iter().all(|d| d.exists()));
        // Then this image's own, made after the sweep: private, and named for this pid.
        let mine = make_profile_dir(&base).expect("profile dir");
        let mode = std::fs::metadata(&mine).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let name = mine
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        assert!(name.starts_with(&format!("seazn-cef-{own}-")), "{name}");
        eprintln!("directories checked: {}", 4 + not_ours.len());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pss_is_read_from_smaps_rollup() {
        let rollup = "55d0-7ffd ---p 00000000 00:00 0 [rollup]\nRss:              116232 kB\n\
                      Pss:               48210 kB\nPss_Anon:          30000 kB\n";
        assert_eq!(pss_kb(rollup), Some(48_210));
        assert_eq!(pss_kb("Rss: 1 kB\n"), None);
    }
}
