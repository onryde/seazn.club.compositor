//! The Machine's environment (C12): the four values R1's runner driver sets (`SESSION_ID`,
//! `JOB_TOKEN`, `APP_URL`, `RELAY_DEADLINE_AT`), the driver mode, and the fake mode's
//! knobs. `parse` is pure: the binary hands it `std::env`, a test a table. An `EnvError` (exit 78,
//! EX_CONFIG) names the key it is about and never a value: the job token is one of them (AGENTS.md §7;
//! C07 review, Owed 32: the token comes from env, never argv).
use std::fmt;

use adapters::phase_http::origin_of;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const SESSION_ID: &str = "SESSION_ID";
pub const JOB_TOKEN: &str = "JOB_TOKEN";
pub const APP_URL: &str = "APP_URL";
pub const RELAY_DEADLINE_AT: &str = "RELAY_DEADLINE_AT";
pub const COMPOSITOR_DRIVERS: &str = "COMPOSITOR_DRIVERS";
/// Fake mode only: `http` runs the real `HttpControlPlane` against `APP_URL` (a test's fake control
/// server) instead of the scripted plane, so the smoke runs can read every POST the binary sends.
pub const COMPOSITOR_FAKE_CONTROL: &str = "COMPOSITOR_FAKE_CONTROL";
/// Fake mode only: the fake programme's `start` fails with this text (`ProgrammeStartFailed`).
pub const COMPOSITOR_FAKE_START_FAIL: &str = "COMPOSITOR_FAKE_START_FAIL";
/// Fake mode only: `1` makes the fake graphics report a dead CEF zygote (generation 1) from the start,
/// which the domain acts on at the first Running tick: the in-place restart, exit 75 (C11b).
pub const COMPOSITOR_FAKE_ZYGOTE_GONE: &str = "COMPOSITOR_FAKE_ZYGOTE_GONE";

/// The four keys every run needs, in R1's order.
pub const REQUIRED: [&str; 4] = [SESSION_ID, JOB_TOKEN, APP_URL, RELAY_DEADLINE_AT];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drivers {
    Live,
    Fake,
}

/// The fake mode's knobs (C12 smoke runs). Read only when `drivers` is `Fake`: a live Machine never
/// sees one, and a live run ignores them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FakeKnobs {
    pub control_http: bool,
    pub start_fail: Option<String>,
    pub zygote_gone: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Env {
    pub session_id: String,
    pub job_token: String,
    pub app_url: String,
    /// APP_URL's origin in the form CEF sends as `Origin` (F-009: scheme + host, the default port
    /// omitted, no trailing slash): the phase endpoint's allowed origin until a page is loaded.
    pub app_origin: String,
    pub deadline_ms: u64,
    pub drivers: Drivers,
    pub fake: FakeKnobs,
}

// The token prints as `****` (AGENTS.md §7); a fake start-failure text may carry a planted credential.
impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Env")
            .field("session_id", &self.session_id)
            .field("job_token", &"****")
            .field("app_origin", &self.app_origin)
            .field("deadline_ms", &self.deadline_ms)
            .field("drivers", &self.drivers)
            .finish_non_exhaustive()
    }
}

/// Why the environment was refused. Every variant names a key, never a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvError {
    /// Unset, or set to the empty string (an unset secret read as `""`).
    Missing(&'static str),
    /// Set, but not a value this key takes (a deadline that is not RFC 3339, an APP_URL with no origin).
    Invalid(&'static str),
    /// `COMPOSITOR_DRIVERS` unset in a release build: a production image never silently runs fakes.
    DriversUnset,
    DriversUnknown,
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(k) => write!(f, "{k} must be set"),
            Self::Invalid(k) => write!(f, "{k} is not a valid value"),
            Self::DriversUnset => f.write_str("COMPOSITOR_DRIVERS must be set (live|fake)"),
            Self::DriversUnknown => f.write_str("COMPOSITOR_DRIVERS must be live or fake"),
        }
    }
}

impl std::error::Error for EnvError {}

/// `RELAY_DEADLINE_AT` (`Date.toISOString()`, e.g. `2026-10-14T18:30:00.000Z`) as epoch milliseconds.
/// Any RFC 3339 instant parses; one before the epoch does not.
pub fn parse_deadline(s: &str) -> Option<u64> {
    let t = OffsetDateTime::parse(s, &Rfc3339).ok()?;
    u64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

/// The environment, from `get` (a key's value, `None` if unset). `release` refuses an unset
/// `COMPOSITOR_DRIVERS`; a debug build defaults it to `fake`.
pub fn parse(get: impl Fn(&str) -> Option<String>, release: bool) -> Result<Env, EnvError> {
    let required = |k: &'static str| get(k)/*MUT*/.ok_or(EnvError::Missing(k));
    let session_id = required(SESSION_ID)?;
    let job_token = required(JOB_TOKEN)?;
    let app_url = required(APP_URL)?;
    let deadline = required(RELAY_DEADLINE_AT)?;
    let deadline_ms = parse_deadline(&deadline).ok_or(EnvError::Invalid(RELAY_DEADLINE_AT))?;
    let app_origin = origin_of(&app_url).ok_or(EnvError::Invalid(APP_URL))?;
    let drivers = match get(COMPOSITOR_DRIVERS).as_deref() {
        Some("live") => Drivers::Live,
        Some("fake") => Drivers::Fake,
        None | Some("") if release => return Err(EnvError::DriversUnset),
        None | Some("") => Drivers::Fake,
        Some(_) => return Err(EnvError::DriversUnknown),
    };
    let fake = match drivers {
        Drivers::Live => FakeKnobs::default(),
        Drivers::Fake => FakeKnobs {
            control_http: match get(COMPOSITOR_FAKE_CONTROL).as_deref() {
                None | Some("") | Some("scripted") => false,
                Some("http") => true,
                Some(_) => return Err(EnvError::Invalid(COMPOSITOR_FAKE_CONTROL)),
            },
            start_fail: get(COMPOSITOR_FAKE_START_FAIL).filter(|v| !v.is_empty()),
            zygote_gone: match get(COMPOSITOR_FAKE_ZYGOTE_GONE).as_deref() {
                None | Some("") | Some("0") => false,
                Some("1") => true,
                Some(_) => return Err(EnvError::Invalid(COMPOSITOR_FAKE_ZYGOTE_GONE)),
            },
        },
    };
    Ok(Env {
        session_id,
        job_token,
        app_url,
        app_origin,
        deadline_ms,
        drivers,
        fake,
    })
}
