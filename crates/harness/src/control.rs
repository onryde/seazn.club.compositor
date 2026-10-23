//! fake-control (C13 brief, Files): R1's two relay routes, the harness's score feed and overlay page,
//! and the case scripts' controls, on one port.
//!
//! - `GET /api/internal/relay/sessions/{sid}`: the vendored `session-composed-720p.json`, rewritten
//!   for the harness ([`rewrite_session`]), in the house envelope `{"ok":true,"data":…}` (F-006).
//! - `POST /api/internal/relay/sessions/{sid}/heartbeat`: appends `{"received_ms","body"}` to
//!   `heartbeats.jsonl` and answers `{"ok":true,"data":{"desiredState":…}}`.
//! - Both check the bearer against `JOB_TOKEN` first (401), then the session id (404), as web does.
//! - `POST /control/desired?state=ending|live`, `POST /control/score-down?ms=N`,
//!   `GET /control/timeline` (the whole score timeline, absolute, for the checker) and
//!   `GET /control/state`.
//! - `GET /score.json`: the events that have happened by now (a live score feed reveals an event
//!   when it happens), or 503 while the feed is down. The page polls it every second.
//! - `GET /overlay-test.html` and `GET /overlay/fixtures/{id}`: the harness page (the rewrite keeps
//!   the fixture's path, so the compositor asks for the second).
//!
//! **Secrets.** Nothing here logs a header, a query string or a body: the log line is the method,
//! the path without its query, and the status.
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{json, Value};

/// The relay routes' prefix (R1, `seazn.club`'s internal relay API).
pub const SESSIONS: &str = "/api/internal/relay/sessions/";
/// The largest heartbeat body read (a beat is a few hundred bytes).
pub const MAX_BODY: u64 = 64 * 1024;

/// Where the rewrite points the session: each origin is `scheme://host:port`.
#[derive(Clone, PartialEq, Eq)]
pub struct RewriteMap {
    pub input_origin: String,
    pub target_origin: String,
    pub overlay_origin: String,
    /// MediaMTX's read stream id for the camera's path (`read:cam`).
    pub stream_id: String,
    /// The destination path's last part (`dest`, so the compositor publishes `live/dest`).
    pub stream_key: String,
    /// The camera path's SRT read passphrase (random per run).
    pub passphrase: String,
}

impl fmt::Debug for RewriteMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RewriteMap")
            .field("input_origin", &self.input_origin)
            .field("target_origin", &self.target_origin)
            .field("overlay_origin", &self.overlay_origin)
            .field("stream_id", &self.stream_id)
            .field("stream_key", &self.stream_key)
            .field("passphrase", &"****")
            .finish()
    }
}

/// `scheme://authority` and the rest (path, query, fragment), or `None` for a URL with no authority.
pub fn split_origin(url: &str) -> Option<(&str, &str)> {
    let scheme_end = url.find("://")?;
    if scheme_end == 0 {
        return None;
    }
    let after = &url[scheme_end + 3..];
    let len = after.find(['/', '?', '#']).unwrap_or(after.len());
    if len == 0 {
        return None;
    }
    Some(url.split_at(scheme_end + 3 + len))
}

/// The query string of a URL (after the first `?`), or "".
pub fn query_of(url: &str) -> &str {
    url.split_once('?').map_or("", |(_, q)| q)
}

/// `url` with its scheme, host and port replaced by `origin`'s; the path and every query parameter
/// kept as they were. Errors never quote the URL (an overlay URL carries the page token).
pub fn rewrite_origin(url: &str, origin: &str) -> Result<String, &'static str> {
    let (_, rest) = split_origin(url).ok_or("not an absolute URL")?;
    let (o, tail) = split_origin(origin).ok_or("the new origin is not scheme://host:port")?;
    if !tail.is_empty() {
        return Err("the new origin carries a path or a query");
    }
    Ok(format!("{o}{rest}"))
}

fn set_str(v: &mut Value, path: &[&str], value: &str) -> Result<(), String> {
    let mut at = v;
    for k in &path[..path.len() - 1] {
        at = at
            .get_mut(*k)
            .ok_or_else(|| format!("the session has no {}", path.join(".")))?;
    }
    let last = path[path.len() - 1];
    let slot = at
        .get_mut(last)
        .ok_or_else(|| format!("the session has no {}", path.join(".")))?;
    *slot = Value::String(value.to_owned());
    Ok(())
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Result<&'a str, String> {
    let mut at = v;
    for k in path {
        at = at
            .get(*k)
            .ok_or_else(|| format!("the session has no {}", path.join(".")))?;
    }
    at.as_str()
        .ok_or_else(|| format!("the session's {} is not a string", path.join(".")))
}

/// The vendored session, pointed at the harness: `sessionId` is the run's; `input.url`,
/// `target.url` and `overlayUrl` keep everything but their origin; MediaMTX's stream id, path key
/// and read passphrase replace Cloudflare's. Refuses a rewrite that changed the overlay URL's query.
pub fn rewrite_session(fixture: &Value, session_id: &str, map: &RewriteMap) -> Result<Value, String> {
    let mut s = fixture.clone();
    let field = |e: &'static str, what: &str| format!("{what}: {e}");
    let input = rewrite_origin(str_at(fixture, &["input", "url"])?, &map.input_origin)
        .map_err(|e| field(e, "input.url"))?;
    let target = rewrite_origin(str_at(fixture, &["target", "url"])?, &map.target_origin)
        .map_err(|e| field(e, "target.url"))?;
    let old_overlay = str_at(fixture, &["overlayUrl"])?;
    let overlay = rewrite_origin(old_overlay, &map.overlay_origin).map_err(|e| field(e, "overlayUrl"))?;
    if query_of(&overlay) != query_of(old_overlay) || query_of(old_overlay).is_empty() {
        return Err("overlayUrl: the rewrite changed its query".into());
    }
    set_str(&mut s, &["sessionId"], session_id)?;
    set_str(&mut s, &["input", "url"], &input)?;
    set_str(&mut s, &["input", "streamId"], &map.stream_id)?;
    set_str(&mut s, &["input", "passphrase"], &map.passphrase)?;
    set_str(&mut s, &["target", "url"], &target)?;
    set_str(&mut s, &["target", "streamKey"], &map.stream_key)?;
    set_str(&mut s, &["overlayUrl"], &overlay)?;
    Ok(s)
}

/// The scripted timeline (`harness/pages/score.json`): `(index, offset from the run's start)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedTimeline {
    pub events: Vec<(u16, i64)>,
}

impl ScriptedTimeline {
    /// Parses `{"events":[{"index":1,"offset_ms":3000},…]}`: indexes 1, 2, 3… in order, offsets
    /// rising, at least one event.
    pub fn parse(v: &Value) -> Result<Self, String> {
        let events = v
            .get("events")
            .and_then(Value::as_array)
            .ok_or("score.json: no events array")?;
        let mut out = Vec::with_capacity(events.len());
        for (n, e) in events.iter().enumerate() {
            let index = e.get("index").and_then(Value::as_u64);
            let offset = e.get("offset_ms").and_then(Value::as_i64);
            match (index, offset) {
                (Some(i), Some(o)) if i == n as u64 + 1 && o >= 0 && u16::try_from(i).is_ok() => {
                    if out.last().is_some_and(|(_, prev)| *prev >= o) {
                        return Err(format!("score.json: event {i}'s offset does not rise"));
                    }
                    out.push((i as u16, o));
                }
                _ => {
                    return Err(format!(
                        "score.json: event {} is not index {} with an offset",
                        n + 1,
                        n + 1
                    ))
                }
            }
        }
        if out.is_empty() {
            return Err("score.json: no events".into());
        }
        Ok(Self { events: out })
    }

    fn events_json(&self, start_ms: i64, until_ms: Option<i64>) -> Vec<Value> {
        self.events
            .iter()
            .map(|(i, o)| (*i, start_ms + o))
            .filter(|(_, at)| until_ms.is_none_or(|u| *at <= u))
            .map(|(i, at)| json!({"index": i, "at": at}))
            .collect()
    }
}

/// An HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Answer {
    fn json(status: u16, v: &Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: v.to_string().into_bytes(),
        }
    }
}

#[derive(Debug)]
struct State {
    desired: &'static str,
    score_down_until_ms: i64,
    beats: u64,
    beats_refused: u64,
    fetches: u64,
}

/// fake-control's routes and state.
pub struct Fake {
    token: String,
    session_id: String,
    session: Value,
    page: Vec<u8>,
    timeline: ScriptedTimeline,
    start_ms: i64,
    delay_ms: i64,
    beats_out: Mutex<Box<dyn Write + Send>>,
    state: Mutex<State>,
}

impl fmt::Debug for Fake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fake")
            .field("start_ms", &self.start_ms)
            .field("delay_ms", &self.delay_ms)
            .finish()
    }
}

fn query_map(url: &str) -> BTreeMap<&str, &str> {
    query_of(url)
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .collect()
}

impl Fake {
    /// `session` is the rewritten session; its `overlayDelayMs` is the timeline's delay.
    pub fn new(
        token: String,
        session: Value,
        page: Vec<u8>,
        timeline: ScriptedTimeline,
        start_ms: i64,
        beats_out: Box<dyn Write + Send>,
    ) -> Result<Self, String> {
        if token.is_empty() {
            return Err("JOB_TOKEN is empty".into());
        }
        let session_id = str_at(&session, &["sessionId"])?.to_owned();
        let delay_ms = session
            .get("overlayDelayMs")
            .and_then(Value::as_i64)
            .ok_or("the session has no overlayDelayMs")?;
        Ok(Self {
            token,
            session_id,
            session,
            page,
            timeline,
            start_ms,
            delay_ms,
            beats_out: Mutex::new(beats_out),
            state: Mutex::new(State {
                desired: "live",
                score_down_until_ms: 0,
                beats: 0,
                beats_refused: 0,
                fetches: 0,
            }),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The whole timeline, absolute (`GET /control/timeline`; the checker's `--timeline`).
    pub fn timeline_json(&self) -> Value {
        json!({"start_wall_ms": self.start_ms, "delay_ms": self.delay_ms,
               "events": self.timeline.events_json(self.start_ms, None)})
    }

    fn relay(
        &self,
        method: &str,
        path: &str,
        authorization: Option<&str>,
        body: &[u8],
        now_ms: i64,
    ) -> Answer {
        if authorization != Some(format!("Bearer {}", self.token).as_str()) {
            return Answer::json(401, &json!({"ok": false, "code": "UNAUTHORIZED"}));
        }
        let rest = &path[SESSIONS.len()..];
        let (sid, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if sid != self.session_id {
            return Answer::json(404, &json!({"ok": false, "code": "NOT_FOUND"}));
        }
        match (method, tail) {
            ("GET", "") => {
                self.state().fetches += 1;
                Answer::json(200, &json!({"ok": true, "data": self.session}))
            }
            ("POST", "heartbeat") => {
                let parsed = serde_json::from_slice::<Value>(body)
                    .ok()
                    .filter(Value::is_object);
                let Some(beat) = parsed else {
                    self.state().beats_refused += 1;
                    return Answer::json(400, &json!({"ok": false, "code": "VALIDATION_ERROR"}));
                };
                let line = json!({"received_ms": now_ms, "body": beat});
                {
                    let mut out = self.beats_out.lock().unwrap_or_else(PoisonError::into_inner);
                    let _ = writeln!(out, "{line}");
                    let _ = out.flush();
                }
                let mut st = self.state();
                st.beats += 1;
                Answer::json(200, &json!({"ok": true, "data": {"desiredState": st.desired}}))
            }
            _ => Answer::json(404, &json!({"ok": false, "code": "NOT_FOUND"})),
        }
    }

    /// One request. `url` is the request target (path and query).
    pub fn handle(
        &self,
        method: &str,
        url: &str,
        authorization: Option<&str>,
        body: &[u8],
        now_ms: i64,
    ) -> Answer {
        let path = url.split('?').next().unwrap_or("");
        let q = query_map(url);
        if path.starts_with(SESSIONS) {
            return self.relay(method, path, authorization, body, now_ms);
        }
        let bad = |what: &str| Answer::json(400, &json!({"ok": false, "error": what}));
        match (method, path) {
            ("POST", "/control/desired") => match q.get("state") {
                Some(&"ending") | Some(&"live") => {
                    let d = if q.get("state") == Some(&"ending") {
                        "ending"
                    } else {
                        "live"
                    };
                    self.state().desired = d;
                    Answer::json(200, &json!({"desiredState": d}))
                }
                _ => bad("state must be ending or live"),
            },
            ("POST", "/control/score-down") => match q.get("ms").and_then(|v| v.parse::<i64>().ok()) {
                Some(ms) if ms > 0 => {
                    let until = now_ms + ms;
                    self.state().score_down_until_ms = until;
                    Answer::json(200, &json!({"score_down_until_ms": until}))
                }
                _ => bad("ms must be a positive integer"),
            },
            ("GET", "/control/timeline") => Answer::json(200, &self.timeline_json()),
            ("GET", "/control/state") => {
                let st = self.state();
                Answer::json(
                    200,
                    &json!({"desiredState": st.desired, "beats": st.beats, "beats_refused": st.beats_refused,
                            "session_fetches": st.fetches, "score_down_until_ms": st.score_down_until_ms}),
                )
            }
            ("GET", "/score.json") => {
                if now_ms < self.state().score_down_until_ms {
                    return Answer::json(503, &json!({"ok": false, "code": "SCORE_FEED_DOWN"}));
                }
                Answer::json(
                    200,
                    &json!({"start_wall_ms": self.start_ms, "events": self.timeline.events_json(self.start_ms, Some(now_ms))}),
                )
            }
            ("GET", "/overlay-test.html") => self.page(),
            ("GET", p) if p.starts_with("/overlay/fixtures/") => self.page(),
            ("GET", "/healthz") => Answer {
                status: 200,
                content_type: "text/plain",
                body: b"ok\n".to_vec(),
            },
            _ => Answer::json(404, &json!({"ok": false, "code": "NOT_FOUND"})),
        }
    }

    fn page(&self) -> Answer {
        Answer {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: self.page.clone(),
        }
    }
}

/// The request's log line: the method, the path with no query and with the session id masked (the
/// run's `SESSION_ID` is never echoed), and the status.
pub fn log_line(method: &str, url: &str, status: u16) -> String {
    let path = url.split('?').next().unwrap_or("");
    let shown = match path.strip_prefix(SESSIONS) {
        Some(rest) => match rest.split_once('/') {
            Some((_, tail)) => format!("{SESSIONS}{{sid}}/{tail}"),
            None => format!("{SESSIONS}{{sid}}"),
        },
        None => path.to_owned(),
    };
    format!("fake-control {method} {shown} {status}")
}

/// Serves `fake` on `server` with `threads` workers, logging one line per request to stdout. Never
/// returns while the server lives.
pub fn serve(fake: Arc<Fake>, server: Arc<tiny_http::Server>, threads: usize, now_ms: fn() -> i64) {
    let workers: Vec<_> = (0..threads.max(1))
        .map(|_| {
            let (fake, server) = (Arc::clone(&fake), Arc::clone(&server));
            std::thread::spawn(move || {
                for mut req in server.incoming_requests() {
                    let method = req.method().as_str().to_owned();
                    let url = req.url().to_owned();
                    let auth = req
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Authorization"))
                        .map(|h| h.value.as_str().to_owned());
                    let mut body = Vec::new();
                    let _ = req.as_reader().take(MAX_BODY).read_to_end(&mut body);
                    let a = fake.handle(&method, &url, auth.as_deref(), &body, now_ms());
                    println!("{}", log_line(&method, &url, a.status));
                    let header =
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], a.content_type.as_bytes())
                            .expect("a static header");
                    let resp = tiny_http::Response::from_data(a.body)
                        .with_status_code(a.status)
                        .with_header(header);
                    let _ = req.respond(resp);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/fixtures/relay-job.v1/session-composed-720p.json"
        );
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn map() -> RewriteMap {
        RewriteMap {
            input_origin: "srt://mediamtx:8890".into(),
            target_origin: "rtmp://mediamtx:1935".into(),
            overlay_origin: "http://fake-control:7000".into(),
            stream_id: "read:cam".into(),
            stream_key: "dest".into(),
            passphrase: "test-passphrase-made-up-1234".into(),
        }
    }

    #[test]
    fn the_rewrite_changes_origins_only_and_keeps_every_query_parameter() {
        let f = fixture();
        let s = rewrite_session(&f, "11111111-2222-4333-8444-555555555555", &map()).unwrap();
        assert_eq!(s["input"]["url"], "srt://mediamtx:8890");
        assert_eq!(s["target"]["url"], "rtmp://mediamtx:1935/live/");
        let old = f["overlayUrl"].as_str().unwrap();
        let new = s["overlayUrl"].as_str().unwrap();
        assert!(
            new.starts_with("http://fake-control:7000/overlay/fixtures/"),
            "{new}"
        );
        assert_eq!(
            split_origin(new).unwrap().1,
            split_origin(old).unwrap().1,
            "path and query kept"
        );
        for k in ["sid=", "st=", "host=compositor", "delay=3100"] {
            assert!(query_of(new).contains(k), "{k} kept");
        }
        assert_eq!(s["sessionId"], "11111111-2222-4333-8444-555555555555");
        assert_eq!(
            (s["input"]["streamId"].as_str(), s["target"]["streamKey"].as_str()),
            (Some("read:cam"), Some("dest"))
        );
        // Everything else is the fixture's.
        for k in [
            "mode",
            "fixtureId",
            "overlayDelayMs",
            "output",
            "heartbeatIntervalMs",
            "pageToken",
        ] {
            assert_eq!(s[k], f[k], "{k}");
        }
    }

    #[test]
    fn a_bad_origin_or_url_is_refused_without_quoting_it() {
        assert_eq!(
            rewrite_origin("https://a.example/x?st=secret", "http://h:1/p"),
            Err("the new origin carries a path or a query")
        );
        assert_eq!(
            rewrite_origin("no-scheme?st=secret", "http://h:1"),
            Err("not an absolute URL")
        );
        assert_eq!(
            rewrite_origin("https:///path", "http://h:1"),
            Err("not an absolute URL")
        );
        let mut f = fixture();
        f["overlayUrl"] = Value::String("https://seazn.club/overlay".into());
        let e = rewrite_session(&f, "sid", &map()).unwrap_err();
        assert!(e.contains("query"), "{e}");
    }

    fn fake(out: Arc<Mutex<Vec<u8>>>) -> Fake {
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(b)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let s = rewrite_session(&fixture(), "sid-1", &map()).unwrap();
        let t = ScriptedTimeline {
            events: vec![(1, 3_000), (2, 6_000)],
        };
        Fake::new(
            "tok".into(),
            s,
            b"<html>".to_vec(),
            t,
            1_000,
            Box::new(Shared(out)),
        )
        .unwrap()
    }

    #[test]
    fn the_log_line_has_no_query_and_no_session_id() {
        assert_eq!(
            log_line("GET", "/overlay/fixtures/x?st=secret&sid=s", 200),
            "fake-control GET /overlay/fixtures/x 200"
        );
        assert_eq!(
            log_line("POST", &format!("{SESSIONS}3f8a2c1e-aaaa/heartbeat"), 200),
            format!("fake-control POST {SESSIONS}{{sid}}/heartbeat 200")
        );
        assert_eq!(
            log_line("GET", &format!("{SESSIONS}3f8a2c1e-aaaa"), 401),
            format!("fake-control GET {SESSIONS}{{sid}} 401")
        );
    }

    #[test]
    fn served_over_http_the_bearer_header_is_read_and_a_beat_is_logged() {
        // The real producer's shape: a bearer in the Authorization header and a JSON body.
        let out = Arc::new(Mutex::new(Vec::new()));
        let f = Arc::new(fake(Arc::clone(&out)));
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        {
            let (f, server) = (Arc::clone(&f), Arc::clone(&server));
            std::thread::spawn(move || serve(f, server, 1, || 7));
        }
        let base = format!("http://127.0.0.1:{port}");
        let get = ureq::get(&format!("{base}{SESSIONS}sid-1"))
            .set("Authorization", "Bearer tok")
            .call()
            .unwrap();
        let v: Value = serde_json::from_reader(get.into_reader()).unwrap();
        assert_eq!(v["data"]["sessionId"], "sid-1");
        match ureq::get(&format!("{base}{SESSIONS}sid-1")).call() {
            Err(ureq::Error::Status(401, _)) => {}
            other => panic!("no bearer: {other:?}"),
        }
        let hb = ureq::post(&format!("{base}{SESSIONS}sid-1/heartbeat"))
            .set("Authorization", "Bearer tok")
            .set("Content-Type", "application/json")
            .send_string(r#"{"state":"playing"}"#)
            .unwrap();
        assert_eq!(hb.status(), 200);
        let logged = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = logged.lines().collect();
        assert_eq!(lines.len(), 1, "{logged}");
        let beat: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(
            (beat["received_ms"].as_i64(), beat["body"]["state"].as_str()),
            (Some(7), Some("playing"))
        );
        server.unblock();
    }

    #[test]
    fn the_relay_routes_check_the_bearer_then_the_session() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let f = fake(Arc::clone(&out));
        let get =
            |auth: Option<&str>, sid: &str| f.handle("GET", &format!("{SESSIONS}{sid}"), auth, b"", 0).status;
        assert_eq!(get(None, "sid-1"), 401);
        assert_eq!(get(Some("Bearer nope"), "sid-1"), 401);
        assert_eq!(get(Some("Bearer tok"), "sid-2"), 404);
        assert_eq!(get(Some("Bearer nope"), "sid-2"), 401, "the bearer first");
        let a = f.handle("GET", &format!("{SESSIONS}sid-1"), Some("Bearer tok"), b"", 0);
        let v: Value = serde_json::from_slice(&a.body).unwrap();
        assert_eq!((a.status, v["ok"].as_bool()), (200, Some(true)));
        assert_eq!(v["data"]["sessionId"], "sid-1");
    }

    #[test]
    fn beats_are_logged_with_their_receipt_time_and_answered_with_the_desired_state() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let f = fake(Arc::clone(&out));
        let hb = format!("{SESSIONS}sid-1/heartbeat");
        let a = f.handle("POST", &hb, Some("Bearer tok"), br#"{"state":"playing"}"#, 42);
        let v: Value = serde_json::from_slice(&a.body).unwrap();
        assert_eq!(v["data"]["desiredState"], "live");
        assert_eq!(
            f.handle("POST", "/control/desired?state=ending", None, b"", 0)
                .status,
            200
        );
        let a = f.handle("POST", &hb, Some("Bearer tok"), br#"{"state":"playing"}"#, 43);
        let v: Value = serde_json::from_slice(&a.body).unwrap();
        assert_eq!(v["data"]["desiredState"], "ending");
        assert_eq!(
            f.handle("POST", &hb, Some("Bearer tok"), b"not json", 44).status,
            400
        );
        assert_eq!(
            f.handle("POST", &hb, None, br#"{"state":"playing"}"#, 45).status,
            401
        );
        let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2, "two accepted beats, nothing else: {text}");
        assert_eq!(
            (lines[0]["received_ms"].as_i64(), lines[1]["received_ms"].as_i64()),
            (Some(42), Some(43))
        );
        assert_eq!(lines[0]["body"]["state"], "playing");
        assert!(!text.contains("tok"), "the token is never logged");
        assert_eq!(
            f.handle("POST", "/control/desired?state=paused", None, b"", 0)
                .status,
            400
        );
    }

    #[test]
    fn the_score_feed_reveals_events_as_they_happen_and_goes_down_on_request() {
        let f = fake(Arc::new(Mutex::new(Vec::new())));
        let events = |now: i64| {
            let a = f.handle("GET", "/score.json", None, b"", now);
            assert_eq!(a.status, 200);
            let v: Value = serde_json::from_slice(&a.body).unwrap();
            v["events"].as_array().unwrap().len()
        };
        // Start 1 000; events at 4 000 and 7 000.
        assert_eq!((events(3_999), events(4_000), events(7_000)), (0, 1, 2));
        assert_eq!(
            f.handle("POST", "/control/score-down?ms=60000", None, b"", 10_000)
                .status,
            200
        );
        assert_eq!(f.handle("GET", "/score.json", None, b"", 69_999).status, 503);
        assert_eq!(events(70_000), 2, "back on the first poll after");
        assert_eq!(
            f.handle("POST", "/control/score-down?ms=0", None, b"", 0).status,
            400
        );
        let t = crate::inputs::parse_timeline(&f.timeline_json()).unwrap();
        assert_eq!(
            (t.start_wall_ms, t.delay_ms, t.events.clone()),
            (1_000, 3_100, vec![(1, 4_000), (2, 7_000)])
        );
    }

    #[test]
    fn the_page_is_served_at_both_paths() {
        let f = fake(Arc::new(Mutex::new(Vec::new())));
        for p in [
            "/overlay-test.html?delay=3100",
            "/overlay/fixtures/a1b2?sid=x&st=y&host=compositor&delay=3100",
        ] {
            let a = f.handle("GET", p, None, b"", 0);
            assert_eq!((a.status, a.body.as_slice()), (200, &b"<html>"[..]), "{p}");
        }
        assert_eq!(f.handle("GET", "/elsewhere", None, b"", 0).status, 404);
    }

    #[test]
    fn the_scripted_timeline_must_count_up_from_one_with_rising_offsets() {
        let ok = json!({"events": [{"index": 1, "offset_ms": 0}, {"index": 2, "offset_ms": 10}]});
        assert_eq!(
            ScriptedTimeline::parse(&ok).unwrap().events,
            vec![(1, 0), (2, 10)]
        );
        for bad in [
            json!({"events": []}),
            json!({"events": [{"index": 2, "offset_ms": 0}]}),
            json!({"events": [{"index": 1, "offset_ms": 5}, {"index": 2, "offset_ms": 5}]}),
            json!({}),
        ] {
            assert!(ScriptedTimeline::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_committed_score_json_is_twenty_events() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../harness/pages/score.json");
        let t = ScriptedTimeline::parse(&serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
            .unwrap();
        assert_eq!(t.events.len(), 20, "the brief's scripted 20-event timeline");
    }
}
