//! The built binary in fake mode (C12 Step 3 and the amendments' smoke runs). Each test spawns
//! `CARGO_BIN_EXE_compositor` with a cleared environment and holds the file's `SERIAL` lock, so two
//! binaries never race for the phase endpoint's port. `COMPOSITOR_FAKE_CONTROL=http` runs the real
//! HTTP control plane against this file's fake control server, so a test reads every POST the binary
//! sent. Every bound is built from `Thresholds::default()` or a contract constant (AGENTS.md class 20).
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use adapters::control_http::parse_session;
use adapters::phase_http::{origin_of, RELAY_PHASE_LONG_POLL_MAX_MS, RELAY_PHASE_PATH, RELAY_PHASE_PORT};
use compositor::env;
use compositor::logged::rfc3339_ms;
use domain::config::Thresholds;
use serde_json::{json, Value};
use testkit::fakes::session_720p;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

const TOKEN: &str = "SECRET-TOKEN-VALUE-c12";
/// The vendored session fixture's id (`session-composed-720p.json`): the plane refuses a session
/// answered for another id.
const SID: &str = "3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c";
/// A stream key that follows no marker (no `/live/`, no `?`, no `streamid=`), at least 8 characters:
/// only the by-value scrub can hide it (Owed 32).
const BARE_KEY: &str = "BAREKEY7d2c9e41f0";

fn fixture(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures/relay-job.v1")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(p).expect(name)).unwrap()
}

fn epoch_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

/// `RELAY_DEADLINE_AT` `ms` from now (R1's ISO form).
fn deadline_in(ms: i64) -> String {
    rfc3339_ms((epoch_ms() + ms) as u64)
}

fn t() -> Thresholds {
    Thresholds::default()
}

// ---- the spawned binary ----

struct Run {
    child: std::process::Child,
    lines: Arc<Mutex<Vec<(Instant, String)>>>,
    spawned: Instant,
    exit: Option<(Option<i32>, Instant)>,
}

impl Run {
    fn spawn(vars: &[(&str, String)]) -> Run {
        Self::spawn_cmd(Command::new(env!("CARGO_BIN_EXE_compositor")), vars)
    }

    fn spawn_cmd(mut cmd: Command, vars: &[(&str, String)]) -> Run {
        cmd.env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        for (k, v) in vars {
            cmd.env(k, v);
        }
        let spawned = Instant::now();
        let mut child = cmd.spawn().expect("spawn the compositor");
        let lines: Arc<Mutex<Vec<(Instant, String)>>> = Arc::default();
        let (out, l) = (child.stderr.take().unwrap(), Arc::clone(&lines));
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                l.lock().unwrap().push((Instant::now(), line));
            }
        });
        Run {
            child,
            lines,
            spawned,
            exit: None,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The instant of the first stderr line holding `needle`, waiting at most `limit`.
    fn wait_line(&self, needle: &str, limit: Duration) -> Option<Instant> {
        let until = Instant::now() + limit;
        loop {
            if let Some((at, _)) = self
                .lines
                .lock()
                .unwrap()
                .iter()
                .find(|(_, l)| l.contains(needle))
            {
                return Some(*at);
            }
            if Instant::now() >= until {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// The exit code (None: a signal) and when it was seen, waiting at most `limit`; kills on timeout.
    fn wait_exit(&mut self, limit: Duration) -> (Option<i32>, Instant) {
        let until = Instant::now() + limit;
        loop {
            if let Some(s) = self.child.try_wait().expect("try_wait") {
                let e = (s.code(), Instant::now());
                self.exit = Some(e);
                // Let the reader drain the last lines.
                thread::sleep(Duration::from_millis(50));
                return e;
            }
            if Instant::now() >= until {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("no exit within {limit:?}; stderr:\n{}", self.stderr());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn stderr(&self) -> String {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn signal(&self, sig: &str) {
        let ok = Command::new("kill")
            .args([format!("-{sig}"), self.pid().to_string()])
            .status()
            .expect("kill");
        assert!(ok.success(), "kill -{sig}");
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if self.exit.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn scripted_env(deadline: String) -> Vec<(&'static str, String)> {
    vec![
        (env::SESSION_ID, SID.to_owned()),
        (env::JOB_TOKEN, TOKEN.to_owned()),
        // The fixture's overlay URL is served from this origin: the page's own (F-009 form).
        (env::APP_URL, "https://seazn.club".to_owned()),
        (env::RELAY_DEADLINE_AT, deadline),
        (env::COMPOSITOR_DRIVERS, "fake".to_owned()),
    ]
}

// ---- the fake control server ----

#[derive(Debug, Clone)]
struct Post {
    at: Instant,
    body: Value,
    status: u16,
    answered: Instant,
}

/// Each POST's answer: (delay before it, status, body).
type Script = Arc<dyn Fn(usize, &Value) -> (Duration, u16, String) + Send + Sync>;

struct FakeControl {
    url: String,
    posts: Arc<Mutex<Vec<Post>>>,
    fetches: Arc<Mutex<usize>>,
    server: Arc<tiny_http::Server>,
}

impl Drop for FakeControl {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn ok(data: Value) -> String {
    json!({ "ok": true, "data": data }).to_string()
}

fn refused(code: &str) -> String {
    json!({ "ok": false, "code": code }).to_string()
}

fn live() -> (Duration, u16, String) {
    (Duration::ZERO, 200, ok(fixture("reply-live.json")))
}

impl FakeControl {
    fn serve(session: Value, script: Script) -> FakeControl {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let posts: Arc<Mutex<Vec<Post>>> = Arc::default();
        let fetches: Arc<Mutex<usize>> = Arc::default();
        let (srv, p, f) = (Arc::clone(&server), Arc::clone(&posts), Arc::clone(&fetches));
        thread::spawn(move || {
            for mut req in srv.incoming_requests() {
                let (p, f, session, script) = (
                    Arc::clone(&p),
                    Arc::clone(&f),
                    session.clone(),
                    Arc::clone(&script),
                );
                thread::spawn(move || {
                    let at = Instant::now();
                    let authed = req.headers().iter().any(|h| {
                        h.field.equiv("Authorization") && h.value.as_str() == format!("Bearer {TOKEN}")
                    });
                    let path = req.url().to_owned();
                    let base = format!("/api/internal/relay/sessions/{SID}");
                    let respond = |req: tiny_http::Request, status: u16, body: String| {
                        let r = tiny_http::Response::from_string(body)
                            .with_status_code(status)
                            .with_header(
                                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                                    .unwrap(),
                            );
                        let _ = req.respond(r);
                    };
                    if !authed {
                        return respond(req, 401, refused("UNAUTHORIZED"));
                    }
                    if *req.method() == tiny_http::Method::Get && path == base {
                        *f.lock().unwrap() += 1;
                        return respond(req, 200, ok(session));
                    }
                    if *req.method() == tiny_http::Method::Post && path == format!("{base}/heartbeat") {
                        let mut text = String::new();
                        let _ = req.as_reader().read_to_string(&mut text);
                        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        let n = p.lock().unwrap().len();
                        let (delay, status, reply) = script(n, &body);
                        let i = {
                            let mut posts = p.lock().unwrap();
                            posts.push(Post {
                                at,
                                body,
                                status,
                                answered: at,
                            });
                            posts.len() - 1
                        };
                        thread::sleep(delay);
                        respond(req, status, reply);
                        p.lock().unwrap()[i].answered = Instant::now();
                        return;
                    }
                    respond(req, 404, refused("NOT_FOUND"))
                });
            }
        });
        FakeControl {
            url,
            posts,
            fetches,
            server,
        }
    }

    fn posts(&self) -> Vec<Post> {
        let mut v = self.posts.lock().unwrap().clone();
        v.sort_by_key(|p| p.at);
        v
    }

    fn last_post(&self) -> Post {
        self.posts().pop().expect("at least one POST")
    }

    fn env(&self, deadline: String) -> Vec<(&'static str, String)> {
        let mut v = scripted_env(deadline);
        v.retain(|(k, _)| *k != env::APP_URL);
        v.push((env::APP_URL, self.url.clone()));
        v.push((env::COMPOSITOR_FAKE_CONTROL, "http".to_owned()));
        v
    }
}

fn assert_no_secret(run: &Run) {
    let e = run.stderr();
    assert!(!e.contains(TOKEN), "the token reached stderr:\n{e}");
}

// ---- Step 3 ----

/// A 3 s deadline: the Final hold, then the drain, which the fake reports at once. An exit through
/// the drain TIMEOUT lands at the upper bound or later, so this proves `OutputDrained` ended the run.
#[test]
fn fake_run_exits_zero_through_the_drain_after_the_final_hold() {
    let _s = serial();
    let t = t();
    let mut run = Run::spawn(&scripted_env(deadline_in(3_000)));
    let lower = Duration::from_millis(2_500 + t.final_hold_ms);
    let upper = Duration::from_millis(3_000 + t.final_hold_ms + t.drain_timeout_ms);
    let (code, at) = run.wait_exit(upper + Duration::from_secs(5));
    let took = at - run.spawned;
    eprintln!("fake run exited {code:?} after {took:?} (bounds {lower:?}..{upper:?})");
    assert_eq!(code, Some(0), "{}", run.stderr());
    assert!(
        took >= lower && took < upper,
        "{took:?} outside {lower:?}..{upper:?}"
    );
    let e = run.stderr();
    assert!(e.contains("msg=programme started"), "{e}");
    assert!(e.contains("msg=exit code=0"), "{e}");
    assert!(e.contains("msg=compositor started drivers=fake"), "{e}");
    assert_no_secret(&run);
}

/// Review focus 4, end to end: a deadline already past exits at once, starting nothing.
#[test]
fn past_deadline_exits_zero_at_once_and_starts_nothing() {
    let _s = serial();
    let mut run = Run::spawn(&scripted_env(deadline_in(-3_600_000)));
    let (code, at) = run.wait_exit(Duration::from_secs(10));
    let took = at - run.spawned;
    eprintln!("past-deadline run exited {code:?} after {took:?}");
    assert_eq!(code, Some(0), "{}", run.stderr());
    assert!(took < Duration::from_secs(1), "{took:?}");
    let e = run.stderr();
    assert!(!e.contains("programme started"), "{e}");
    assert!(e.contains("msg=exit code=0"), "{e}");
}

#[test]
fn fake_session_matches_the_vendored_fixture() {
    let parsed = parse_session(&fixture("session-composed-720p.json")).expect("the fixture decodes");
    assert_eq!(session_720p(), parsed);
}

// ---- the phase endpoint (amendment 2026-10-15) ----

/// One GET of the endpoint, or None while nothing listens yet.
fn poll_phase(after: u64, origin: &str) -> Option<(u16, Value)> {
    let r = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(RELAY_PHASE_LONG_POLL_MAX_MS + 5_000))
        .build()
        .get(&format!(
            "http://127.0.0.1:{RELAY_PHASE_PORT}{RELAY_PHASE_PATH}?after={after}"
        ))
        .set("Origin", origin)
        .call();
    match r {
        Ok(r) => {
            let status = r.status();
            Some((status, serde_json::from_str(&r.into_string().ok()?).ok()?))
        }
        Err(ureq::Error::Status(c, _)) => Some((c, Value::Null)),
        Err(_) => None,
    }
}

/// The seam, from the real driver through the real publisher to the real endpoint: `final` arrives
/// at the deadline, every seq rising, then the process exits 0. The poll's origin is the page's own,
/// the fixture overlay URL's, which is APP_URL's here (the cross-check the M6 review owes).
#[test]
fn fake_run_serves_final_on_the_loopback_endpoint_at_the_deadline() {
    let _s = serial();
    let t = t();
    let origin = origin_of(&session_720p().overlay_url).expect("an origin");
    assert_eq!(
        origin,
        origin_of("https://seazn.club").unwrap(),
        "the page's origin is the run's APP_URL's"
    );
    let mut run = Run::spawn(&scripted_env(deadline_in(3_000)));
    let mut seen: Vec<(Duration, String, u64)> = Vec::new();
    let mut after = 0;
    let give_up = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            Instant::now() < give_up,
            "no final; seen {seen:?}\n{}",
            run.stderr()
        );
        match poll_phase(after, &origin) {
            None => thread::sleep(Duration::from_millis(20)),
            Some((status, body)) => {
                assert_eq!(status, 200, "{body}");
                let seq = body["seq"].as_u64().expect("seq");
                let phase = body["phase"].as_str().expect("phase").to_owned();
                if seq > after || seen.is_empty() {
                    seen.push((run.spawned.elapsed(), phase.clone(), seq));
                }
                after = seq;
                if phase == "final" {
                    break;
                }
            }
        }
    }
    eprintln!("phases seen: {seen:?}");
    let final_at = seen.last().unwrap().0;
    assert!(final_at >= Duration::from_millis(2_500), "final at {final_at:?}");
    for w in seen.windows(2) {
        assert!(w[1].2 > w[0].2, "seq did not rise: {seen:?}");
    }
    // Anti-vacuity: the answer came from a publish (seq past the endpoint's initial 0), not the bound.
    assert!(seen.last().unwrap().2 >= 1, "{seen:?}");
    let (code, _) = run.wait_exit(Duration::from_millis(
        t.final_hold_ms + t.drain_timeout_ms + 5_000,
    ));
    assert_eq!(code, Some(0), "{}", run.stderr());
}

#[test]
fn phase_port_taken_still_runs_to_exit_zero() {
    let _s = serial();
    let t = t();
    let _held = TcpListener::bind(("127.0.0.1", RELAY_PHASE_PORT)).expect("hold the phase port");
    let mut run = Run::spawn(&scripted_env(deadline_in(3_000)));
    let (code, _) = run.wait_exit(Duration::from_millis(
        3_000 + t.final_hold_ms + t.drain_timeout_ms + 5_000,
    ));
    assert_eq!(
        code,
        Some(0),
        "a graphics hint never ends a run: {}",
        run.stderr()
    );
    let e = run.stderr();
    let warned = e
        .lines()
        .filter(|l| l.contains("phase endpoint unavailable"))
        .count();
    assert_eq!(warned, 1, "{e}");
    assert!(e.contains("level=warn msg=phase endpoint unavailable"), "{e}");
    assert!(e.contains("msg=programme started"), "it ran: {e}");
}

// ---- the final beat (owner ruling 2026-10-15; amendments 2026-10-18 and 2026-10-19) ----

/// Web's `ending`: the Final scene holds (an earlier POST carries `final`), then the run ends.
#[test]
fn reply_ending_holds_final_then_posts_ended() {
    let _s = serial();
    let t = t();
    let fc = FakeControl::serve(
        fixture("session-composed-720p.json"),
        Arc::new(|_, _| (Duration::ZERO, 200, ok(fixture("reply-ending.json")))),
    );
    let mut run = Run::spawn(&fc.env(deadline_in(3_600_000)));
    let (code, _) = run.wait_exit(Duration::from_millis(
        t.final_hold_ms + t.drain_timeout_ms + 10_000,
    ));
    assert_eq!(code, Some(0), "{}", run.stderr());
    let posts = fc.posts();
    let last = posts.last().expect("posts");
    assert_eq!(last.body["state"], "stopped", "{}", last.body);
    assert_eq!(last.body["exitReason"], "ended", "{}", last.body);
    let finals = posts[..posts.len() - 1]
        .iter()
        .filter(|p| p.body["programmePhase"] == "final")
        .count();
    eprintln!("posts: {}, earlier ones carrying final: {finals}", posts.len());
    assert!(
        finals > 0,
        "the Final scene never reached a beat: {:?}",
        posts.iter().map(|p| &p.body).collect::<Vec<_>>()
    );
    assert_eq!(*fc.fetches.lock().unwrap(), 1);
    assert_no_secret(&run);
}

/// `ProgrammeStartFailed` with a text naming a marker-less stream key: exit 70, the last POST `fatal`
/// with the text, the key masked there and in stderr. Run twice: at once, and with every POST
/// answered after 1.5 s (the periodic slot full when the start fails).
#[test]
fn start_failure_exits_70_with_the_detail_scrubbed_of_a_bare_key() {
    let _s = serial();
    let forced = format!("rtmp2sink: stream {BARE_KEY} refused at publish");
    assert!(
        forced.contains(BARE_KEY),
        "test setup: the forced text names the key bare"
    );
    let mut session = fixture("session-composed-720p.json");
    session["target"]["streamKey"] = json!(BARE_KEY);
    let mut checked = 0;
    for delay in [Duration::ZERO, Duration::from_millis(1_500)] {
        let fc = FakeControl::serve(
            session.clone(),
            Arc::new(move |_, _| (delay, 200, ok(fixture("reply-live.json")))),
        );
        let mut vars = fc.env(deadline_in(3_600_000));
        vars.push((env::COMPOSITOR_FAKE_START_FAIL, forced.clone()));
        let mut run = Run::spawn(&vars);
        let (code, _) = run.wait_exit(Duration::from_secs(15));
        assert_eq!(code, Some(70), "{delay:?}: {}", run.stderr());
        let last = fc.last_post();
        assert_eq!(last.body["exitReason"], "fatal", "{delay:?}: {}", last.body);
        let detail = last.body["exitDetail"].as_str().expect("a detail");
        assert_eq!(detail, "rtmp2sink: stream **** refused at publish", "{delay:?}");
        let e = run.stderr();
        assert!(!e.contains(BARE_KEY), "{delay:?}: the key reached stderr:\n{e}");
        assert!(
            e.contains("msg=programme start failed: rtmp2sink: stream **** refused at publish"),
            "{e}"
        );
        assert!(
            !fc.posts().iter().any(|p| p.body.to_string().contains(BARE_KEY)),
            "a POST carried the key"
        );
        assert_no_secret(&run);
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// A host stop (SIGTERM, or SIGUSR2 as the image's entrypoint forwards it: F-016) drains at once:
/// exit 0 within the drain bound, the last POST `signalled`, and no POST carries `final` (a host stop
/// never shows "Stream ended"). The final beat is answered 503 here, and the binary logs that failure,
/// scrubbed.
#[test]
fn sigterm_two_seconds_after_start_drains_and_posts_signalled() {
    let _s = serial();
    let t = t();
    let mut checked = 0;
    for sig in ["TERM", "USR2"] {
        let fc = FakeControl::serve(
            fixture("session-composed-720p.json"),
            Arc::new(|_, body: &Value| {
                if body["state"] == "stopped" {
                    (Duration::ZERO, 503, refused("UNAVAILABLE"))
                } else {
                    live()
                }
            }),
        );
        let mut run = Run::spawn(&fc.env(deadline_in(3_600_000)));
        let started = run
            .wait_line("msg=programme started", Duration::from_secs(10))
            .unwrap_or_else(|| panic!("never started:\n{}", run.stderr()));
        thread::sleep((started + Duration::from_secs(2)).saturating_duration_since(Instant::now()));
        let sent = Instant::now();
        run.signal(sig);
        let (code, at) = run.wait_exit(Duration::from_millis(t.drain_timeout_ms + 5_000));
        let took = at - sent;
        eprintln!("SIG{sig} to exit: {took:?}");
        assert_eq!(code, Some(0), "{sig}: {}", run.stderr());
        assert!(
            took < Duration::from_millis(t.drain_timeout_ms + 1_000),
            "{sig}: {took:?}"
        );
        let posts = fc.posts();
        assert_eq!(fc.last_post().body["exitReason"], "signalled", "{sig}");
        assert!(
            posts.iter().all(|p| p.body["programmePhase"] != "final"),
            "{sig}: a host stop showed final"
        );
        let e = run.stderr();
        assert!(
            e.contains("level=warn msg=final beat failed: heartbeat refused: status 503"),
            "{sig}: {e}"
        );
        assert_no_secret(&run);
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// R1's stop (SIGINT, or SIGUSR1 as the image's entrypoint forwards it: F-016) holds the Final scene
/// for `final_hold_ms`, then drains: exit 0 no sooner than the hold and within hold + drain + 1 s, an
/// earlier POST carrying `programmePhase: final`, the last POST `signalled`.
#[test]
fn sigint_holds_the_final_scene_then_drains() {
    let _s = serial();
    let t = t();
    let mut checked = 0;
    for sig in ["INT", "USR1"] {
        let fc = FakeControl::serve(fixture("session-composed-720p.json"), Arc::new(|_, _| live()));
        let mut run = Run::spawn(&fc.env(deadline_in(3_600_000)));
        let started = run
            .wait_line("msg=programme started", Duration::from_secs(10))
            .unwrap_or_else(|| panic!("never started:\n{}", run.stderr()));
        thread::sleep((started + Duration::from_secs(2)).saturating_duration_since(Instant::now()));
        let sent = Instant::now();
        run.signal(sig);
        let (code, at) = run.wait_exit(Duration::from_millis(
            t.final_hold_ms + t.drain_timeout_ms + 5_000,
        ));
        let took = at - sent;
        eprintln!("SIG{sig} to exit: {took:?}");
        assert_eq!(code, Some(0), "{sig}: {}", run.stderr());
        assert!(
            took >= Duration::from_millis(t.final_hold_ms),
            "{sig}: no hold: {took:?}"
        );
        assert!(
            took < Duration::from_millis(t.final_hold_ms + t.drain_timeout_ms + 1_000),
            "{sig}: {took:?}"
        );
        let posts = fc.posts();
        let (last, earlier) = posts.split_last().expect("posts");
        assert_eq!(last.body["exitReason"], "signalled", "{sig}: {}", last.body);
        assert!(
            earlier.iter().any(|p| p.body["programmePhase"] == "final"),
            "{sig}: the Final scene never reached a beat"
        );
        assert_no_secret(&run);
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// Web's 410 ends the run at once: exit 0 within 1 s of the reply, and no POST after it.
#[test]
fn a_410_on_a_beat_exits_zero_within_a_second_and_posts_nothing_after() {
    let _s = serial();
    let fc = FakeControl::serve(
        fixture("session-composed-720p.json"),
        Arc::new(|_, _| (Duration::ZERO, 410, refused("SESSION_ENDED"))),
    );
    let mut run = Run::spawn(&fc.env(deadline_in(3_600_000)));
    let (code, at) = run.wait_exit(Duration::from_secs(20));
    assert_eq!(code, Some(0), "{}", run.stderr());
    let posts = fc.posts();
    let first = posts
        .iter()
        .find(|p| p.status == 410)
        .expect("a 410 was answered");
    let after = at.saturating_duration_since(first.answered);
    eprintln!("410 to exit: {after:?}; posts: {}", posts.len());
    assert!(after < Duration::from_secs(1), "{after:?}");
    assert!(
        posts.iter().all(|p| p.at <= first.answered),
        "a POST after the 410"
    );
}

/// C11b: a dead CEF zygote restarts the process in place: exit 75, the last POST the vendored
/// restart fixture's words (Owed 66).
#[test]
fn a_dead_zygote_exits_75_posting_restart() {
    let _s = serial();
    let want = fixture("heartbeat-final-restart.json");
    let fc = FakeControl::serve(fixture("session-composed-720p.json"), Arc::new(|_, _| live()));
    let mut vars = fc.env(deadline_in(3_600_000));
    vars.push((env::COMPOSITOR_FAKE_ZYGOTE_GONE, "1".to_owned()));
    let mut run = Run::spawn(&vars);
    let (code, _) = run.wait_exit(Duration::from_secs(15));
    assert_eq!(code, Some(75), "{}", run.stderr());
    let last = fc.last_post();
    for key in ["state", "exitReason", "exitDetail"] {
        assert_eq!(last.body[key], want[key], "{key}: {}", last.body);
    }
    assert!(last.body["exitDetail"]
        .as_str()
        .unwrap()
        .starts_with("restart in place: "));
    let e = run.stderr();
    let started = e.find("msg=programme started").expect("started first");
    let ended = e.find("msg=exit code=75").expect("the exit line");
    assert!(started < ended, "{e}");
    assert!(e.contains("msg=cef health zygote_gone=1 "), "{e}");
}

/// C07 amendment 2026-10-19: a job token with a CR (a CRLF `.env` line) is a misconfigured Machine:
/// exit 78 before the loop, no beat, and no byte of the token in stderr, in both modes that build the
/// HTTP plane. An empty token is refused the same way (unset, as the environment reads it).
#[test]
fn a_job_token_with_a_cr_exits_78_and_never_prints_it() {
    let _s = serial();
    let fc = FakeControl::serve(fixture("session-composed-720p.json"), Arc::new(|_, _| live()));
    let mut checked = 0;
    for (mode, token) in [
        ("live", format!("{TOKEN}\r")),
        ("fake", format!("{TOKEN}\r")),
        ("fake", String::new()),
    ] {
        let mut vars = fc.env(deadline_in(3_600_000));
        vars.retain(|(k, _)| *k != env::JOB_TOKEN && *k != env::COMPOSITOR_DRIVERS);
        vars.push((env::JOB_TOKEN, token.clone()));
        vars.push((env::COMPOSITOR_DRIVERS, mode.to_owned()));
        let mut run = Run::spawn(&vars);
        let (code, at) = run.wait_exit(Duration::from_secs(10));
        assert_eq!(code, Some(78), "{mode} {token:?}: {}", run.stderr());
        assert!(
            at - run.spawned < Duration::from_secs(2),
            "refused before the loop"
        );
        assert_no_secret(&run);
        let e = run.stderr();
        assert!(
            !e.contains("programme started") && !e.contains("compositor started"),
            "{e}"
        );
        checked += 1;
    }
    assert!(
        fc.posts().is_empty() && *fc.fetches.lock().unwrap() == 0,
        "no request reached web"
    );
    assert_eq!(checked, 3);
}

/// No core of the compositor (Owed 68 and the PR #36 review's I1), set by the binary itself: the run
/// starts from a raised soft limit as a non-root user (both preconditions checked, so the test cannot
/// pass where cores are already off or every `/proc` entry is root's), and once it runs
/// `/proc/<pid>/limits` reads 1 byte soft and hard, and `/proc/<pid>/status` belongs to root, which is
/// how the kernel shows a non-dumpable process (its files, not its directory). CEF's children inherit the limit, not the
/// flag (the image test reads theirs and forces an abort).
#[cfg(target_os = "linux")]
#[test]
fn the_compositor_runs_non_dumpable_with_a_one_byte_core_limit() {
    let _s = serial();
    let raise = "ulimit -S -c unlimited 2>/dev/null || ulimit -S -c \"$(ulimit -H -c)\"";
    let pre = Command::new("sh")
        .args(["-c", &format!("{raise}; cat /proc/self/limits")])
        .output()
        .expect("sh");
    let core_line = |text: &str| -> Vec<String> {
        let l = text
            .lines()
            .find(|l| l.starts_with("Max core file size"))
            .unwrap_or_else(|| panic!("no core line in {text}"))
            .to_owned();
        l["Max core file size".len()..]
            .split_whitespace()
            .take(2)
            .map(str::to_owned)
            .collect()
    };
    let before = core_line(&String::from_utf8_lossy(&pre.stdout));
    eprintln!("raised core limit (soft, hard): {before:?}");
    assert_ne!(
        before[0], "0",
        "precondition: this environment's hard core limit is 0, so nothing can be witnessed"
    );
    let mut cmd = Command::new("sh");
    cmd.args([
        "-c",
        &format!("{raise}; exec \"$0\""),
        env!("CARGO_BIN_EXE_compositor"),
    ]);
    let mut run = Run::spawn_cmd(cmd, &scripted_env(deadline_in(3_600_000)));
    run.wait_line("msg=programme started", Duration::from_secs(10))
        .unwrap_or_else(|| panic!("never started:\n{}", run.stderr()));
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", run.pid())).expect("limits");
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", run.pid())).unwrap_or_default();
    assert_eq!(
        comm.trim(),
        "compositor",
        "the pid is the compositor (sh exec'd it)"
    );
    // The kernel's value, not the binary's constant: at exactly 1 byte it refuses a core to a piped
    // core_pattern (fs/coredump.c), and 1 is below the ELF minimum for a file one.
    let want = "1".to_owned();
    assert_eq!(core_line(&limits), vec![want.clone(), want], "{limits}");
    use std::os::unix::fs::MetadataExt;
    // The kernel hands a non-dumpable process's /proc FILES to root (`task_dump_owner`); the
    // /proc/<pid> directory keeps the euid either way (the first image probe read it, and read 10001).
    // This test's own status file is the control: dumpable, so it stays this uid's.
    let me = std::fs::metadata("/proc/self/status")
        .expect("/proc/self/status")
        .uid();
    assert_ne!(me, 0, "precondition: run as root, every /proc entry is root's");
    let owner = std::fs::metadata(format!("/proc/{}/status", run.pid()))
        .expect("/proc/<pid>/status")
        .uid();
    assert_eq!(
        owner, 0,
        "the compositor is dumpable: /proc/<pid>/status belongs to uid {owner}"
    );
    run.signal("TERM");
    let (code, _) = run.wait_exit(Duration::from_secs(15));
    assert_eq!(code, Some(0), "{}", run.stderr());
}

// ---- Owed 82: the held-teardown test hook ----

/// An APP_URL nothing listens on: a live run's session fetch fails and it retries, so it runs on
/// (until the test kills it) with no web and no session.
fn closed_app_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    drop(l);
    format!("http://127.0.0.1:{port}")
}

/// A live run's environment, `COMPOSITOR_TEST_HOLD_TEARDOWNS` set to `hold` if given.
fn live_env(hold: Option<&str>) -> Vec<(&'static str, String)> {
    let mut v = scripted_env(deadline_in(3_600_000));
    v.retain(|(k, _)| *k != env::APP_URL && *k != env::COMPOSITOR_DRIVERS);
    v.push((env::APP_URL, closed_app_url()));
    v.push((env::COMPOSITOR_DRIVERS, "live".to_owned()));
    if let Some(h) = hold {
        v.push(("COMPOSITOR_TEST_HOLD_TEARDOWNS", h.to_owned()));
    }
    v
}

/// The live run's lines from the test hook, once it has started.
fn hook_lines(hold: Option<&str>) -> Vec<String> {
    let run = Run::spawn(&live_env(hold));
    run.wait_line("msg=compositor started drivers=live", Duration::from_secs(20))
        .unwrap_or_else(|| panic!("{hold:?}: never started:\n{}", run.stderr()));
    assert_no_secret(&run);
    run.stderr()
        .lines()
        .filter(|l| l.contains("test hook") || l.contains("COMPOSITOR_TEST_HOLD_TEARDOWNS"))
        .map(str::to_owned)
        .collect()
}

/// Owed 82 (owner option A, 2026-10-24), the `test-hooks` binary: a live run arms exactly the held
/// teardowns `COMPOSITOR_TEST_HOLD_TEARDOWNS` asks for, in one line, C14 case 16's witness that the
/// run carries the hook; unset or 0 arms nothing; a value it does not take exits 78 before the loop.
#[cfg(feature = "test-hooks")]
#[test]
fn test_hook_a_live_run_arms_the_held_teardowns_it_is_given() {
    let _s = serial();
    let mut checked = 0;
    for (hold, armed) in [
        (None, None),
        (Some("0"), None),
        (Some("2"), Some(2)),
        (Some("3"), Some(3)),
    ] {
        let lines = hook_lines(hold);
        match armed {
            None => assert!(lines.is_empty(), "{hold:?}: {lines:?}"),
            Some(n) => {
                let want = format!("msg=WARN test hook (Owed 82): the next {n} CEF teardowns never return");
                assert_eq!(lines.len(), 1, "{hold:?}: {lines:?}");
                assert!(lines[0].ends_with(&want), "{hold:?}: {lines:?}");
            }
        }
        checked += 1;
    }
    let mut run = Run::spawn(&live_env(Some("two")));
    let (code, _) = run.wait_exit(Duration::from_secs(10));
    assert_eq!(code, Some(78), "{}", run.stderr());
    let e = run.stderr();
    assert!(
        e.contains("msg=COMPOSITOR_TEST_HOLD_TEARDOWNS is not a valid value"),
        "{e}"
    );
    assert!(
        !e.contains("compositor started") && !e.contains("test hook"),
        "{e}"
    );
    checked += 1;
    eprintln!("hook runs checked: {checked}");
    assert_eq!(checked, 5, "anti-vacuity");
}

/// Owed 82: the binary built without the feature (the release image's) arms nothing and says nothing
/// of the key, whatever it is set to: the hook is not in it.
#[cfg(not(feature = "test-hooks"))]
#[test]
fn test_hook_a_build_without_the_feature_arms_nothing() {
    let _s = serial();
    let mut checked = 0;
    for hold in ["3", "two"] {
        let lines = hook_lines(Some(hold));
        assert!(lines.is_empty(), "{hold:?}: {lines:?}");
        checked += 1;
    }
    eprintln!("hook runs checked: {checked}");
    assert_eq!(checked, 2, "anti-vacuity");
}

// ---- Owed 94: the sink-stall test hook ----

/// Owed 94 (the owner, 2026-10-24, "B"), the `test-hooks` binary: a live run arms the sink stall
/// either key asks for, in one line (C14's output-hung witness); both keys at once exit 78 before
/// the loop.
#[cfg(feature = "test-hooks")]
#[test]
fn test_hook_a_live_run_arms_the_sink_stall_it_is_given() {
    let _s = serial();
    let mut checked = 0;
    for (key, word) in [
        ("COMPOSITOR_TEST_HOLD_SINK_AFTER_MS", "hold"),
        ("COMPOSITOR_TEST_HANG_SINK_AFTER_MS", "hang"),
    ] {
        let mut v = live_env(None);
        v.push((key, "12000".to_owned()));
        let run = Run::spawn(&v);
        run.wait_line("msg=compositor started drivers=live", Duration::from_secs(20))
            .unwrap_or_else(|| panic!("{key}: never started:\n{}", run.stderr()));
        assert_no_secret(&run);
        let lines: Vec<String> = run
            .stderr()
            .lines()
            .filter(|l| l.contains("test hook"))
            .map(str::to_owned)
            .collect();
        let want = format!("WARN test hook (Owed 94): sink gen=1 stalls ({word}) 12000 ms after its build");
        assert_eq!(lines.len(), 1, "{key}: {lines:?}");
        assert!(lines[0].ends_with(&want), "{key}: {lines:?}");
        checked += 1;
    }
    let mut v = live_env(None);
    v.push(("COMPOSITOR_TEST_HOLD_SINK_AFTER_MS", "12000".to_owned()));
    v.push(("COMPOSITOR_TEST_HANG_SINK_AFTER_MS", "12000".to_owned()));
    let mut run = Run::spawn(&v);
    let (code, _) = run.wait_exit(Duration::from_secs(10));
    assert_eq!(code, Some(78), "{}", run.stderr());
    let e = run.stderr();
    assert!(
        e.contains("msg=COMPOSITOR_TEST_HANG_SINK_AFTER_MS is not a valid value"),
        "{e}"
    );
    assert!(
        !e.contains("compositor started") && !e.contains("test hook"),
        "{e}"
    );
    checked += 1;
    eprintln!("sink-stall hook runs checked: {checked}");
    assert_eq!(checked, 3, "anti-vacuity");
}

/// Owed 94: the binary built without the feature (the release image's) plants no sink stall and
/// says nothing of the keys, whatever they are set to.
#[cfg(not(feature = "test-hooks"))]
#[test]
fn test_hook_a_build_without_the_feature_plants_no_sink_stall() {
    let _s = serial();
    let mut v = live_env(None);
    v.push(("COMPOSITOR_TEST_HOLD_SINK_AFTER_MS", "12000".to_owned()));
    v.push(("COMPOSITOR_TEST_HANG_SINK_AFTER_MS", "ten".to_owned()));
    let run = Run::spawn(&v);
    run.wait_line("msg=compositor started drivers=live", Duration::from_secs(20))
        .unwrap_or_else(|| panic!("never started:\n{}", run.stderr()));
    let e = run.stderr();
    assert!(!e.contains("test hook") && !e.contains("SINK_AFTER_MS"), "{e}");
}
