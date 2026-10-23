//! The process's stderr log and the decorators that write the lines C14, C15 and the C17 runbook read
//! (C12 amendments 2026-10-18 and 2026-10-19). Every line is `ts=<RFC 3339, ms, UTC> level=<l>
//! msg=<text>`, one line per message, and every text passes the process's `Scrubber` first: the job
//! token from the start, and a fetched session's four credentials once `LoggedPlane` has seen it
//! (Owed 32: a stream key in a GStreamer error follows no marker, so `redact` alone cannot see it).
//!
//! - `LoggedProgramme`: `programme started` on a successful `start`, `programme start failed: <text>`
//!   (scrubbed) on a failed one, and `sink swap gen=<n> target=<redacted>` on `swap_sink`. Nothing on
//!   `hide_overlay`: the driver logs the one `overlay hidden reason=<r> gen=<g>` line per hide.
//! - `LoggedGraphics`: `graphics restart gen=<n>` and `graphics stop`.
//! - `LoggedPlane`: a final beat whose call returned an error, with its text (C07 has scrubbed it; it
//!   is scrubbed again here), on the final lane too (C08 review round 1: final beats run on
//!   `final_lane()`'s instance, so the decorator wraps that instance as well).
//! - `Logger::log_sink`: the driver's own lines (`app::ports::LogSink`), in both driver modes.
use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use adapters::redact::Scrubber;
use app::ports::{
    ControlPlane, FetchError, GraphicsRenderer, HeartbeatError, LogSink, Measured, ProgrammeOutput,
    SessionConfig, TargetSpec,
};
use domain::cef_health::CefHealth;
use domain::config::OutputProfile;
use domain::health::HealthReport;
use domain::ladder::Rung;
use domain::programme::FrameChoice;
use domain::supervisor::Desired;
use time::OffsetDateTime;

use crate::clock::Clock;

fn locked<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `ms` (epoch milliseconds) as RFC 3339 in UTC with exactly three fractional digits,
/// `2026-10-14T18:30:00.250Z`.
pub fn rfc3339_ms(ms: u64) -> String {
    let nanos = i128::from(ms) * 1_000_000;
    match OffsetDateTime::from_unix_timestamp_nanos(nanos) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second(),
            t.millisecond()
        ),
        Err(_) => format!("epoch+{ms}ms"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn word(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

struct Inner {
    clock: Arc<Clock>,
    out: Mutex<Box<dyn Write + Send>>,
    scrubber: Mutex<Scrubber>,
}

/// The process's log: a cloneable handle onto one writer (stderr, or a test's buffer), one clock and
/// one scrubber.
#[derive(Clone)]
pub struct Logger(Arc<Inner>);

impl fmt::Debug for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Logger")
            .field("scrubber", &*locked(&self.0.scrubber))
            .finish_non_exhaustive()
    }
}

impl Logger {
    pub fn stderr(clock: Arc<Clock>) -> Self {
        Self::to_writer(clock, Box::new(std::io::stderr()))
    }

    pub fn to_writer(clock: Arc<Clock>, out: Box<dyn Write + Send>) -> Self {
        Self(Arc::new(Inner {
            clock,
            out: Mutex::new(out),
            scrubber: Mutex::new(Scrubber::default()),
        }))
    }

    /// Masks `token` (the job token) in every later line.
    pub fn learn_token(&self, token: &str) {
        locked(&self.0.scrubber).add(token);
    }

    /// Masks a fetched session's four credentials in every later line (`Scrubber::add_session`, the
    /// list C07's plane learns too).
    pub fn learn_session(&self, s: &SessionConfig) {
        locked(&self.0.scrubber).add_session(s);
    }

    pub fn scrub(&self, text: &str) -> String {
        locked(&self.0.scrubber).scrub(text)
    }

    pub fn line(&self, level: Level, msg: &str) {
        // One line per message: an embedded newline (a GStreamer debug text) would forge a new one.
        let msg = self.scrub(msg).replace('\r', "\\r").replace('\n', "\\n");
        let ts = rfc3339_ms(self.0.clock.now_ms());
        let mut out = locked(&self.0.out);
        // A closed stderr loses the line; there is nowhere else to say so.
        let _ = writeln!(out, "ts={ts} level={} msg={msg}", level.word());
        let _ = out.flush();
    }

    pub fn info(&self, msg: &str) {
        self.line(Level::Info, msg);
    }

    pub fn warn(&self, msg: &str) {
        self.line(Level::Warn, msg);
    }

    pub fn error(&self, msg: &str) {
        self.line(Level::Error, msg);
    }

    /// The sink the adapters' `with_log` takes. Their lines are already scrubbed of what each adapter
    /// knows; they pass this scrubber too.
    pub fn adapter_sink(&self) -> Arc<dyn Fn(&str) + Send + Sync> {
        let l = self.clone();
        Arc::new(move |line: &str| l.info(line))
    }

    /// The driver's `LogSink` (`Ports.log`).
    pub fn log_sink(&self) -> Box<dyn LogSink> {
        Box::new(DriverLog(self.clone()))
    }
}

#[derive(Debug)]
struct DriverLog(Logger);

impl LogSink for DriverLog {
    fn info(&mut self, line: &str) {
        self.0.info(line);
    }
}

/// Logs the programme's lifecycle and delegates every call.
pub struct LoggedProgramme {
    inner: Box<dyn ProgrammeOutput>,
    log: Logger,
}

impl fmt::Debug for LoggedProgramme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoggedProgramme")
    }
}

impl LoggedProgramme {
    pub fn new(inner: Box<dyn ProgrammeOutput>, log: Logger) -> Self {
        Self { inner, log }
    }
}

impl ProgrammeOutput for LoggedProgramme {
    fn start(&mut self, profile: &OutputProfile) -> Result<(), String> {
        let r = self.inner.start(profile);
        match &r {
            Ok(()) => self.log.info("programme started"),
            // The domain gets the text verbatim (C07's wire redacts it); the log gets it scrubbed.
            Err(text) => self.log.error(&format!("programme start failed: {text}")),
        }
        r
    }

    fn push(&mut self, index: u64, choice: FrameChoice) {
        self.inner.push(index, choice);
    }

    fn swap_sink(&mut self, gen: u64, target: &TargetSpec) {
        // The URL alone, redacted; the stream key is a field of its own and is never printed.
        self.log.info(&format!(
            "sink swap gen={gen} target={}",
            adapters::redact(&target.url)
        ));
        self.inner.swap_sink(gen, target);
    }

    fn step_down(&mut self, rung: Rung) {
        self.inner.step_down(rung);
    }

    fn hide_overlay(&mut self) {
        self.inner.hide_overlay();
    }

    fn drain(&mut self) {
        self.inner.drain();
    }
}

/// Logs each graphics restart and stop, and delegates every call.
pub struct LoggedGraphics {
    inner: Box<dyn GraphicsRenderer>,
    log: Logger,
}

impl fmt::Debug for LoggedGraphics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoggedGraphics")
    }
}

impl LoggedGraphics {
    pub fn new(inner: Box<dyn GraphicsRenderer>, log: Logger) -> Self {
        Self { inner, log }
    }
}

impl GraphicsRenderer for LoggedGraphics {
    fn restart(&mut self, gen: u64, url: &str) {
        self.log.info(&format!("graphics restart gen={gen}"));
        self.inner.restart(gen, url);
    }

    fn stop(&mut self) {
        self.log.info("graphics stop");
        self.inner.stop();
    }

    fn health(&self) -> CefHealth {
        self.inner.health()
    }
}

/// Teaches the log each fetched session's credentials, and logs a final beat that failed.
pub struct LoggedPlane {
    inner: Box<dyn ControlPlane>,
    log: Logger,
}

impl fmt::Debug for LoggedPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoggedPlane")
    }
}

impl LoggedPlane {
    pub fn new(inner: Box<dyn ControlPlane>, log: Logger) -> Self {
        Self { inner, log }
    }
}

impl ControlPlane for LoggedPlane {
    fn fetch_session(&mut self) -> Result<SessionConfig, FetchError> {
        let r = self.inner.fetch_session();
        if let Ok(s) = &r {
            self.log.learn_session(s);
        }
        r
    }

    fn heartbeat(&mut self, report: &HealthReport, measured: &Measured) -> Result<Desired, HeartbeatError> {
        let r = self.inner.heartbeat(report, measured);
        if report.exit.is_some() {
            match &r {
                Ok(_) => {}
                Err(HeartbeatError::Gone) => self
                    .log
                    .warn("final beat failed: web answered 410 (session gone)"),
                Err(HeartbeatError::Transport(text)) => self.log.warn(&format!("final beat failed: {text}")),
            }
        }
        r
    }

    fn final_lane(&self) -> Box<dyn ControlPlane> {
        Box::new(Self {
            inner: self.inner.final_lane(),
            log: self.log.clone(),
        })
    }
}
