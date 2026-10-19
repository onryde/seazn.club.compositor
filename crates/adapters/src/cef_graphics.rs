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
//! and the page keeps painting (F-013).
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
//! returned within `REAPER_KILL_AFTER_MS`, the watch sends SIGKILL to the build's renderer processes,
//! logs their pids at WARN and ends. Nothing here joins a thread. Page lines arrive on CEF's own UI
//! thread, so the log handler only queues them (bounded, the oldest dropped); this adapter's
//! dispatcher thread relays them.
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
//! `GST_CEF_LOG_SEVERITY` in the environment overrides it, and is warned about.
//!
//! **Guardrails** (spec §6), every `CHILD_POLL_MS` on every CEF child process (a descendant whose
//! command line carries Chromium's `--type=`): nice `CEF_NICE` on each of its threads (Linux nice is
//! per thread; the zygote's carries over to the renderers it forks), `oom_score_adj`
//! `CEF_OOM_SCORE_ADJ`, and the PSS sum against the cap.
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

use app::ports::{redacted_origin, GraphicsRenderer, Inbound};
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
/// Spec §6: CEF runs under nice 10.
pub const CEF_NICE: i32 = 10;
/// Spec §6's OOM order: CEF's children go first (C12's encoder writes −900).
pub const CEF_OOM_SCORE_ADJ: i32 = 1_000;
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
    /// Chromium's `--type=` (`renderer`, `zygote`, `gpu-process`, `utility`).
    pub kind: String,
    /// A renderer's `--renderer-client-id=`.
    pub client_id: Option<i64>,
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
    pub pss_mb: u64,
}

/// Nice `CEF_NICE` on every thread and `oom_score_adj` `CEF_OOM_SCORE_ADJ` on every child, where it
/// differs; and the children's PSS sum.
fn guard(kids: &[CefChild]) -> Guarded {
    let mut g = Guarded {
        children: kids.len(),
        ..Guarded::default()
    };
    let mut pss_kb_sum = 0u64;
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
        let want = CEF_OOM_SCORE_ADJ.to_string();
        if let Ok(now) = std::fs::read_to_string(&oom_path) {
            if now.trim() != want {
                match std::fs::write(&oom_path, &want) {
                    Ok(()) => g.oom_written += 1,
                    Err(_) => g.failures += 1,
                }
            }
        }
        pss_kb_sum += std::fs::read_to_string(format!("/proc/{}/smaps_rollup", kid.pid))
            .ok()
            .and_then(|r| pss_kb(&r))
            .unwrap_or(0);
    }
    g.pss_mb = pss_kb_sum / 1024;
    g
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

/// What the pipeline threads share with the adapter.
struct Shared {
    inst: u64,
    tx: Sender<Inbound>,
    frames: FrameStore,
    log: LogFn,
    scrubber: Mutex<Scrubber>,
    gate: Mutex<Gate>,
    notes: Mutex<VecDeque<Note>>,
    notes_cv: Condvar,
    notes_dropped: AtomicU64,
    retired: AtomicBool,
    rss_cap_mb: AtomicU64,
    /// Retired builds whose teardown has not returned.
    teardowns: AtomicUsize,
    /// Recent builds: their generation and sample counts.
    builds: Mutex<BTreeMap<u64, (u64, Arc<SampleStats>)>>,
    reports: Mutex<BTreeMap<u64, ReaperPath>>,
    last_guard: Mutex<Guarded>,
    #[cfg(feature = "test-hooks")]
    teardown_delay_ms: AtomicU64,
    #[cfg(feature = "test-hooks")]
    js_signals: AtomicBool,
}

impl Shared {
    fn log(&self, line: &str) {
        let clean = locked(&self.scrubber).scrub(line);
        (self.log)(&clean);
    }

    /// Queues a note for the dispatcher; never waits (a full queue drops its oldest).
    fn push(&self, note: Note) {
        let mut q = locked(&self.notes);
        if q.len() >= NOTE_QUEUE_CAP {
            q.pop_front();
            self.notes_dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(note);
        drop(q);
        self.notes_cv.notify_one();
    }

    /// The next note; `None` once the adapter is dropped and the queue is empty.
    fn next_note(&self) -> Option<Note> {
        let mut q = locked(&self.notes);
        loop {
            if let Some(n) = q.pop_front() {
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
}

/// One build's start, retirement and teardown, shared by its thread and its reaper watch.
#[derive(Default)]
struct Life {
    state: Mutex<(bool, Option<Duration>)>,
    cv: Condvar,
}

impl Life {
    fn stop(&self) {
        locked(&self.state).0 = true;
        self.cv.notify_all();
    }

    fn wait_stop(&self) {
        let mut s = locked(&self.state);
        while !s.0 {
            s = self.cv.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn finish(&self, took: Duration) {
        locked(&self.state).1 = Some(took);
        self.cv.notify_all();
    }

    /// How long the teardown took, if it ended within `limit` of this call.
    fn wait_done(&self, limit: Duration) -> Option<Duration> {
        let end = Instant::now() + limit;
        let mut s = locked(&self.state);
        loop {
            if let Some(took) = s.1 {
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
}

impl CefGraphics {
    /// `fps` is the programme's (`max-video-framerate`); `rss_cap_mb` is `Thresholds::cef_rss_cap_mb`.
    /// It loads cefsrc's plugin and installs the process's log handler, so build it before the loop.
    pub fn new(frames: FrameStore, tx: Sender<Inbound>, fps: u32, rss_cap_mb: u64) -> Self {
        static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
        let inst = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(Shared {
            inst,
            tx,
            frames,
            log: stderr_log(),
            scrubber: Mutex::new(Scrubber::default()),
            gate: Mutex::new(Gate::default()),
            notes: Mutex::new(VecDeque::new()),
            notes_cv: Condvar::new(),
            notes_dropped: AtomicU64::new(0),
            retired: AtomicBool::new(false),
            rss_cap_mb: AtomicU64::new(rss_cap_mb),
            teardowns: AtomicUsize::new(0),
            builds: Mutex::new(BTreeMap::new()),
            reports: Mutex::new(BTreeMap::new()),
            last_guard: Mutex::new(Guarded::default()),
            #[cfg(feature = "test-hooks")]
            teardown_delay_ms: AtomicU64::new(0),
            #[cfg(feature = "test-hooks")]
            js_signals: AtomicBool::new(false),
        });
        let g = Self {
            shared,
            fps: fps.max(1),
            current: None,
            builds: 0,
            threads: false,
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
    /// of stderr. C12 wires its log sink here. Call it before the first `restart`.
    pub fn with_log(mut self, log: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            shared.log = log;
        }
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

    /// Page lines dropped because the dispatcher's queue was full.
    pub fn notes_dropped(&self) -> u64 {
        self.shared.notes_dropped.load(Ordering::Relaxed)
    }

    /// What the last child poll did.
    pub fn last_guard(&self) -> Guarded {
        *locked(&self.shared.last_guard)
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

    /// Test hook: every later teardown waits `hold` before its `set_state(NULL)`, standing in for a
    /// teardown that does not return, so a retired page keeps painting and the gate must drop it.
    #[cfg(feature = "test-hooks")]
    pub fn delay_teardowns(&self, hold: Duration) {
        self.shared
            .teardown_delay_ms
            .store(hold.as_millis() as u64, Ordering::SeqCst);
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
                    shared.frames.set_overlay(straight);
                    drop(gate);
                    stats.written.fetch_add(1, Ordering::SeqCst);
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
    {
        let hold = shared.teardown_delay_ms.load(Ordering::SeqCst);
        if hold > 0 {
            thread::sleep(Duration::from_millis(hold));
        }
    }
    let t0 = Instant::now();
    if pipeline.set_state(gst::State::Null).is_err() {
        shared.log(&format!("gen={gen}: teardown to NULL failed"));
    }
    life.finish(t0.elapsed());
    shared.teardowns.fetch_sub(1, Ordering::SeqCst);
}

/// The reaper's watch on one retired build: the kill fallback after `REAPER_KILL_AFTER_MS`.
fn reap(shared: &Shared, life: &Life, build: u64, gen: u64) {
    if let Some(took) = life.wait_done(Duration::from_millis(REAPER_KILL_AFTER_MS)) {
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
        "WARN gen={gen}: teardown has not returned after {REAPER_KILL_AFTER_MS} ms; SIGKILL sent to \
         renderer pids {killed:?} (of {pids:?})"
    ));
    shared.report(build, ReaperPath::Killed { pids: killed });
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
            Note::RendererGone { build, gen } => {
                if shared.decide_crash(build) {
                    shared.log(&format!(
                        "gen={gen}: the renderer process exited (cefsrc reloads the page)"
                    ));
                    shared.send(Event::CefCrashed { gen });
                } else {
                    let n = storms.entry(build).or_insert(0);
                    *n += 1;
                    if (*n).is_multiple_of(STORM_LOG_EVERY) {
                        shared.log(&format!("gen={gen}: {n} more renderer exits"));
                    }
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

/// The child poll: guardrails, renderer claims and the memory cap, every `CHILD_POLL_MS`.
fn poll_children(shared: &Shared) {
    let mut failures_logged = false;
    let mut ambiguity_logged = false;
    while !shared.retired.load(Ordering::SeqCst) {
        let kids = cef_children();
        if claim(&kids) && !ambiguity_logged {
            ambiguity_logged = true;
            shared.log(
                "WARN: more than one new renderer at once (a spare renderer?): the reaper may not \
                 tell one build's renderer from the next's (F-013)",
            );
        }
        let g = guard(&kids);
        if g.failures > 0 && !failures_logged {
            failures_logged = true;
            shared.log(&format!(
                "WARN: {} nice or oom_score_adj writes on CEF's children failed",
                g.failures
            ));
        }
        *locked(&shared.last_guard) = g;
        let cap = shared.rss_cap_mb.load(Ordering::SeqCst);
        let current = locked(&shared.gate).current;
        if let Some((build, gen)) = current {
            if g.pss_mb > cap && shared.decide_crash(build) {
                shared.push(Note::Crashed {
                    gen,
                    why: format!(
                        "CEF's {} child processes use {} MB (PSS), over the {cap} MB cap",
                        g.children, g.pss_mb
                    ),
                });
            }
        }
        thread::sleep(Duration::from_millis(CHILD_POLL_MS));
    }
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

    #[test]
    fn pss_is_read_from_smaps_rollup() {
        let rollup = "55d0-7ffd ---p 00000000 00:00 0 [rollup]\nRss:              116232 kB\n\
                      Pss:               48210 kB\nPss_Anon:          30000 kB\n";
        assert_eq!(pss_kb(rollup), Some(48_210));
        assert_eq!(pss_kb("Rss: 1 kB\n"), None);
    }
}
