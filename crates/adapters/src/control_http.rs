//! The live control plane (C07; spec §5): the session GET and the heartbeat POST to R1, over HTTPS,
//! with the job token as a bearer. The wire is the vendored contract `contracts/relay-job.v1.json`
//! (`tests/contract.rs` pins it and validates every beat against it).
//!
//! **Classification** (C07 brief "Status mapping", amended 2026-10-18 for the heartbeat 410):
//! - the GET: 200 with a composed session in the house envelope → `Ok`; 401, 404, 410 →
//!   `Refused`; a 200 that is JSON but not a composed session's envelope → `Refused(WrongMode)`;
//!   anything else (a 5xx, any other status, a redirect, a body that did not arrive as JSON, a network
//!   error, a timeout) → `Transient`, which the domain retries on its back-off.
//! - the POST: 200 with a heartbeat reply → `Ok(Desired)`; **410 → `Gone`, on every beat, the final
//!   one included**; everything else → `Transport`.
//!
//! **Secrets** (AGENTS.md §7). The job token goes into the `Authorization` header and nowhere else:
//! never into a URL, an error or `Debug`. No error carries a response body (the session's body holds
//! the stream key and the SRT credentials), and every error text passes through `redact`. Redirects
//! are never followed, so the token and the call never go anywhere but `APP_URL`.
use std::fmt;
use std::io::Read;
use std::time::Duration;

use app::ports::{
    ControlPlane, FetchError, HeartbeatError, InputSpec, Measured, SessionConfig, TargetSpec,
    FINAL_BEAT_TIMEOUT_MS,
};
use domain::config::OutputProfile;
use domain::health::{BeatState, GraphicsStatus, HealthReport, LinkStatus, PhaseChannel};
use domain::ladder::Rung;
use domain::lifecycle::ExitReason;
use domain::programme::ProgrammePhase;
use domain::supervisor::{Desired, Refusal};
use serde_json::{Map, Value};

use crate::redact::redact;

/// The contract's `maximum` on every integer it declares (2^53 − 1, the largest integer a JSON
/// number carries exactly; `relay-job.v1.json`). A counter is written at most at it: a larger value
/// would be a 400, and the beat lost.
const JSON_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// The contract's `exitDetail` `maxLength` (100 characters, i.e. Unicode scalar values; F-006, #966
/// review M1): within it, any text fits web's zod bound of 200 UTF-16 units.
const EXIT_DETAIL_MAX_CHARS: usize = 100;
/// The most of a reply body read. The contract's largest reply, the session, is about 1 KiB; this
/// bounds what a misbehaving server can make the compositor buffer (AGENTS.md §4: nothing unbounded).
/// A longer body reads as cut, which is transient.
const MAX_REPLY_BYTES: u64 = 64 * 1024;

/// The beat's `programmePhase` word (contract `heartbeat.programmePhase`, `phaseReply.phase`). C12's
/// loopback phase endpoint reuses it, so the two channels name a phase identically (plan,
/// "Amendments 2026-10-15"; `docs/_INDEX.md` Owed 4).
pub fn programme_phase_wire(p: ProgrammePhase) -> &'static str {
    match p {
        ProgrammePhase::Waiting => "waiting",
        ProgrammePhase::Live => "live",
        ProgrammePhase::Frozen => "frozen",
        ProgrammePhase::Reconnecting => "reconnecting",
        ProgrammePhase::Final => "final",
    }
}

fn state_wire(s: BeatState) -> &'static str {
    match s {
        BeatState::Starting => "starting",
        BeatState::Playing => "playing",
        BeatState::Stalled => "stalled",
        BeatState::Stopped => "stopped",
    }
}

fn link_wire(l: LinkStatus) -> &'static str {
    match l {
        LinkStatus::Connecting => "connecting",
        LinkStatus::Connected => "connected",
        LinkStatus::Backoff => "backoff",
    }
}

fn graphics_wire(g: GraphicsStatus) -> &'static str {
    match g {
        GraphicsStatus::Starting => "starting",
        GraphicsStatus::Ready => "ready",
        GraphicsStatus::Restarting => "restarting",
        GraphicsStatus::Failed => "failed",
    }
}

/// Owed 20 (owner 2026-10-18): the ladder has two rungs, and the contract two words.
fn rung_wire(r: Rung) -> &'static str {
    match r {
        Rung::Full => "full",
        Rung::OverlayHalfRate => "overlay_half_rate",
    }
}

fn phase_channel_wire(c: PhaseChannel) -> &'static str {
    match c {
        PhaseChannel::Loopback => "loopback",
        PhaseChannel::Web => "web",
    }
}

/// The snake_case of `ExitReason` (contract `heartbeat.exitReason`).
fn exit_reason_wire(r: ExitReason) -> &'static str {
    match r {
        ExitReason::Ended => "ended",
        ExitReason::Deadline => "deadline",
        ExitReason::Signalled => "signalled",
        ExitReason::SessionGone => "session_gone",
        ExitReason::Refused => "refused",
        ExitReason::Fatal => "fatal",
    }
}

fn int(n: u64) -> Value {
    Value::from(n.min(JSON_MAX_SAFE_INTEGER))
}

/// A measurement the contract admits: finite and not negative (`minimum: 0`). Anything else is not a
/// value we know, so the key is left out.
fn measurement(x: Option<f64>) -> Option<Value> {
    x.filter(|v| v.is_finite() && *v >= 0.0).map(Value::from)
}

/// The heartbeat body (contract `#/$defs/heartbeat`, sent bare). Every key whose value is unknown is
/// absent, never `null` and never 0: `scoreStale` and `phaseChannel` until the page reports them,
/// each `Measured` field until an adapter measures it, and `exitReason` / `exitDetail` on every beat
/// but a `Stopped` one with an exit.
pub fn heartbeat_wire(report: &HealthReport, measured: &Measured) -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    put("state", state_wire(report.state).into());
    put(
        "programmePhase",
        programme_phase_wire(report.programme_phase).into(),
    );
    put("input", link_wire(report.input).into());
    put("output", link_wire(report.output).into());
    put("graphics", graphics_wire(report.graphics).into());
    put("inputKicks", int(u64::from(report.input_kicks)));
    put("inputKickAlert", report.input_kick_alert.into());
    put("pipelineRebuilds", int(report.pipeline_rebuilds));
    put("sinkSwaps", int(report.sink_swaps));
    put("droppedFrames", int(report.dropped_frames));
    put("rung", rung_wire(report.rung).into());
    if let Some(b) = report.score_stale {
        put("scoreStale", b.into());
    }
    if let Some(c) = report.phase_channel {
        put("phaseChannel", phase_channel_wire(c).into());
    }
    if let Some(v) = measurement(measured.fps) {
        put("fps", v);
    }
    if let Some(n) = measured.bitrate_kbps {
        put("bitrateKbps", n.into());
    }
    if let Some(n) = measured.egress_bytes {
        put("egressBytes", int(n));
    }
    if let Some(n) = measured.measured_latency_ms {
        put("measuredLatencyMs", int(n));
    }
    if let Some(v) = measurement(measured.encoder_speed) {
        put("encoderSpeed", v);
    }
    if let Some(v) = measurement(measured.cpu_pct) {
        put("cpuPct", v);
    }
    if let Some(n) = measured.mem_mb {
        put("memMb", int(n));
    }
    // The exit keys ride only a `stopped` beat (the contract's if/then; web answers 400 otherwise).
    if let (BeatState::Stopped, Some(exit)) = (report.state, &report.exit) {
        put("exitReason", exit_reason_wire(exit.reason).into());
        let detail: String = redact(exit.detail.as_deref().unwrap_or(""))
            .chars()
            .take(EXIT_DETAIL_MAX_CHARS)
            .collect();
        if !detail.is_empty() {
            put("exitDetail", detail.into());
        }
    }
    Value::Object(m)
}

fn wrong_mode<T>() -> Result<T, FetchError> {
    Err(FetchError::Refused(Refusal::WrongMode))
}

fn str_at<'a>(v: &'a Value, key: &str) -> Result<&'a str, FetchError> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or(FetchError::Refused(Refusal::WrongMode))
}

fn u64_at(v: &Value, key: &str) -> Result<u64, FetchError> {
    v.get(key)
        .and_then(Value::as_u64)
        .ok_or(FetchError::Refused(Refusal::WrongMode))
}

/// The contract's `input.url` pattern, `^srt://[^\s/?#@]+:\d+$`: `srt://host:port` and nothing else.
fn is_bare_srt_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("srt://") else {
        return false;
    };
    let Some((host, port)) = rest.rsplit_once(':') else {
        return false;
    };
    !host.is_empty()
        && !host
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '@'))
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
}

/// The `delay=` value in `overlayUrl`'s query, if it appears exactly once; `None` when it is absent or
/// repeated (the contract: "its `delay=` appears exactly once").
fn single_delay(overlay_url: &str) -> Option<&str> {
    let (_, query) = overlay_url.split_once('?')?;
    let mut found = query.split('&').filter_map(|kv| kv.strip_prefix("delay="));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The output must be one of the two declared profiles (ADR 0005; web's `relayOutputProfile`, R2004),
/// never improvised from the numbers.
fn declared_profile(o: &Value) -> Result<OutputProfile, FetchError> {
    let field = |k: &str| u64_at(o, k).and_then(|n| u32::try_from(n).or(wrong_mode()));
    let got = OutputProfile {
        width: field("width")?,
        height: field("height")?,
        fps: field("fps")?,
        video_kbps: field("videoKbps")?,
    };
    [OutputProfile::P720, OutputProfile::P1080]
        .into_iter()
        .find(|p| *p == got)
        .ok_or(FetchError::Refused(Refusal::WrongMode))
}

/// A composed session from the GET's `data` (contract `#/$defs/session`), or `WrongMode`: the mode is
/// not `composed`, a composed key (`input`, `output`, `overlayUrl`, `heartbeatIntervalMs`) or a
/// required one (`sessionId`, `target`, `overlayDelayMs`) is absent or of the wrong type, the output is
/// not a declared profile, the input is not a bare SRT URL, the interval is 0, or `overlayUrl`'s
/// `delay=` is absent, repeated or different from `overlayDelayMs` (both authorities must be present
/// and agree; neither is defaulted from the other).
pub fn parse_session(v: &Value) -> Result<SessionConfig, FetchError> {
    if v.get("mode").and_then(Value::as_str) != Some("composed") {
        return wrong_mode();
    }
    let input = v.get("input").ok_or(FetchError::Refused(Refusal::WrongMode))?;
    if input.get("protocol").and_then(Value::as_str) != Some("srt") {
        return wrong_mode();
    }
    let input_url = str_at(input, "url")?;
    if !is_bare_srt_url(input_url) {
        return wrong_mode();
    }
    let target = v.get("target").ok_or(FetchError::Refused(Refusal::WrongMode))?;
    let output = declared_profile(v.get("output").ok_or(FetchError::Refused(Refusal::WrongMode))?)?;
    let heartbeat_interval_ms = u64_at(v, "heartbeatIntervalMs")?;
    if heartbeat_interval_ms == 0 {
        return wrong_mode();
    }
    let overlay_url = str_at(v, "overlayUrl")?;
    let overlay_delay_ms = u64_at(v, "overlayDelayMs")?;
    if single_delay(overlay_url) != Some(overlay_delay_ms.to_string().as_str()) {
        return wrong_mode();
    }
    let fixture_id = match v.get("fixtureId") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) => None,
        _ => return wrong_mode(),
    };
    Ok(SessionConfig {
        session_id: str_at(v, "sessionId")?.to_owned(),
        fixture_id,
        input: InputSpec {
            url: input_url.to_owned(),
            stream_id: str_at(input, "streamId")?.to_owned(),
            passphrase: str_at(input, "passphrase")?.to_owned(),
        },
        target: TargetSpec {
            url: str_at(target, "url")?.to_owned(),
            stream_key: str_at(target, "streamKey")?.to_owned(),
        },
        output,
        heartbeat_interval_ms,
        overlay_url: overlay_url.to_owned(),
        overlay_delay_ms,
    })
}

/// What a 200's body held.
enum Body {
    /// The `data` of the house envelope `{ "ok": true, "data": … }` (F-006).
    Data(Value),
    /// JSON, but not that envelope: web's answer, in a shape that is not ours.
    NotEnvelope,
    /// Not JSON at all (empty, cut mid-document or mid-transfer, a page from something in between):
    /// not web's answer.
    NotJson,
}

fn body_of(text: &str) -> Body {
    let Ok(mut doc) = serde_json::from_str::<Value>(text) else {
        return Body::NotJson;
    };
    if doc.get("ok") != Some(&Value::Bool(true)) {
        return Body::NotEnvelope;
    }
    match doc.get_mut("data") {
        Some(data) => Body::Data(data.take()),
        None => Body::NotEnvelope,
    }
}

/// A reply's status and, for a 200, its body read to at most `MAX_REPLY_BYTES` (a body cut
/// mid-transfer reads as empty). A transport failure is its redacted text: never a header and never a
/// body, so neither the token nor a session credential can reach it.
fn exchange(result: Result<ureq::Response, ureq::Error>) -> Result<(u16, String), String> {
    let resp = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Ok((code, String::new())),
        Err(e) => return Err(redact(&e.to_string())),
    };
    let code = resp.status();
    let mut body = String::new();
    if code == 200
        && resp
            .into_reader()
            .take(MAX_REPLY_BYTES)
            .read_to_string(&mut body)
            .is_err()
    {
        body.clear();
    }
    Ok((code, body))
}

/// The HTTP control plane: one agent, `APP_URL`, the session id and the job token (C12 reads both
/// from env, never argv; AGENTS.md §7).
pub struct HttpControlPlane {
    agent: ureq::Agent,
    session_url: String,
    heartbeat_url: String,
    session_id: String,
    token: String,
    timeout_ms: u64,
}

impl fmt::Debug for HttpControlPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpControlPlane")
            .field("session_url", &redact(&self.session_url))
            .field("session_id", &self.session_id)
            .field("token", &"****")
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

impl HttpControlPlane {
    /// `timeout_ms` bounds every request, connect to last byte; the final beat's is the smaller of it
    /// and `FINAL_BEAT_TIMEOUT_MS`.
    pub fn new(app_url: &str, session_id: &str, job_token: String, timeout_ms: u64) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            // Web never redirects (R8). A followed redirect would take the call somewhere APP_URL does
            // not name, so a 3xx is a status like any other.
            .redirects(0)
            .build();
        let base = app_url.trim_end_matches('/');
        let session_url = format!("{base}/api/internal/relay/sessions/{session_id}");
        Self {
            agent,
            heartbeat_url: format!("{session_url}/heartbeat"),
            session_url,
            session_id: session_id.to_owned(),
            token: job_token,
            timeout_ms,
        }
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

impl ControlPlane for HttpControlPlane {
    fn fetch_session(&mut self) -> Result<SessionConfig, FetchError> {
        let sent = self
            .agent
            .get(&self.session_url)
            .set("Authorization", &self.bearer())
            .call();
        let (code, body) =
            exchange(sent).map_err(|e| FetchError::Transient(format!("fetch transport: {e}")))?;
        match code {
            200 => {}
            401 => return Err(FetchError::Refused(Refusal::Unauthorized)),
            404 => return Err(FetchError::Refused(Refusal::NotFound)),
            410 => return Err(FetchError::Refused(Refusal::Gone)),
            other => return Err(FetchError::Transient(format!("fetch refused: status {other}"))),
        }
        match body_of(&body) {
            Body::Data(data) => parse_session(&data),
            Body::NotEnvelope => wrong_mode(),
            Body::NotJson => Err(FetchError::Transient("fetch reply: not JSON".to_owned())),
        }
    }

    fn heartbeat(&mut self, report: &HealthReport, measured: &Measured) -> Result<Desired, HeartbeatError> {
        let mut req = self
            .agent
            .post(&self.heartbeat_url)
            .set("Authorization", &self.bearer());
        if report.exit.is_some() {
            // The final beat: one attempt, never delaying `Exit` past its bound (Owed 28).
            req = req.timeout(Duration::from_millis(self.timeout_ms.min(FINAL_BEAT_TIMEOUT_MS)));
        }
        let sent = req.send_json(heartbeat_wire(report, measured));
        let (code, body) =
            exchange(sent).map_err(|e| HeartbeatError::Transport(format!("heartbeat transport: {e}")))?;
        match code {
            200 => {}
            410 => return Err(HeartbeatError::Gone),
            other => {
                return Err(HeartbeatError::Transport(format!(
                    "heartbeat refused: status {other}"
                )))
            }
        }
        let desired = match body_of(&body) {
            Body::Data(d) => match d.get("desiredState").and_then(Value::as_str) {
                Some("live") => Some(Desired::Live),
                Some("ending") => Some(Desired::Ending),
                _ => None,
            },
            Body::NotEnvelope | Body::NotJson => None,
        };
        desired.ok_or_else(|| HeartbeatError::Transport("heartbeat reply: not a heartbeatReply".to_owned()))
    }
}
