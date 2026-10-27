//! The ports the event loop drives (C07: the control plane; C08: the rest). Plain data and traits:
//! the live implementations sit in the HTTP/media crate, which this crate never names
//! (`tests/boundary.rs`), and `testkit` holds a fake for each.
//!
//! Secrets (AGENTS.md §7). A session carries four credentials: the SRT stream id and passphrase
//! (Input A), the RTMPS stream key (Input B) and the overlay page's token inside `overlay_url`'s
//! query. None of them may reach a log, an error or `Debug` output, so the three structs that carry
//! them write their own `Debug`: every credential prints as `****`, and every URL as its scheme and
//! authority alone (`redacted_origin`), with no path or query.
use std::fmt;
use std::time::Duration;

use domain::cef_health::CefHealth;
use domain::config::OutputProfile;
use domain::health::HealthReport;
use domain::ladder::Rung;
use domain::programme::{FrameChoice, ProgrammePhase};
use domain::supervisor::{Desired, Event, Refusal};

/// The final heartbeat's one attempt is bounded by this (C07 brief, "Final heartbeat with the exit
/// reason"; wave gate 1 review I4, `docs/_INDEX.md` Owed 28): the beat whose report carries an
/// `exit` never delays `Exit` by more than it. The HTTP control plane caps that beat's request at it,
/// and C08's `run()` bounds its flush by it, so both read this one value. It fits R1's stop grace
/// (D15, 20 s) beside the final hold and the drain: 10 + 7 + 2 s (`Thresholds::default()`).
pub const FINAL_BEAT_TIMEOUT_MS: u64 = 2_000;

/// The scheme and host[:port] of `url` (`srt://host:port`, `https://host`), with every path, query
/// and fragment cut and replaced by `/…` when anything followed. `Debug` output of a session uses
/// it so that no stream key in a path and no page token in a query is ever printed. Nothing of a
/// malformed URL prints (C07 review M1): with no `://`, or a scheme that is not purely alphanumeric
/// (text before it), the whole of it is `****`; a userinfo (`user:pass@`, a credential) ends at the
/// last `@` before the first `/`; and a host that is not one (empty, or a port that is not digits,
/// as a userinfo holding a `/` reads) prints as `****`.
pub fn redacted_origin(url: &str) -> String {
    let Some(i) = url.find("://") else {
        return "****".to_owned();
    };
    let scheme = &url[..i];
    if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return "****".to_owned();
    }
    let rest = &url[i + 3..];
    let before_path = &rest[..rest.find('/').unwrap_or(rest.len())];
    let host_at = before_path.rfind('@').map_or(0, |at| at + 1);
    let after_userinfo = &rest[host_at..];
    let end = after_userinfo
        .find(['/', '?', '#'])
        .unwrap_or(after_userinfo.len());
    let host = &after_userinfo[..end];
    let host = if is_host_and_port(host) { host } else { "****" };
    let cut = if end < after_userinfo.len() { "/…" } else { "" };
    format!("{scheme}://{host}{cut}")
}

/// `host` or `host:port`: a name or IPv4 (`[A-Za-z0-9.-]+`) or a bracketed IPv6 literal, and a port of
/// digits.
fn is_host_and_port(s: &str) -> bool {
    let (host_ok, port) = match s.strip_prefix('[') {
        Some(v6) => {
            let Some((addr, tail)) = v6.split_once(']') else {
                return false;
            };
            let port = match tail.strip_prefix(':') {
                Some(p) => Some(p),
                None if tail.is_empty() => None,
                None => return false,
            };
            let addr_ok = !addr.is_empty()
                && addr
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.');
            (addr_ok, port)
        }
        None => {
            let (host, port) = match s.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (s, None),
            };
            let name_ok = !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
            (name_ok, port)
        }
    };
    host_ok && port.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Cloudflare Input A's SRT playback (contract `session.input`): `url` is `srt://host:port` and
/// nothing else; the stream id and passphrase travel as their own fields.
#[derive(Clone, PartialEq, Eq)]
pub struct InputSpec {
    pub url: String,
    pub stream_id: String,
    pub passphrase: String,
}

impl fmt::Debug for InputSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputSpec")
            .field("url", &redacted_origin(&self.url))
            .field("stream_id", &"****")
            .field("passphrase", &"****")
            .finish()
    }
}

/// Cloudflare Input B's RTMPS ingest (contract `session.target`): `url` without the key.
#[derive(Clone, PartialEq, Eq)]
pub struct TargetSpec {
    pub url: String,
    pub stream_key: String,
}

impl fmt::Debug for TargetSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TargetSpec")
            .field("url", &redacted_origin(&self.url))
            .field("stream_key", &"****")
            .finish()
    }
}

/// A composed session's boot facts (contract `#/$defs/session`, the `data` of the GET).
/// `maxDurationMinutes` is deliberately NOT decoded: RELAY_DEADLINE_AT (env) is the one deadline
/// authority. `overlay_delay_ms` has one reader: the parser refuses a session whose `overlayUrl`
/// `delay=` disagrees with it.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionConfig {
    pub session_id: String,
    /// `None` once the fixture was deleted mid-session: the session outlives it (contract
    /// `session.fixtureId`, nullable; `_FINDINGS.md` F-006).
    pub fixture_id: Option<String>,
    pub input: InputSpec,
    pub target: TargetSpec,
    pub output: OutputProfile,
    pub heartbeat_interval_ms: u64,
    /// Carries the page token in its query: only ever printed without it.
    pub overlay_url: String,
    pub overlay_delay_ms: u64,
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("session_id", &self.session_id)
            .field("fixture_id", &self.fixture_id)
            .field("input", &self.input)
            .field("target", &self.target)
            .field("output", &self.output)
            .field("heartbeat_interval_ms", &self.heartbeat_interval_ms)
            .field("overlay_url", &redacted_origin(&self.overlay_url))
            .field("overlay_delay_ms", &self.overlay_delay_ms)
            .finish()
    }
}

/// The session GET's outcome when it is not a session. `Refused` is web's considered answer (the
/// domain exits); `Transient` is anything the domain retries on its bootstrap back-off. The text is
/// already redacted, and never carries the job token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Refused(Refusal),
    Transient(String),
}

/// A heartbeat's 200, the contract's `heartbeatReply`: what web wants (`desiredState`) and, once the
/// session's phone has been live, web's phone-live edge counter (`phoneLiveEdge`, Owed 108 fix (b)):
/// `None` when the reply carries none, or a value the contract does not admit (the beat still counts).
/// C08 hands `desired` to the domain as `Event::HeartbeatReply` and a present counter as
/// `Event::PhoneLiveEdge` after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    pub desired: Desired,
    pub phone_live_edge: Option<u64>,
}

/// A reply with no phone-live counter: a session with no phone, or none live yet (web omits the key).
impl From<Desired> for Reply {
    fn from(desired: Desired) -> Self {
        Self {
            desired,
            phone_live_edge: None,
        }
    }
}

/// A heartbeat's outcome when it is not a reply (amended 2026-10-18, wave gate 1 review I5;
/// `docs/_INDEX.md` Owed 29). Only a 410 is `Gone`, on every beat, the final one included: web has
/// ended the session (C08 maps it to `HeartbeatRefused(HeartbeatRefusal::Gone)`). Everything else,
/// a 401, a 404, a 5xx, a reply that is not one, a network error or a timeout, is `Transport`
/// (C08 maps it to `HeartbeatFailed`). The text is already redacted, and never carries the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatError {
    Gone,
    Transport(String),
}

/// Values measured by adapters (absent ⇒ None ⇒ omitted on the wire, never sent as 0).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Measured {
    /// The programme's REAL output rate (Owed 86): the video frames that left the encoder over the
    /// beat window, per second, never the profile's nominal `fps`. The driver logs it once a beat
    /// against the profile's floor and feeds the starvation alarm (`domain::starvation`); the
    /// heartbeat carries it as `fps`.
    pub fps: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub egress_bytes: Option<u64>,
    pub measured_latency_ms: Option<u64>,
    pub encoder_speed: Option<f64>,
    pub cpu_pct: Option<f64>,
    pub mem_mb: Option<u64>,
    /// Camera frames no tick put on air, this session (Owed 81 (i)). The driver logs it once a beat
    /// (`camera frames replaced beat=<n> total=<n>`). It is NOT on the heartbeat: the contract's
    /// `heartbeat` refuses an unknown key (a 400 loses the beat), so it waits for web to declare it
    /// (`docs/_INDEX.md` Owed 90). It counts the frames above one a camera tick, so a source faster
    /// than the programme (60 fps on 30) and a late tick thread raise it too, judder or not (the
    /// stats module's `ReplacedFrames` says how).
    pub camera_frames_replaced: Option<u64>,
}

/// This process's memory from `/proc/self/status` (the soak's memory split). Each field is absent
/// when its line is missing or unreadable, never 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcStatus {
    /// `VmRSS`, kB.
    pub vm_rss_kb: Option<u64>,
    /// `RssAnon`, kB: the heap and the threads' stacks (glibc's arenas among them).
    pub rss_anon_kb: Option<u64>,
    /// `RssFile`, kB: mapped files (the shared libraries, CEF's among them).
    pub rss_file_kb: Option<u64>,
    /// `RssShmem`, kB: shared memory.
    pub rss_shmem_kb: Option<u64>,
    /// `Threads`.
    pub threads: Option<u64>,
}

/// One kind of CEF child in one poll: how many children of that kind the poll listed (`listed`), how
/// many of them had their PSS read (`n`), and the PSS summed over those, in kB (`kb`). A kind listed
/// with none read has `n == 0`: its PSS is unknown, never 0 (PR #82 review I2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindPss {
    pub kb: u64,
    pub n: u32,
    pub listed: u32,
}

/// CEF's child processes' PSS by Chromium's `--type=`, from one child poll. A kind the poll listed no
/// child of is absent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CefPss {
    pub renderer: Option<KindPss>,
    pub gpu_process: Option<KindPss>,
    pub utility: Option<KindPss>,
    pub zygote: Option<KindPss>,
    /// A `--type=` none of the other four names.
    pub other: Option<KindPss>,
}

impl CefPss {
    /// One listed child under its `--type=`, with its PSS (kB) when it was read. Every child counts
    /// as listed. One whose PSS was not read (`None`: it ended between the listing and the read, or
    /// its `smaps_rollup` could not be read) adds no PSS and is not counted as read, so a kind
    /// listed with none read stays visible as such: the soak's verdict refuses it rather than read
    /// CEF as flat.
    pub fn add(&mut self, kind: &str, pss_kb: Option<u64>) {
        let slot = match kind {
            "renderer" => &mut self.renderer,
            "gpu-process" => &mut self.gpu_process,
            "utility" => &mut self.utility,
            "zygote" => &mut self.zygote,
            _ => &mut self.other,
        };
        let k = slot.get_or_insert_with(KindPss::default);
        k.listed = k.listed.saturating_add(1);
        if let Some(kb) = pss_kb {
            k.kb = k.kb.saturating_add(kb);
            k.n = k.n.saturating_add(1);
        }
    }

    /// Each kind, named as the memory split's line names it, in the line's order.
    pub fn kinds(&self) -> [(&'static str, Option<KindPss>); 5] {
        [
            ("renderer", self.renderer),
            ("gpu_process", self.gpu_process),
            ("utility", self.utility),
            ("zygote", self.zygote),
            ("other", self.other),
        ]
    }
}

/// The graphics adapter's last child poll: its number (1 the first) and its PSS by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CefPoll {
    pub poll: u64,
    pub pss: CefPss,
}

/// The soak's memory split, one sample: this process's status fields and, when a CEF adapter has
/// polled its children, that poll (absent with no CEF, as with the fake drivers, or before the first
/// poll).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemorySplit {
    pub status: ProcStatus,
    pub cef: Option<CefPoll>,
}

/// The two outbound calls to R1 (spec §5): the session GET and the heartbeat POST.
pub trait ControlPlane: Send {
    fn fetch_session(&mut self) -> Result<SessionConfig, FetchError>;
    /// A report whose `exit` is `Some` is the final beat: one attempt, bounded by
    /// `FINAL_BEAT_TIMEOUT_MS`. Its result is classified exactly like any other beat's.
    fn heartbeat(&mut self, report: &HealthReport, measured: &Measured) -> Result<Reply, HeartbeatError>;
    /// A second instance for the final-beat lane (C08 review round 1, the coordinator's ruling), which
    /// the control worker runs on its own thread: its own connection, so a call in flight on this
    /// instance never holds a final beat up; and every credential this instance knows or LATER learns
    /// (a fetched session's), so the final beat's text is scrubbed of them as this one's would be.
    /// It only heartbeats.
    fn final_lane(&self) -> Box<dyn ControlPlane>;
}

/// Input A's SRT pipeline (C10). `rebuild` tears down the current generation and builds `gen`;
/// `stop` ends it for good (the drain, the exit). `abandon` (C10b, Owed 50) is the domain's word that
/// it has given generation `gen` up by its own rule (the connect timeout or the stall rule): before
/// it returns, nothing more of that generation (video, audio or event) passes the adapter, save its
/// first pipeline error, still reported as `Event::InputError` until the next `rebuild` (Owed 108,
/// F-032: Cloudflare's hold-end close lands on such a build); its audio already buffered for the
/// programme is dropped too (so none of it goes on air), and the next `rebuild` tears it down. The adapter keeps no copy of those rules. A `gen` that is not the
/// current build's (one already rebuilt past, or after `stop`) changes nothing, and so does a second
/// call. It never blocks the loop: it closes a gate.
pub trait InputSource: Send {
    fn rebuild(&mut self, gen: u64, spec: &InputSpec);
    fn abandon(&mut self, gen: u64);
    fn stop(&mut self);
}

/// The programme pipeline (C09): never rebuilt. `push` hands it tick `index`'s choice, and it
/// timestamps the frame by that index, so the output stays constant-rate however the loop is paced.
pub trait ProgrammeOutput: Send {
    /// Builds and starts the pipeline once. Its error's text becomes the fatal exit's detail
    /// (`Event::Fatal`), unredacted: C07's wire redacts it.
    fn start(&mut self, profile: &OutputProfile) -> Result<(), String>;
    fn push(&mut self, index: u64, choice: FrameChoice);
    fn swap_sink(&mut self, gen: u64, target: &TargetSpec);
    fn step_down(&mut self, rung: Rung);
    fn hide_overlay(&mut self);
    fn drain(&mut self);
}

/// The overlay page in CEF (C11). `restart` loads `url` as generation `gen`. (C05b PR #22 review I1)
/// `stop` returns without waiting: it takes `restart`'s reaper teardown and kill fallback, and before
/// it returns it closes the stopped generation's slot gate, so no sample of that generation reaches
/// the overlay slot after it. It never blocks the loop (F-001: `set_state(NULL)` did not return for
/// 10 s on a hung renderer). `health` is what the adapter knows of CEF itself, one per process
/// (C11b): the driver reads it once per `tick_due` that ran a tick, and the domain decides from it
/// (`CefHealth::restart_cause`), never the adapter. It must not block: it reads counters.
pub trait GraphicsRenderer: Send {
    fn restart(&mut self, gen: u64, url: &str);
    fn stop(&mut self);
    fn health(&self) -> CefHealth;
}

/// The heartbeat's measured values (C12's `/proc` sampler plus C09's counters), sampled per beat.
pub trait Metrics: Send {
    fn sample(&mut self) -> Measured;

    /// The soak's memory split (soak 40139563554's drift, 2026-10-25) as of the last `sample`: the
    /// compositor's own memory and CEF's children's PSS by kind, which `mem_mb` (the whole tree's
    /// VmRSS) cannot tell apart. The driver logs it once a minute (`memory split …`,
    /// `driver::memory_line`). It is not part of `Measured`, so it can never reach the heartbeat, and
    /// `mem_mb` is unchanged beside it. None by default, and off Linux (no `/proc`).
    fn memory(&mut self) -> Option<MemorySplit> {
        None
    }
}

/// The compositor's programme phase, served to the overlay page on loopback (overlay templates spec
/// 2026-10-15 §4.3). `seq` rises by one per change. The adapter (C12) holds the latest value and
/// wakes long-polls; it serves `(Waiting, 0)` until the first `publish`.
pub trait PhasePublisher: Send {
    fn publish(&mut self, phase: ProgrammePhase, seq: u64);
}

/// The driver's own log lines (C12's amendment 2026-10-18; C14 cases 8 and 11 and C15 read them):
/// `overlay hidden reason=<r> gen=<g>`, `sink swap reason=<r> gen=<g>` (Owed 87),
/// `cef ready|crashed|alive gen=<n>`, `ticks skipped total=<n>` and `heartbeats dropped total=<n>`
/// (at most one of each a minute, and the rest at the exit; review round 1, item 4),
/// `cef health zygote_gone=<gen|-> leaked_teardowns=<n> gpu_process_deaths=<n>` when CEF's health
/// changes (C11b), and `run()`'s `final beat not confirmed within 2000 ms` (Owed 35). The adapter
/// (C12) writes each as `ts=<rfc3339> level=info msg=<line>` on stderr: the timestamp is the adapter's,
/// because this crate reads no clock. No line carries a credential or a fatal error's text: the
/// driver never logs either (C07 confirm pass; Owed 32).
pub trait LogSink: Send {
    fn info(&mut self, line: &str);
}

/// What the loop asks of the control worker. A `Heartbeat` whose report carries an `exit` is a final
/// beat, and the worker's queue never drops it (Owed 28). Its `Debug` is derived: `HealthReport`'s
/// fatal text is an `ExitDetail`, which prints its length alone (Owed 31).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    FetchSession,
    Heartbeat(HealthReport, Measured),
}

/// The control worker's side of the queue (`control_worker::ControlHandle`). `request` never
/// blocks the frame clock.
pub trait ControlQueue: Send {
    fn request(&mut self, req: ControlRequest);
    /// Periodic beats replaced or retired before they were sent, so far (the driver logs it; review
    /// round 1, item 4).
    fn dropped(&self) -> u64;
    /// Blocks until every final beat requested so far has been posted (its call returned, whatever
    /// its result), or until `timeout` has passed on the queue's own clock, never the HTTP layer's
    /// (DNS is unbounded inside ureq 2: C07 review I1, Owed 28). True if every one was posted. Only
    /// `run()` calls it, once, after `Exit`.
    fn flush(&mut self, timeout: Duration) -> bool;
}

/// A message into the loop: an adapter's or the worker's event, or the fetched session. Its `Debug`
/// is derived: `SessionConfig` writes its own credential-free `Debug` (C07), and an `Event::Fatal`'s
/// text is an `ExitDetail`.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Event(Event),
    Session(SessionConfig),
}

/// Every port the driver applies effects through.
pub struct Ports {
    pub input: Box<dyn InputSource>,
    pub programme: Box<dyn ProgrammeOutput>,
    pub graphics: Box<dyn GraphicsRenderer>,
    pub metrics: Box<dyn Metrics>,
    pub control: Box<dyn ControlQueue>,
    pub phase: Box<dyn PhasePublisher>,
    pub log: Box<dyn LogSink>,
}

// Trait objects can't derive `Debug`, and C01's `missing_debug_implementations` under `-D warnings`
// makes a missing one fatal. The names alone: nothing a port holds is printed.
impl fmt::Debug for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ports { input, programme, graphics, metrics, control, phase, log }")
    }
}
