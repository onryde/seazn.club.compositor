//! The ports the event loop drives (C07: the control plane; C08 adds the rest). Plain data and
//! traits: the live implementations sit in the HTTP/media crate, which this crate never names
//! (`tests/boundary.rs`).
//!
//! Secrets (AGENTS.md §7). A session carries four credentials: the SRT stream id and passphrase
//! (Input A), the RTMPS stream key (Input B) and the overlay page's token inside `overlay_url`'s
//! query. None of them may reach a log, an error or `Debug` output, so the three structs that carry
//! them write their own `Debug`: every credential prints as `****`, and every URL as its scheme and
//! authority alone (`redacted_origin`), with no path or query.
use std::fmt;

use domain::config::OutputProfile;
use domain::health::HealthReport;
use domain::supervisor::{Desired, Refusal};

/// The final heartbeat's one attempt is bounded by this (C07 brief, "Final heartbeat with the exit
/// reason"; wave gate 1 review I4, `docs/_INDEX.md` Owed 28): the beat whose report carries an
/// `exit` never delays `Exit` by more than it. The HTTP control plane caps that beat's request at it,
/// and C08's `run()` bounds its flush by it, so both read this one value. It fits R1's stop grace
/// (D15, 20 s) beside the final hold and the drain: 10 + 7 + 2 s (`Thresholds::default()`).
pub const FINAL_BEAT_TIMEOUT_MS: u64 = 2_000;

/// The scheme and authority of `url` (`srt://host:port`, `https://host`), with every path, query
/// and fragment cut and replaced by `/…` when anything followed. `Debug` output of a session uses
/// it so that no stream key in a path and no page token in a query is ever printed. A string with
/// no `://` prints as `****`: it is not a URL this crate can show safely.
pub fn redacted_origin(url: &str) -> String {
    let Some(i) = url.find("://") else {
        return "****".to_owned();
    };
    let rest = &url[i + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    // Userinfo (`user:pass@host`) is a credential too: keep only what follows the last `@`.
    let authority = &rest[..end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let cut = if end < rest.len() { "/…" } else { "" };
    format!("{}://{host}{cut}", &url[..i])
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
    pub fps: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub egress_bytes: Option<u64>,
    pub measured_latency_ms: Option<u64>,
    pub encoder_speed: Option<f64>,
    pub cpu_pct: Option<f64>,
    pub mem_mb: Option<u64>,
}

/// The two outbound calls to R1 (spec §5): the session GET and the heartbeat POST.
pub trait ControlPlane: Send {
    fn fetch_session(&mut self) -> Result<SessionConfig, FetchError>;
    /// A report whose `exit` is `Some` is the final beat: one attempt, bounded by
    /// `FINAL_BEAT_TIMEOUT_MS`. Its result is classified exactly like any other beat's.
    fn heartbeat(&mut self, report: &HealthReport, measured: &Measured) -> Result<Desired, HeartbeatError>;
}
