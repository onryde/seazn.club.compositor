//! The loopback programme-phase endpoint (C12 amendment 2026-10-15; overlay templates spec §4.3).
//! Every test binds `127.0.0.1:0`, so they run in parallel. Expected values come from the vendored
//! contract (`$defs.phaseEndpoint`, `$defs.phaseReply`) or the spec, never from the module.
use std::net::SocketAddr;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use adapters::control_http::heartbeat_wire;
use adapters::phase_http::{
    origin_of, PhaseBindError, PhaseHttp, PHASE_HTTP_MAX_CONCURRENT, RELAY_PHASE_LONG_POLL_MAX_MS,
    RELAY_PHASE_PATH, RELAY_PHASE_PORT,
};
use app::ports::{Measured, PhasePublisher};
use domain::config::Thresholds;
use domain::health::HealthReport;
use domain::programme::ProgrammePhase;
use domain::supervisor::{Event, Supervisor};
use serde_json::{json, Value};

/// The app's origin as F-009 observed it in CEF's `Origin` header: scheme + host, no slash.
const ORIGIN: &str = "https://stg.seazn.club";

fn contract() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/relay-job.v1.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn schema_def(def: &str) -> jsonschema::Validator {
    let doc = contract();
    let s = json!({ "$schema": doc["$schema"], "$defs": doc["$defs"], "$ref": format!("#/$defs/{def}") });
    jsonschema::validator_for(&s).expect("schema compiles")
}

fn endpoint() -> PhaseHttp {
    PhaseHttp::bind("127.0.0.1:0".parse().unwrap(), ORIGIN.to_owned()).expect("bind")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(RELAY_PHASE_LONG_POLL_MAX_MS + 5_000))
        .build()
}

/// One answer: its status, its headers (names lowercased) and its body, whatever the status.
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn cors_headers(&self) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.starts_with("access-control-"))
            .map(|(n, _)| n.as_str())
            .collect()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {:?}", self.body))
    }
}

fn send(
    method: &str,
    addr: SocketAddr,
    path_and_query: &str,
    origin: Option<&str>,
    extra: &[(&str, &str)],
) -> Answer {
    let mut req = agent().request(method, &format!("http://{addr}{path_and_query}"));
    if let Some(o) = origin {
        req = req.set("Origin", o);
    }
    for (k, v) in extra {
        req = req.set(k, v);
    }
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("{method} {path_and_query}: {e}"),
    };
    let status = resp.status();
    let headers = resp
        .headers_names()
        .into_iter()
        .map(|n| {
            let v = resp.header(&n).unwrap_or_default().to_owned();
            (n.to_ascii_lowercase(), v)
        })
        .collect();
    let body = resp.into_string().unwrap_or_default();
    Answer {
        status,
        headers,
        body,
    }
}

fn get_after(addr: SocketAddr, after: &str) -> Answer {
    send(
        "GET",
        addr,
        &format!("{RELAY_PHASE_PATH}?after={after}"),
        Some(ORIGIN),
        &[],
    )
}

/// Waits (bounded) until the endpoint holds `n` admitted requests.
fn wait_in_flight(p: &PhaseHttp, n: usize) {
    let until = Instant::now() + Duration::from_secs(5);
    while p.in_flight() != n {
        assert!(
            Instant::now() < until,
            "in flight {} never reached {n}",
            p.in_flight()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Every phase, by an exhaustive match: a new variant fails to compile here.
fn all_phases() -> Vec<ProgrammePhase> {
    let all = [
        ProgrammePhase::Waiting,
        ProgrammePhase::Live,
        ProgrammePhase::Frozen,
        ProgrammePhase::Reconnecting,
        ProgrammePhase::Final,
    ];
    for p in all {
        match p {
            ProgrammePhase::Waiting
            | ProgrammePhase::Live
            | ProgrammePhase::Frozen
            | ProgrammePhase::Reconnecting
            | ProgrammePhase::Final => {}
        }
    }
    all.to_vec()
}

#[test]
fn serves_the_published_phase_with_cors_for_the_app_origin() {
    let mut p = endpoint();
    p.publish(ProgrammePhase::Live, 1);
    let a = get_after(p.local_addr(), "0");
    assert_eq!(a.status, 200, "{a:?}");
    let body = a.json();
    let errs: Vec<String> = schema_def("phaseReply")
        .iter_errors(&body)
        .map(|e| e.to_string())
        .collect();
    assert!(errs.is_empty(), "{body} violates phaseReply: {errs:?}");
    assert_eq!(body, json!({ "phase": "live", "seq": 1 }));
    assert_eq!(
        a.header("Access-Control-Allow-Origin"),
        Some(ORIGIN),
        "the origin itself, never *"
    );
    assert_eq!(a.header("Vary"), Some("Origin"));
    assert_eq!(a.header("Cache-Control"), Some("no-store"));
}

#[test]
fn preflight_allows_private_network_for_the_app_origin_only() {
    let p = endpoint();
    let pna = [
        ("Access-Control-Request-Private-Network", "true"),
        ("Access-Control-Request-Method", "GET"),
    ];
    let mut checked = 0;
    let ok = send("OPTIONS", p.local_addr(), RELAY_PHASE_PATH, Some(ORIGIN), &pna);
    assert_eq!(ok.status, 204, "{ok:?}");
    assert_eq!(ok.header("Access-Control-Allow-Private-Network"), Some("true"));
    assert_eq!(ok.header("Access-Control-Allow-Origin"), Some(ORIGIN));
    assert_eq!(ok.header("Access-Control-Allow-Methods"), Some("GET"));
    assert_eq!(ok.header("Vary"), Some("Origin"));
    assert_eq!(ok.header("Cache-Control"), Some("no-store"));
    assert_eq!(
        ok.header("Access-Control-Allow-Headers"),
        None,
        "a CORS-simple GET only (F-009)"
    );
    checked += 1;
    for origin in [Some("https://evil.example"), None] {
        let a = send("OPTIONS", p.local_addr(), RELAY_PHASE_PATH, origin, &pna);
        assert_eq!(a.status, 403, "{origin:?}: {a:?}");
        assert!(a.cors_headers().is_empty(), "{origin:?}: {:?}", a.cors_headers());
        let g = send(
            "GET",
            p.local_addr(),
            &format!("{RELAY_PHASE_PATH}?after=0"),
            origin,
            &[],
        );
        assert_eq!(g.status, 403, "{origin:?}: {g:?}");
        assert!(g.cors_headers().is_empty(), "{origin:?}: {:?}", g.cors_headers());
        checked += 1;
    }
    eprintln!("origins checked: {checked}");
    assert_eq!(checked, 3);
}

#[test]
fn long_poll_waits_for_a_newer_seq_and_answers_at_once_when_behind() {
    let mut p = endpoint();
    let addr = p.local_addr();
    p.publish(ProgrammePhase::Live, 1);
    let started = Instant::now();
    let parked = thread::spawn(move || get_after(addr, "1"));
    wait_in_flight(&p, 1);
    thread::sleep(Duration::from_millis(300));
    assert!(!parked.is_finished(), "after=1 at seq 1 must wait");
    p.publish(ProgrammePhase::Frozen, 2);
    let a = parked.join().unwrap();
    let took = started.elapsed();
    assert_eq!(a.json(), json!({ "phase": "frozen", "seq": 2 }));
    assert!(
        took < Duration::from_millis(800),
        "answered {took:?} after the poll"
    );

    let t = Instant::now();
    let behind = get_after(addr, "0");
    assert!(
        t.elapsed() < Duration::from_millis(200),
        "behind: {:?}",
        t.elapsed()
    );
    assert_eq!(behind.json()["seq"], 2);

    let t = Instant::now();
    let idle = get_after(addr, "2");
    let held = t.elapsed();
    assert!(
        held >= Duration::from_millis(RELAY_PHASE_LONG_POLL_MAX_MS - 100)
            && held <= Duration::from_millis(RELAY_PHASE_LONG_POLL_MAX_MS + 1_000),
        "held {held:?}"
    );
    assert_eq!(idle.json(), json!({ "phase": "frozen", "seq": 2 }), "unchanged");
}

#[test]
fn binds_loopback_only() {
    let refused = PhaseHttp::bind("0.0.0.0:0".parse().unwrap(), ORIGIN.to_owned());
    assert_eq!(refused.err(), Some(PhaseBindError::NotLoopback));
    let p = endpoint();
    assert_eq!(p.local_addr().ip().to_string(), "127.0.0.1");
}

#[test]
fn fifth_concurrent_poll_is_refused_503() {
    let mut p = endpoint();
    let addr = p.local_addr();
    p.publish(ProgrammePhase::Live, 1);
    // Four polls parked at distinct `after`s, so one publish releases exactly one of them.
    let parked: Vec<_> = (1..=PHASE_HTTP_MAX_CONCURRENT as u64)
        .map(|after| thread::spawn(move || get_after(addr, &after.to_string())))
        .collect();
    wait_in_flight(&p, PHASE_HTTP_MAX_CONCURRENT);
    let fifth = get_after(addr, "0");
    assert_eq!(fifth.status, 503, "{fifth:?}");
    p.publish(ProgrammePhase::Frozen, 2);
    wait_in_flight(&p, PHASE_HTTP_MAX_CONCURRENT - 1);
    let again = get_after(addr, "0");
    assert_eq!(again.status, 200, "a poll is accepted again: {again:?}");
    assert_eq!(again.json()["seq"], 2);
    p.publish(ProgrammePhase::Final, 5);
    let answered: Vec<u16> = parked.into_iter().map(|h| h.join().unwrap().status).collect();
    assert_eq!(answered, vec![200; PHASE_HTTP_MAX_CONCURRENT]);
}

#[test]
fn before_any_publish_serves_waiting_seq_0() {
    let p = endpoint();
    // No `after` is below seq 0, so `after=0` waits and the long-poll bound answers it, with the
    // value served before any publish.
    let t = Instant::now();
    let a = get_after(p.local_addr(), "0");
    assert!(t.elapsed() >= Duration::from_millis(RELAY_PHASE_LONG_POLL_MAX_MS - 100));
    assert_eq!(a.status, 200);
    assert_eq!(a.json(), json!({ "phase": "waiting", "seq": 0 }));
}

#[test]
fn bad_requests_are_refused() {
    let p = endpoint();
    let addr = p.local_addr();
    let mut checked = 0;
    for (method, path, want) in [
        ("GET", "/elsewhere?after=0", 404),
        ("POST", "/phase?after=0", 405),
        ("GET", "/phase?after=abc", 400),
        ("GET", "/phase", 400),
        ("GET", "/phase?after=-1", 400),
    ] {
        let a = send(method, addr, path, Some(ORIGIN), &[]);
        assert_eq!(a.status, want, "{method} {path}: {a:?}");
        checked += 1;
    }
    eprintln!("bad requests checked: {checked}");
    assert_eq!(checked, 5);
}

#[test]
fn endpoint_constants_match_the_vendored_contract() {
    let props = &contract()["$defs"]["phaseEndpoint"]["properties"];
    assert_eq!(props["port"]["const"], json!(RELAY_PHASE_PORT));
    assert_eq!(props["path"]["const"], json!(RELAY_PHASE_PATH));
    assert_eq!(
        props["longPollMaxMs"]["const"],
        json!(RELAY_PHASE_LONG_POLL_MAX_MS)
    );
}

fn report_in(phase: ProgrammePhase) -> HealthReport {
    let s = Supervisor::new(Thresholds::default()).decide(Event::Tick, 0).next;
    HealthReport {
        programme_phase: phase,
        ..s.health(0)
    }
}

#[test]
fn phase_word_matches_the_heartbeat_wire_for_every_phase() {
    let declared: Vec<Value> = contract()["$defs"]["phaseReply"]["properties"]["phase"]["enum"]
        .as_array()
        .expect("phaseReply.phase enum")
        .clone();
    let mut p = endpoint();
    let mut checked = 0;
    for (i, phase) in all_phases().into_iter().enumerate() {
        let seq = i as u64 + 1;
        p.publish(phase, seq);
        let served = get_after(p.local_addr(), &(seq - 1).to_string()).json();
        let beat = heartbeat_wire(&report_in(phase), &Measured::default());
        assert_eq!(served["phase"], beat["programmePhase"], "{phase:?}");
        assert!(
            declared.contains(&served["phase"]),
            "{phase:?}: {} not in {declared:?}",
            served["phase"]
        );
        assert_eq!(served["seq"], seq);
        checked += 1;
    }
    eprintln!("phases checked: {checked}");
    assert_eq!(checked, 5, "anti-vacuity: every phase");
}

/// F-009's form, which the wiring derives the allowed origin with (APP_URL's and each overlay URL's).
#[test]
fn origin_of_is_scheme_host_and_a_non_default_port() {
    let cases = [
        ("https://seazn.club", Some("https://seazn.club")),
        ("https://stg.seazn.club/", Some("https://stg.seazn.club")),
        ("http://localhost:3000/x", Some("http://localhost:3000")),
        (
            "https://seazn.club:443/overlay/f?st=t",
            Some("https://seazn.club"),
        ),
        ("http://h:80", Some("http://h")),
        ("HTTPS://Stg.Seazn.Club/o?st=t#x", Some("https://stg.seazn.club")),
        ("https://h:8443?st=t", Some("https://h:8443")),
        ("srt://h:778", None),
        ("seazn.club", None),
        ("https://user:pw@h/x", None),
        ("https://h:/x", None),
        ("https://h:4x3/x", None),
        ("https:///x", None),
    ];
    for (url, want) in cases {
        assert_eq!(origin_of(url).as_deref(), want, "{url}");
    }
    eprintln!("origins derived: {}", cases.len());
}
