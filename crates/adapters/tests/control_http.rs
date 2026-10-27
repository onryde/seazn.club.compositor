//! `HttpControlPlane` against a local fake of web's two internal routes on 127.0.0.1:0. No test
//! touches the network, and every wait is bounded: a server thread gives up after `SERVER_LIFE`, every
//! read of what it saw is a `recv_timeout`, and every plane runs with a short timeout, so a broken
//! build fails a test instead of hanging it.
//!
//! The fake is a raw `TcpListener`, not `tiny_http`: the error paths need a reply cut mid-body, a
//! connection closed with no reply and a server that never answers, and `tiny_http` answers a dropped
//! request with a 500.
use adapters::control_http::{
    heartbeat_wire, parse_reply, parse_session, HttpControlPlane, PlaneConfigError,
};
use app::ports::{
    ControlPlane, FetchError, HeartbeatError, Measured, Reply as BeatReply, FINAL_BEAT_TIMEOUT_MS,
};
use domain::config::Thresholds;
use domain::health::{BeatState, ExitDetail, ExitInfo, HealthReport};
use domain::lifecycle::ExitReason;
use domain::supervisor::{Desired, Event, Refusal, Supervisor};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// A made-up job token, planted so every error path can be searched for it (AGENTS.md §7).
const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.PLANTED-JOB-TOKEN.c2lnbmF0dXJl";
const SID: &str = "3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c";
/// How long a fake server thread lives at most.
const SERVER_LIFE: Duration = Duration::from_secs(20);
/// How long a test waits to see a request the fake recorded.
const SEEN_WAIT: Duration = Duration::from_secs(5);
/// The plane's overall per-request timeout in these tests.
const PLANE_TIMEOUT_MS: u64 = 3_000;

fn fixture_text(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures/relay-job.v1")
        .join(name);
    std::fs::read_to_string(p).expect(name)
}
fn fixture(name: &str) -> Value {
    serde_json::from_str(&fixture_text(name)).unwrap()
}
/// A reply with no phone-live counter, as web sends it before the session's phone was ever live.
fn bare(desired: Desired) -> Result<BeatReply, HeartbeatError> {
    Ok(BeatReply::from(desired))
}
/// The house envelope of a 200 (contract description; F-006): the fixtures are its `data`.
fn ok(data: Value) -> String {
    json!({ "ok": true, "data": data }).to_string()
}
fn refused(code: &str) -> String {
    json!({ "ok": false, "code": code }).to_string()
}

#[derive(Debug, Clone)]
enum Reply {
    /// A complete response with this status and body.
    Status(u16, String),
    /// A response whose `Content-Length` promises more than it sends, then a close.
    Truncated(u16, String),
    /// A redirect to `location`.
    Redirect(u16, String),
    /// Read the request, then close with no reply.
    Close,
    /// Read the request, then say nothing for this long.
    Hang(Duration),
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Fake {
    url: String,
    seen: Receiver<Seen>,
}

impl Fake {
    fn next(&self) -> Seen {
        self.seen
            .recv_timeout(SEEN_WAIT)
            .expect("the fake saw no request within 5 s")
    }
    /// Nothing reached the fake within `wait`.
    fn none_within(&self, wait: Duration) -> bool {
        self.seen.recv_timeout(wait).is_err()
    }
}

fn read_request(s: &mut TcpStream) -> Option<Seen> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Seen {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Other",
    }
}

/// Serve `replies` in order, one connection each (`Connection: close`), then stop.
fn serve(replies: Vec<Reply>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let born = Instant::now();
        let mut replies = replies.into_iter();
        let Some(mut reply) = replies.next() else { return };
        while born.elapsed() < SERVER_LIFE {
            let (mut s, _) = match listener.accept() {
                Ok(c) => c,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            s.set_nonblocking(false).unwrap();
            let Some(seen) = read_request(&mut s) else {
                continue;
            };
            let _ = tx.send(seen);
            let head = |code: u16, len: usize, extra: &str| {
                format!(
                    "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\n{extra}Connection: close\r\n\r\n",
                    reason(code)
                )
            };
            match &reply {
                Reply::Status(code, body) => {
                    let _ = s.write_all(head(*code, body.len(), "").as_bytes());
                    let _ = s.write_all(body.as_bytes());
                }
                Reply::Truncated(code, body) => {
                    let _ = s.write_all(head(*code, body.len() + 500, "").as_bytes());
                    let _ = s.write_all(body.as_bytes());
                }
                Reply::Redirect(code, location) => {
                    let _ = s.write_all(head(*code, 0, &format!("Location: {location}\r\n")).as_bytes());
                }
                Reply::Close => {}
                Reply::Hang(d) => std::thread::sleep(*d),
            }
            let _ = s.flush();
            drop(s);
            match replies.next() {
                Some(r) => reply = r,
                None => return,
            }
        }
    });
    Fake { url, seen: rx }
}

/// A port nothing listens on: bind, read the port, and close the listener.
fn unbound_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    format!("http://127.0.0.1:{port}")
}

fn plane(url: &str) -> HttpControlPlane {
    plane_with(url, PLANE_TIMEOUT_MS)
}
fn plane_with(url: &str, timeout_ms: u64) -> HttpControlPlane {
    HttpControlPlane::new(url, SID, TOKEN.to_owned(), timeout_ms).expect("a valid plane")
}

fn periodic() -> HealthReport {
    let s = Supervisor::new(Thresholds::default()).decide(Event::Tick, 0).next;
    s.health(0)
}
fn final_beat(reason: ExitReason) -> HealthReport {
    HealthReport {
        state: BeatState::Stopped,
        exit: Some(ExitInfo { reason, detail: None }),
        ..periodic()
    }
}
fn schema_heartbeat() -> jsonschema::Validator {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/relay-job.v1.json");
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let s = json!({ "$schema": doc["$schema"], "$defs": doc["$defs"], "$ref": "#/$defs/heartbeat" });
    jsonschema::validator_for(&s).unwrap()
}

fn assert_no_token(what: &str, text: &str) {
    assert!(!text.contains(TOKEN), "{what}: the job token leaked: {text}");
    assert!(
        !text.contains("PLANTED-JOB-TOKEN"),
        "{what}: part of the job token leaked: {text}"
    );
}

// ---- the session GET ----

#[test]
fn fetch_session_sends_bearer_token_and_parses_200() {
    let mut checked = 0;
    for name in ["session-composed-720p.json", "session-composed-1080p.json"] {
        // Each fixture is its own session: the plane asks for that one (C07 review M3).
        let sid = fixture(name)["sessionId"].as_str().unwrap().to_owned();
        let fake = serve(vec![Reply::Status(200, ok(fixture(name)))]);
        let got = HttpControlPlane::new(&fake.url, &sid, TOKEN.to_owned(), PLANE_TIMEOUT_MS)
            .unwrap()
            .fetch_session();
        let seen = fake.next();
        assert_eq!(seen.method, "GET");
        assert_eq!(seen.path, format!("/api/internal/relay/sessions/{sid}"));
        assert_eq!(
            seen.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert_eq!(
            got,
            parse_session(&fixture(name)),
            "{name}: the envelope's data, decoded"
        );
        assert!(got.is_ok(), "{name}: {got:?}");
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// A base with a trailing slash still names the route exactly (no `//api`).
#[test]
fn a_trailing_slash_on_the_app_url_is_tolerated() {
    let fake = serve(vec![Reply::Status(
        200,
        ok(fixture("session-composed-720p.json")),
    )]);
    let got = plane(&format!("{}/", fake.url)).fetch_session();
    assert_eq!(fake.next().path, format!("/api/internal/relay/sessions/{SID}"));
    assert!(got.is_ok(), "{got:?}");
}

#[test]
fn status_401_404_410_map_to_refusals() {
    let table = [
        (401, "UNAUTHORIZED", Refusal::Unauthorized),
        (404, "NOT_FOUND", Refusal::NotFound),
        (410, "SESSION_ENDED", Refusal::Gone),
    ];
    for (code, body, want) in table {
        let fake = serve(vec![Reply::Status(code, refused(body))]);
        assert_eq!(
            plane(&fake.url).fetch_session(),
            Err(FetchError::Refused(want)),
            "{code}"
        );
    }
    eprintln!("fetch refusals checked: {}", table.len());
}

#[test]
fn status_503_and_connection_refused_are_transient() {
    let mut checked = 0;
    for code in [503, 500, 502, 429, 400, 403] {
        let fake = serve(vec![Reply::Status(code, refused("X"))]);
        let got = plane(&fake.url).fetch_session();
        assert!(matches!(got, Err(FetchError::Transient(_))), "{code}: {got:?}");
        checked += 1;
    }
    let got = plane(&unbound_url()).fetch_session();
    assert!(
        matches!(got, Err(FetchError::Transient(_))),
        "connection refused: {got:?}"
    );
    eprintln!("transient fetch outcomes checked: {}", checked + 1);
}

/// The domain retries a transient fetch on its back-off: the same plane's next call succeeds.
#[test]
fn a_fetch_after_a_503_succeeds_and_a_second_fetch_succeeds_again() {
    let body = ok(fixture("session-composed-720p.json"));
    let fake = serve(vec![
        Reply::Status(503, refused("UNAVAILABLE")),
        Reply::Status(200, body.clone()),
        Reply::Status(200, body),
    ]);
    let mut p = plane(&fake.url);
    assert!(matches!(p.fetch_session(), Err(FetchError::Transient(_))));
    let second = p.fetch_session();
    let third = p.fetch_session();
    assert_eq!(second, parse_session(&fixture("session-composed-720p.json")));
    assert_eq!(second, third, "a second fetch reads the same session");
    for _ in 0..3 {
        assert_eq!(
            fake.next().header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }
}

/// A 200 whose bytes did not arrive as JSON (empty, cut mid-document, cut mid-transfer, an HTML page
/// from something in between) is transient: it is not web's answer. A JSON 200 that is not a composed
/// session's envelope IS web's answer, and is refused.
#[test]
fn a_200_that_is_not_json_is_transient_and_one_that_is_not_a_session_is_refused() {
    let whole = ok(fixture("session-composed-720p.json"));
    let half = whole[..whole.len() / 2].to_owned();
    let transient = [
        Reply::Status(200, String::new()),
        Reply::Status(200, half.clone()),
        Reply::Truncated(200, half),
        Reply::Status(200, "<html>maintenance</html>".into()),
    ];
    let mut checked = 0;
    for r in transient {
        let fake = serve(vec![r.clone()]);
        let got = plane(&fake.url).fetch_session();
        assert!(matches!(got, Err(FetchError::Transient(_))), "{r:?}: {got:?}");
        checked += 1;
    }
    let refused_bodies = [
        json!({ "ok": true }).to_string(),
        json!({ "ok": false, "data": fixture("session-composed-720p.json") }).to_string(),
        fixture_text("session-composed-720p.json"), // bare: no envelope
        ok(fixture("session-passthrough.json")),
        ok(json!(null)),
        "[]".to_owned(),
    ];
    for body in refused_bodies {
        let fake = serve(vec![Reply::Status(200, body.clone())]);
        let got = plane(&fake.url).fetch_session();
        assert_eq!(got, Err(FetchError::Refused(Refusal::WrongMode)), "{body}");
        checked += 1;
    }
    eprintln!("200 bodies checked: {checked}");
    assert_eq!(checked, 10);
}

// ---- the heartbeat POST ----

#[test]
fn heartbeat_posts_to_heartbeat_path_and_reads_desired_state() {
    let v = schema_heartbeat();
    let m = Measured {
        fps: Some(30.0),
        cpu_pct: Some(41.5),
        ..Measured::default()
    };
    let mut checked = 0;
    // Every reply vector (fixtures README, "Names"): `desiredState` is read as the fixture spells it,
    // and its `phoneLiveEdge` (Owed 108 fix (b)) as the integer it carries, or as none when it carries
    // none, a null, or a value the contract refuses (an `invalid-` vector): the beat still counts.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/relay-job.v1");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("reply-"))
        .collect();
    names.sort();
    for reply in &names {
        let data = fixture(reply);
        let desired = match data["desiredState"].as_str() {
            Some("live") => Desired::Live,
            Some("ending") => Desired::Ending,
            other => panic!("{reply}: desiredState {other:?}"),
        };
        let edge = if reply.contains("-invalid-") {
            None
        } else {
            data["phoneLiveEdge"].as_u64()
        };
        let want = BeatReply {
            desired,
            phone_live_edge: edge,
        };
        let fake = serve(vec![Reply::Status(200, ok(data))]);
        let h = periodic();
        assert_eq!(plane(&fake.url).heartbeat(&h, &m), Ok(want), "{reply}");
        let seen = fake.next();
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.path, format!("/api/internal/relay/sessions/{SID}/heartbeat"));
        assert_eq!(
            seen.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert!(seen
            .header("content-type")
            .is_some_and(|c| c.starts_with("application/json")));
        // The seam: what web receives is exactly the wire, bare (no envelope), and the contract admits it.
        let posted: Value = serde_json::from_str(&seen.body).expect("the body is JSON");
        assert_eq!(posted, heartbeat_wire(&h, &m));
        let errs: Vec<String> = v.iter_errors(&posted).map(|e| e.to_string()).collect();
        assert!(errs.is_empty(), "web would refuse {posted}: {errs:?}");
        checked += 1;
    }
    let edges = names
        .iter()
        .filter(|n| fixture(n)["phoneLiveEdge"].is_u64())
        .count();
    eprintln!("reply fixtures checked: {checked}, with a counter: {edges}");
    assert!(checked >= 7 && edges >= 2, "anti-vacuity: {names:?}");
}

/// Owed 108 fix (b): `phoneLiveEdge` is read tolerantly. The contract admits an integer of at least 0
/// (a number with no fraction, `5.0` included: JSON Schema's integer) or null; absent, null, a
/// negative, a string, a fraction or a boolean reads as no counter, and the reply still reads its
/// `desiredState`. A reply that is not a `heartbeatReply` stays refused whatever counter it carries.
#[test]
fn the_reply_reads_its_phone_live_counter_tolerantly() {
    let cases: [(Option<Value>, Option<u64>); 9] = [
        (None, None),
        (Some(json!(null)), None),
        (Some(json!(0)), Some(0)),
        (Some(json!(5)), Some(5)),
        (Some(json!(5.0)), Some(5)),
        (Some(json!(-1)), None),
        (Some(json!("3")), None),
        (Some(json!(1.5)), None),
        (Some(json!(true)), None),
    ];
    let mut checked = 0;
    for (word, desired) in [("live", Desired::Live), ("ending", Desired::Ending)] {
        for (edge, want) in &cases {
            let mut data = json!({ "desiredState": word });
            if let Some(e) = edge {
                data["phoneLiveEdge"] = e.clone();
            }
            let got = parse_reply(&data);
            let want = BeatReply {
                desired,
                phone_live_edge: *want,
            };
            assert_eq!(got, Some(want), "{data}");
            checked += 1;
        }
    }
    for data in [
        json!({ "desiredState": "stop", "phoneLiveEdge": 3 }),
        json!({ "phoneLiveEdge": 3 }),
    ] {
        assert_eq!(parse_reply(&data), None, "{data}");
        checked += 1;
    }
    eprintln!("replies parsed: {checked}");
    assert_eq!(checked, 2 * cases.len() + 2);
}

/// Wave gate 1 review I5 (Owed 29): only a 410 is `Gone`; a 401, a 404 and a 5xx stay transport
/// errors, and none of their texts carries the token.
#[test]
fn heartbeat_410_is_gone_and_other_refusals_are_transport() {
    let table = [
        (410, true),
        (401, false),
        (404, false),
        (503, false),
        (500, false),
        (400, false),
    ];
    for (code, gone) in table {
        let fake = serve(vec![Reply::Status(code, refused("X"))]);
        let got = plane(&fake.url).heartbeat(&periodic(), &Measured::default());
        if gone {
            assert_eq!(got, Err(HeartbeatError::Gone), "{code}");
        } else {
            match &got {
                Err(HeartbeatError::Transport(text)) => assert_no_token(&format!("{code}"), text),
                other => panic!("{code}: {other:?}"),
            }
        }
        assert_no_token(&format!("{code} debug"), &format!("{got:?}"));
    }
    eprintln!("heartbeat statuses checked: {}", table.len());
}

/// The word each reason posts: the contract's `exitReason` description, word for word ("ended,
/// deadline, signalled and session_gone exit 0; refused exits 78; fatal exits 70; restart exits 75";
/// Owed 66 added the last, vendored at dec7b95d9). Exhaustive, so a new reason fails to compile here.
fn contract_exit_word(r: ExitReason) -> &'static str {
    match r {
        ExitReason::Ended => "ended",
        ExitReason::Deadline => "deadline",
        ExitReason::Signalled => "signalled",
        ExitReason::SessionGone => "session_gone",
        ExitReason::Refused => "refused",
        ExitReason::Fatal => "fatal",
        ExitReason::Restart => "restart",
    }
}

/// The final beat's result is classified like any other beat's (C07 amendment; Owed 29): a 410 is
/// `Gone` (C08 maps it to `HeartbeatRefused(Gone)`: a drain is cut short, Exited ignores it), a 404 a
/// transport error, a 200 its reply. Each reason posts its own contract word (Owed 66: the restart
/// posts `restart`, no longer `fatal`).
#[test]
fn the_final_beat_under_a_410_is_gone() {
    let mut checked = 0;
    let mut words: Vec<String> = Vec::new();
    for reason in ExitReason::ALL {
        let fake = serve(vec![Reply::Status(410, refused("SESSION_ENDED"))]);
        assert_eq!(
            plane(&fake.url).heartbeat(&final_beat(reason), &Measured::default()),
            Err(HeartbeatError::Gone),
            "{reason:?}"
        );
        let posted: Value = serde_json::from_str(&fake.next().body).unwrap();
        assert_eq!(posted["state"], "stopped");
        assert_eq!(
            posted["exitReason"],
            contract_exit_word(reason),
            "{reason:?}: {posted}"
        );
        words.push(posted["exitReason"].as_str().unwrap().to_owned());
        checked += 1;
    }
    words.sort();
    words.dedup();
    assert_eq!(
        words.len(),
        ExitReason::ALL.len(),
        "one word per reason: {words:?}"
    );
    let fake = serve(vec![Reply::Status(404, refused("NOT_FOUND"))]);
    let got = plane(&fake.url).heartbeat(&final_beat(ExitReason::Ended), &Measured::default());
    assert!(matches!(got, Err(HeartbeatError::Transport(_))), "{got:?}");
    let fake = serve(vec![Reply::Status(200, ok(fixture("reply-ending.json")))]);
    let got = plane(&fake.url).heartbeat(&final_beat(ExitReason::Ended), &Measured::default());
    assert_eq!(got, bare(Desired::Ending));
    eprintln!("final beats under a 410 checked: {checked}");
    assert_eq!(checked, 7, "the six of C06 and C11b's restart");
}

#[test]
fn a_heartbeat_reply_that_is_not_a_reply_is_a_transport_error() {
    let whole = ok(fixture("reply-live.json"));
    let replies = [
        Reply::Status(200, String::new()),
        Reply::Status(200, whole[..whole.len() - 3].to_owned()),
        Reply::Truncated(200, whole.clone()),
        Reply::Status(200, ok(json!({ "desiredState": "stop" }))),
        Reply::Status(200, ok(json!({}))),
        Reply::Status(200, fixture_text("reply-live.json")), // bare: no envelope
        Reply::Status(
            200,
            json!({ "ok": false, "data": fixture("reply-live.json") }).to_string(),
        ),
        Reply::Redirect(302, "http://127.0.0.1:9/".into()),
    ];
    let mut checked = 0;
    for r in replies {
        let fake = serve(vec![r.clone()]);
        let got = plane(&fake.url).heartbeat(&periodic(), &Measured::default());
        assert!(matches!(got, Err(HeartbeatError::Transport(_))), "{r:?}: {got:?}");
        checked += 1;
    }
    eprintln!("non-reply heartbeat bodies checked: {checked}");
    assert_eq!(checked, 8);
}

// ---- bounds ----

fn hung_server() -> Fake {
    serve(vec![Reply::Hang(Duration::from_secs(15))])
}

/// The final beat is one attempt bounded by `FINAL_BEAT_TIMEOUT_MS` (Owed 28), even when the plane's
/// own timeout is longer, so a hung web delays `Exit` at most 2 s.
#[test]
fn a_hung_final_beat_gives_up_within_its_2_s_bound() {
    let fake = hung_server();
    let mut p = plane_with(&fake.url, 8_000);
    let t0 = Instant::now();
    let got = p.heartbeat(&final_beat(ExitReason::Fatal), &Measured::default());
    let took = t0.elapsed();
    assert!(matches!(got, Err(HeartbeatError::Transport(_))), "{got:?}");
    let bound = Duration::from_millis(FINAL_BEAT_TIMEOUT_MS);
    assert!(
        took < bound + Duration::from_millis(1_500),
        "the final beat took {took:?}"
    );
    assert!(
        took + Duration::from_millis(300) >= bound,
        "gave up early, after {took:?}"
    );
    fake.next();
}

/// A plane timeout shorter than 2 s bounds the final beat too: the 2 s is a ceiling, never a floor.
#[test]
fn a_shorter_plane_timeout_bounds_the_final_beat() {
    let fake = hung_server();
    let mut p = plane_with(&fake.url, 600);
    let t0 = Instant::now();
    let got = p.heartbeat(&final_beat(ExitReason::Ended), &Measured::default());
    assert!(matches!(got, Err(HeartbeatError::Transport(_))), "{got:?}");
    assert!(
        t0.elapsed() < Duration::from_millis(1_500),
        "took {:?}",
        t0.elapsed()
    );
}

/// A periodic beat and the session GET to a server that accepts and then never answers wait out the
/// plane's own timeout, and then give up. This covers a hung REPLY only: the connect phase has its
/// own bound (`timeout_connect`, pinned by the inline `the_agent_bounds_connect_and_pools_nothing`,
/// C07 review I1), and DNS has none in ureq 2 (`docs/_INDEX.md` Owed 28).
#[test]
fn the_plane_timeout_bounds_a_hung_reply_on_every_call() {
    for call in ["fetch", "beat"] {
        let fake = hung_server();
        let mut p = plane_with(&fake.url, 2_500);
        let t0 = Instant::now();
        let text = match call {
            "fetch" => match p.fetch_session() {
                Err(FetchError::Transient(t)) => t,
                other => panic!("{other:?}"),
            },
            _ => match p.heartbeat(&periodic(), &Measured::default()) {
                Err(HeartbeatError::Transport(t)) => t,
                other => panic!("{other:?}"),
            },
        };
        let took = t0.elapsed();
        assert!(
            took >= Duration::from_millis(2_200),
            "{call}: gave up after {took:?}, before its timeout"
        );
        assert!(took < Duration::from_millis(4_500), "{call}: took {took:?}");
        assert_no_token(call, &text);
    }
}

/// Web never redirects (its route comments, R8), and a redirect must not carry the token, or the
/// call, anywhere else: none is followed, and the second server sees nothing.
#[test]
fn a_redirect_is_never_followed() {
    let mut checked = 0;
    for code in [301, 302, 303, 307, 308] {
        let elsewhere = serve(vec![Reply::Status(
            200,
            ok(fixture("session-composed-720p.json")),
        )]);
        let fake = serve(vec![Reply::Redirect(
            code,
            format!("{}/api/internal/relay/sessions/{SID}", elsewhere.url),
        )]);
        let got = plane(&fake.url).fetch_session();
        assert!(matches!(got, Err(FetchError::Transient(_))), "{code}: {got:?}");
        assert!(
            elsewhere.none_within(Duration::from_millis(300)),
            "{code}: the GET was followed"
        );
        let beat_elsewhere = serve(vec![Reply::Status(200, ok(fixture("reply-live.json")))]);
        let fake = serve(vec![Reply::Redirect(code, format!("{}/hb", beat_elsewhere.url))]);
        let got = plane(&fake.url).heartbeat(&periodic(), &Measured::default());
        assert!(
            matches!(got, Err(HeartbeatError::Transport(_))),
            "{code}: {got:?}"
        );
        assert!(
            beat_elsewhere.none_within(Duration::from_millis(300)),
            "{code}: the POST was followed"
        );
        checked += 1;
    }
    eprintln!("redirects checked: {checked}");
    assert_eq!(checked, 5);
}

// ---- secrets ----

/// AGENTS.md §7: the job token, and the session's own credentials, never reach an error's text or its
/// `Debug`, on every error path of both calls: a refused connection, a close with no reply, a cut
/// body, a hang past the timeout, every status, a redirect, and a body that is not one.
#[test]
fn transport_errors_never_contain_the_token() {
    let session = fixture("session-composed-720p.json");
    let secrets = [
        session["target"]["streamKey"].as_str().unwrap().to_owned(),
        session["input"]["passphrase"].as_str().unwrap().to_owned(),
        session["input"]["streamId"].as_str().unwrap().to_owned(),
        session["pageToken"].as_str().unwrap().to_owned(),
    ];
    // A body that leaks a credential if it is ever echoed into an error.
    let leaky = ok(session.clone());
    let leaky_half = leaky[..leaky.len() - 5].to_owned();
    let paths: Vec<Option<Reply>> = vec![
        None, // connection refused
        Some(Reply::Close),
        Some(Reply::Truncated(200, leaky_half.clone())),
        Some(Reply::Status(200, leaky_half)),
        Some(Reply::Hang(Duration::from_secs(5))),
        Some(Reply::Status(401, refused("UNAUTHORIZED"))),
        Some(Reply::Status(404, refused("NOT_FOUND"))),
        Some(Reply::Status(410, refused("SESSION_ENDED"))),
        Some(Reply::Status(503, refused(TOKEN))), // even a body that names it
        Some(Reply::Redirect(302, format!("http://127.0.0.1:9/?token={TOKEN}"))),
        Some(Reply::Status(
            200,
            ok(json!({ "mode": "composed", "streamKey": secrets[0] })),
        )),
    ];
    let mut checked = 0;
    for path in &paths {
        for call in ["fetch", "beat"] {
            let url = match path {
                None => unbound_url(),
                Some(r) => serve(vec![r.clone()]).url,
            };
            let mut p = plane_with(&url, 1_000);
            let text = match call {
                "fetch" => format!("{:?}", p.fetch_session().err()),
                _ => format!(
                    "{:?}",
                    p.heartbeat(&final_beat(ExitReason::Fatal), &Measured::default())
                        .err()
                ),
            };
            assert_no_token(&format!("{call} {path:?}"), &text);
            for s in &secrets {
                assert!(!text.contains(s.as_str()), "{call} {path:?}: {s} leaked: {text}");
            }
            assert_no_token("plane debug", &format!("{p:?}"));
            checked += 1;
        }
    }
    // C07 review I2: ureq's header check prints the whole `Authorization` line for a token holding a
    // CR, LF, DEL or non-ASCII byte. The constructor refuses such a token, and its refusal never
    // names it.
    let bad_tokens = [
        format!("{TOKEN}\r"),
        format!("{TOKEN}\n"),
        format!("{TOKEN}\u{7f}"),
        format!("{TOKEN}é"),
    ];
    for tok in &bad_tokens {
        let refused = HttpControlPlane::new(&unbound_url(), SID, tok.clone(), 1_000).map(|_| ());
        assert_eq!(refused, Err(PlaneConfigError::TokenNotVisibleAscii), "{tok:?}");
        let e = refused.unwrap_err();
        assert_no_token(&format!("refusal of {tok:?}"), &format!("{e:?} {e}"));
        checked += 1;
    }
    eprintln!("error paths checked for secrets: {checked}");
    assert_eq!(checked, paths.len() * 2 + bad_tokens.len());
}

/// The plane's `Debug` prints APP_URL as its origin alone: no userinfo, path or query of it.
#[test]
fn the_plane_debug_prints_only_its_origin() {
    let p = plane_with(
        "https://user:PLANTED-PW@seazn.club/base/PLANTED-PATH?k=PLANTED-Q",
        1_000,
    );
    let dbg = format!("{p:?} {p:#?}");
    assert!(!dbg.contains("PLANTED"), "{dbg}");
    assert!(dbg.contains("https://seazn.club/…"), "{dbg}");
}

/// The plane's own `Debug` (C01's `missing_debug_implementations`) names the session, never the token.
#[test]
fn the_plane_never_prints_its_token() {
    let p = plane_with("https://seazn.club", 1_000);
    let dbg = format!("{p:?}");
    assert_no_token("debug", &dbg);
    assert!(dbg.contains(SID), "{dbg}");
}

/// A transport error's text is `redact`ed (C07 Step 7): ureq names the URL in its errors, so a
/// credential in that URL (here a query planted on APP_URL) never reaches the error.
#[test]
fn a_transport_error_is_redacted() {
    let planted = "PLANTED-URL-SECRET-4d5e";
    let base = format!("{}/?probe={planted}", unbound_url());
    let mut p = plane_with(&base, 1_000);
    let fetch = format!("{:?}", p.fetch_session());
    let beat = format!("{:?}", p.heartbeat(&periodic(), &Measured::default()));
    for (what, text) in [("fetch", &fetch), ("beat", &beat)] {
        assert!(
            text.contains("transport"),
            "{what}: a transport error, got {text}"
        );
        assert!(!text.contains(planted), "{what}: {text}");
        assert_no_token(what, text);
    }
}

/// A reply body is read to a bound (AGENTS.md §4: nothing unbounded): a 200 far past it reads as cut,
/// which is transient, whatever it holds.
#[test]
fn a_reply_past_the_body_cap_is_transient() {
    let mut huge = fixture("session-composed-720p.json");
    huge["themeId"] = json!(" ".repeat(1024 * 1024));
    let fake = serve(vec![Reply::Status(200, ok(huge))]);
    let got = plane(&fake.url).fetch_session();
    assert!(matches!(got, Err(FetchError::Transient(_))), "{got:?}");
    let mut big_reply = fixture("reply-live.json");
    big_reply["pad"] = json!("x".repeat(1024 * 1024));
    let fake = serve(vec![Reply::Status(200, ok(big_reply))]);
    let got = plane(&fake.url).heartbeat(&periodic(), &Measured::default());
    assert!(matches!(got, Err(HeartbeatError::Transport(_))), "{got:?}");
}

// ---- the constructor's guards (C07 review I2, M2) ----

/// A bearer token is visible ASCII (RFC 6750 `b64token` is a subset): every byte 0x21..=0x7E is
/// accepted, and the bytes just outside it (space, DEL), a control byte and a non-ASCII one are
/// refused, with a refusal that never names the token.
#[test]
fn the_constructor_refuses_a_token_outside_visible_ascii() {
    let every_visible: String = (0x21u8..=0x7e).map(char::from).collect();
    assert_eq!(every_visible.len(), 94);
    assert!(
        HttpControlPlane::new("http://127.0.0.1:9", SID, every_visible, 1_000).is_ok(),
        "every visible ASCII byte is a token byte"
    );
    let mut checked = 0;
    for bad in [" ", "\u{7f}", "\t", "\0", "\r\n", "é", "😀"] {
        let tok = format!("PLANTED-JOB-TOKEN{bad}tail");
        let got = HttpControlPlane::new("http://127.0.0.1:9", SID, tok.clone(), 1_000).map(|_| ());
        assert_eq!(got, Err(PlaneConfigError::TokenNotVisibleAscii), "{tok:?}");
        let e = got.unwrap_err();
        assert_no_token(&format!("{tok:?}"), &format!("{e:?} {e}"));
        checked += 1;
    }
    eprintln!("refused token bytes checked: {checked}");
    assert_eq!(checked, 7);
}

/// A 0 ms timeout would fail every call forever (C07 review M2): refused. 1 ms is a timeout.
#[test]
fn a_zero_timeout_is_refused_at_construction() {
    let got = HttpControlPlane::new("http://127.0.0.1:9", SID, TOKEN.to_owned(), 0).map(|_| ());
    assert_eq!(got, Err(PlaneConfigError::ZeroTimeout));
    assert_no_token("zero timeout", &format!("{:?} {}", got, got.unwrap_err()));
    assert!(HttpControlPlane::new("http://127.0.0.1:9", SID, TOKEN.to_owned(), 1).is_ok());
}

/// An empty `JOB_TOKEN` (an unset secret read as `""`) is a misconfigured Machine: refused at
/// construction, the same path as a bad token (the coordinator, 2026-10-19; C12 exits 78 on it).
/// One byte is a token.
#[test]
fn an_empty_token_is_refused_at_construction() {
    let got = HttpControlPlane::new("http://127.0.0.1:9", SID, String::new(), 1_000).map(|_| ());
    assert_eq!(got, Err(PlaneConfigError::EmptyToken));
    assert_eq!(got.unwrap_err().to_string(), "the job token is empty");
    assert!(HttpControlPlane::new("http://127.0.0.1:9", SID, "x".to_owned(), 1_000).is_ok());
}

// ---- the session it fetched (C07 review M3, I3) ----

/// Web answered with a different session than the plane asked for (C07 review M3): refused, as a
/// session that is not ours. The id compares as a UUID, so its case does not matter.
#[test]
fn a_session_for_another_session_id_is_refused() {
    let mut other = fixture("session-composed-720p.json");
    other["sessionId"] = json!("00000000-0000-4000-8000-000000000000");
    let fake = serve(vec![Reply::Status(200, ok(other))]);
    assert_eq!(
        plane(&fake.url).fetch_session(),
        Err(FetchError::Refused(Refusal::WrongMode))
    );
    let mut upper = fixture("session-composed-720p.json");
    upper["sessionId"] = json!(SID.to_ascii_uppercase());
    let fake = serve(vec![Reply::Status(200, ok(upper))]);
    let got = plane(&fake.url).fetch_session();
    assert!(got.is_ok(), "the same UUID in upper case: {got:?}");
}

/// C07 review I3 (a): once a session is fetched, the plane scrubs its four credentials by value, raw
/// and percent-encoded, from the final beat's `exitDetail` and from every error text, so a key that
/// follows no marker is masked too. Each expected detail is the input with the value replaced by
/// `****`; each encoded form is written from RFC 3986, not from the code.
#[test]
fn the_sessions_credentials_are_scrubbed_by_value_from_the_final_beat_and_errors() {
    const KEY: &str = "PLANTED-SK/1+2=";
    const PASS: &str = "PLANTED-PASS-9";
    const STREAM_ID: &str = "PLANTED-SID-8";
    const PAGE: &str = "PLANTED-PT-7";
    // The overlay URL's `st=`, planted apart from `pageToken` so each is proven learned on its own.
    const ST: &str = "PLANTED-ST-6";
    let mut session = fixture("session-composed-720p.json");
    session["target"]["streamKey"] = json!(KEY);
    session["input"]["passphrase"] = json!(PASS);
    session["input"]["streamId"] = json!(STREAM_ID);
    session["pageToken"] = json!(PAGE);
    let overlay = session["overlayUrl"]
        .as_str()
        .unwrap()
        .replace("fixture-page-token-720p", ST);
    session["overlayUrl"] = json!(overlay);
    let forms = [
        KEY,
        "PLANTED-SK%2F1%2B2%3D",
        "PLANTED-SK%2f1%2b2%3d",
        PASS,
        STREAM_ID,
        PAGE,
        ST,
    ];
    let mut replies = vec![Reply::Status(200, ok(session))];
    replies.extend(
        forms
            .iter()
            .map(|_| Reply::Status(200, ok(fixture("reply-live.json")))),
    );
    replies.push(Reply::Close);
    // APP_URL carries the key in its path, so ureq's own error text names it (it prints the URL).
    let fake = serve(replies);
    let mut p = plane(&format!("{}/{KEY}", fake.url));
    assert!(p.fetch_session().is_ok());
    fake.next();
    let mut checked = 0;
    for form in forms {
        let beat = HealthReport {
            state: BeatState::Stopped,
            exit: Some(ExitInfo {
                reason: ExitReason::Fatal,
                detail: Some(ExitDetail::new(format!("stream '{form}' publish failed"))),
            }),
            ..periodic()
        };
        assert_eq!(
            p.heartbeat(&beat, &Measured::default()),
            bare(Desired::Live),
            "{form}"
        );
        let posted: Value = serde_json::from_str(&fake.next().body).unwrap();
        assert_eq!(posted["exitDetail"], "stream '****' publish failed", "{form}");
        checked += 1;
    }
    let text = match p.heartbeat(&periodic(), &Measured::default()) {
        Err(HeartbeatError::Transport(t)) => t,
        other => panic!("a closed connection: {other:?}"),
    };
    assert!(!text.contains("PLANTED"), "{text}");
    assert!(text.contains("****"), "the URL was in the error, masked: {text}");
    eprintln!("credential forms scrubbed from a final beat: {checked}");
    assert_eq!(checked, 7);
}

/// C08 review round 1 (the final-beat lane): the control worker posts final beats through
/// `final_lane()`, made when the worker starts, before any session is fetched. The fetch on the first
/// instance still teaches the lane every credential, so the lane's final beat is scrubbed of each one;
/// and the lane calls the same session's route with the same token.
#[test]
fn the_final_lane_scrubs_what_the_plane_learns_after_it_was_made() {
    const KEY: &str = "PLANTED-SK-LANE-5";
    const PASS: &str = "PLANTED-PASS-LANE-4";
    const STREAM_ID: &str = "PLANTED-SID-LANE-3";
    const PAGE: &str = "PLANTED-PT-LANE-2";
    const ST: &str = "PLANTED-ST-LANE-1";
    let mut session = fixture("session-composed-720p.json");
    session["target"]["streamKey"] = json!(KEY);
    session["input"]["passphrase"] = json!(PASS);
    session["input"]["streamId"] = json!(STREAM_ID);
    session["pageToken"] = json!(PAGE);
    let overlay = session["overlayUrl"]
        .as_str()
        .unwrap()
        .replace("fixture-page-token-720p", ST);
    session["overlayUrl"] = json!(overlay);
    let values = [KEY, PASS, STREAM_ID, PAGE, ST];
    let mut replies = vec![Reply::Status(200, ok(session))];
    replies.extend(
        values
            .iter()
            .map(|_| Reply::Status(200, ok(fixture("reply-live.json")))),
    );
    let fake = serve(replies);
    let mut p = plane(&fake.url);
    let mut lane = p.final_lane();
    assert!(p.fetch_session().is_ok());
    fake.next();
    let mut checked = 0;
    for value in values {
        let beat = HealthReport {
            state: BeatState::Stopped,
            exit: Some(ExitInfo {
                reason: ExitReason::Fatal,
                detail: Some(ExitDetail::new(format!("stream '{value}' publish failed"))),
            }),
            ..periodic()
        };
        assert_eq!(
            lane.heartbeat(&beat, &Measured::default()),
            bare(Desired::Live),
            "{value}"
        );
        let seen = fake.next();
        assert_eq!(seen.path, format!("/api/internal/relay/sessions/{SID}/heartbeat"));
        assert_eq!(
            seen.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        let posted: Value = serde_json::from_str(&seen.body).unwrap();
        assert_eq!(posted["exitDetail"], "stream '****' publish failed", "{value}");
        checked += 1;
    }
    eprintln!("credentials scrubbed from the final lane's beat: {checked}");
    assert_eq!(checked, 5);
}

// ---- pooling (C07 review M4) ----

/// A fake that keeps each connection open (no `Connection: close`) and answers every request on it
/// with `body`; it reports the index of the connection each request came on.
fn serve_keep_alive(body: String) -> (String, Receiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let born = Instant::now();
        let mut conn = 0;
        while born.elapsed() < SERVER_LIFE {
            let Ok((mut s, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            s.set_nonblocking(false).unwrap();
            let (tx, body, idx) = (tx.clone(), body.clone(), conn);
            conn += 1;
            std::thread::spawn(move || {
                while read_request(&mut s).is_some() {
                    let _ = tx.send(idx);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(body.as_bytes());
                    let _ = s.flush();
                }
            });
        }
    });
    (url, rx)
}

/// No connection is pooled: ureq never retries a POST on a stale pooled connection, so a reused one
/// the server had closed would lose that beat, the final one included (C07 review M4). Each call
/// opens its own, even when the server would keep the last one open.
#[test]
fn every_call_opens_a_fresh_connection() {
    let (url, conns) = serve_keep_alive(ok(fixture("reply-live.json")));
    let mut p = plane(&url);
    let mut seen = Vec::new();
    for _ in 0..3 {
        assert_eq!(
            p.heartbeat(&periodic(), &Measured::default()),
            bare(Desired::Live)
        );
        seen.push(conns.recv_timeout(SEEN_WAIT).expect("the fake saw the beat"));
    }
    assert_eq!(seen, vec![0, 1, 2], "one connection per call");
}
