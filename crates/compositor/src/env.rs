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

/// Owed 82, a `test-hooks` build only (C14 case 16; owner option A, 2026-10-24): a count, `N`. The
/// live graphics adapter's next `N` retired CEF teardowns never return (`hold_next_teardowns`), so
/// the reaper kills and counts each as leaked as it would a real stuck one. Unset, empty or `0` holds
/// none; anything but ASCII digits is refused (78). A build without the feature has neither the key
/// nor the hook: it ignores the variable.
#[cfg(feature = "test-hooks")]
pub const COMPOSITOR_TEST_HOLD_TEARDOWNS: &str = "COMPOSITOR_TEST_HOLD_TEARDOWNS";

/// Owed 94, a `test-hooks` build only (C14's output-hung case; the owner, 2026-10-24, "B"): ms, `N`.
/// The live programme's sink generation 1 has its writes held for good `N` ms after its build
/// (`GstProgramme::stall_first_sink`, `SinkStall::Hold`): its total stops, the encoders keep
/// delivering and no sink error comes, so only the domain's egress backstop swaps it. Unset, empty or
/// `0` plants nothing; anything but ASCII digits is refused (78), and so is setting it with
/// `COMPOSITOR_TEST_HANG_SINK_AFTER_MS`.
#[cfg(feature = "test-hooks")]
pub const COMPOSITOR_TEST_HOLD_SINK_AFTER_MS: &str = "COMPOSITOR_TEST_HOLD_SINK_AFTER_MS";

/// Owed 94, a `test-hooks` build only: as `COMPOSITOR_TEST_HOLD_SINK_AFTER_MS`, with the sink's next
/// write hung on its streaming lock instead (`SinkStall::Hang`), so its teardown hangs too (F-001):
/// the backstop's swap must not wait on it.
#[cfg(feature = "test-hooks")]
pub const COMPOSITOR_TEST_HANG_SINK_AFTER_MS: &str = "COMPOSITOR_TEST_HANG_SINK_AFTER_MS";

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

/// Owed 94: how the planted sink stall stalls (`COMPOSITOR_TEST_HOLD_SINK_AFTER_MS`,
/// `COMPOSITOR_TEST_HANG_SINK_AFTER_MS`).
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkStallHook {
    Hold,
    Hang,
}

/// The test hooks a `test-hooks` build reads (Owed 82, Owed 94). Empty in a build without the
/// feature, so a release binary has nothing to arm. Read in both modes; only the live adapters have
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestHooks {
    /// `COMPOSITOR_TEST_HOLD_TEARDOWNS`: retired CEF teardowns to hold for good (0: none).
    #[cfg(feature = "test-hooks")]
    pub hold_teardowns: usize,
    /// Owed 94: the stall to plant on sink generation 1, and how many ms after its build.
    #[cfg(feature = "test-hooks")]
    pub sink_stall: Option<(SinkStallHook, u64)>,
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
    pub hooks: TestHooks,
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
    let required = |k: &'static str| get(k).filter(|v| !v.is_empty()).ok_or(EnvError::Missing(k));
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
        hooks: test_hooks(&get)?,
    })
}

/// Owed 82 and Owed 94: the hooks a `test-hooks` build was asked for.
#[cfg(feature = "test-hooks")]
fn test_hooks(get: &impl Fn(&str) -> Option<String>) -> Result<TestHooks, EnvError> {
    let hold_teardowns = digits(get, COMPOSITOR_TEST_HOLD_TEARDOWNS)?;
    let hold: u64 = digits(get, COMPOSITOR_TEST_HOLD_SINK_AFTER_MS)?;
    let hang: u64 = digits(get, COMPOSITOR_TEST_HANG_SINK_AFTER_MS)?;
    let sink_stall = match (hold, hang) {
        (0, 0) => None,
        (ms, 0) => Some((SinkStallHook::Hold, ms)),
        (0, ms) => Some((SinkStallHook::Hang, ms)),
        // One stall: the second key is the one refused.
        _ => return Err(EnvError::Invalid(COMPOSITOR_TEST_HANG_SINK_AFTER_MS)),
    };
    Ok(TestHooks {
        hold_teardowns,
        sink_stall,
    })
}

/// A hook's number: unset or empty is 0; ASCII digits only (`from_str` also takes a leading `+`).
#[cfg(feature = "test-hooks")]
fn digits<N: std::str::FromStr + Default>(
    get: &impl Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<N, EnvError> {
    match get(key).as_deref() {
        None | Some("") => Ok(N::default()),
        Some(v) if v.bytes().all(|b| b.is_ascii_digit()) => v.parse().map_err(|_| EnvError::Invalid(key)),
        Some(_) => Err(EnvError::Invalid(key)),
    }
}

/// A build without the feature reads no hook.
#[cfg(not(feature = "test-hooks"))]
fn test_hooks(_: &impl Fn(&str) -> Option<String>) -> Result<TestHooks, EnvError> {
    Ok(TestHooks::default())
}
