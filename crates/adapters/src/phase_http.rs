//! The loopback programme-phase endpoint (C12; overlay templates spec 2026-10-15 §4.3, D10): "the
//! compositor serves `GET http://127.0.0.1:<RELAY_PHASE_PORT>/phase` on loopback inside the Machine.
//! It returns `{ phase, seq }` and supports long-poll (`?after=<seq>`, held ≤ 10 s)". The overlay page
//! reads it; it is never exposed outside the Machine and is not part of the outbound-only host
//! contract (ADR 0007). C08 owns the `PhasePublisher` port and the publish-on-change rule; this is
//! its HTTP adapter.
//!
//! | Request | Answer |
//! |---|---|
//! | a 5th concurrent request | 503 |
//! | any request whose `Origin` is not the allowed one, or has none | 403, no `Access-Control-*` header |
//! | another path | 404 |
//! | `OPTIONS /phase` | 204, CORS for the origin, `Access-Control-Allow-Private-Network: true` |
//! | another method | 405 |
//! | `GET /phase` with `after` missing or not a `u64` | 400 |
//! | `GET /phase?after=S`, `seq` > S | 200 at once, `{"phase":<word>,"seq":<seq>}` |
//! | same, `seq` ≤ S | 200 once a publish raises `seq`, or after `RELAY_PHASE_LONG_POLL_MAX_MS` |
//!
//! Every 200 and 204 carries `Cache-Control: no-store`, `Access-Control-Allow-Origin: <origin>` (the
//! origin itself, never `*`) and `Vary: Origin`. No `Access-Control-Allow-Headers`, by design (F-009
//! proved a CORS-simple GET only; R2005's hook stays one).
//!
//! The allowed origin is the overlay page's own (F-009: the `Origin` header CEF sends is
//! `https://<host>`, scheme and host, the default port omitted, no trailing slash). C12's wiring sets
//! it from the overlay URL each page is loaded from (`AllowedOrigin::set`), so a page served from a
//! host other than APP_URL's (stg serves `/overlay` on two hosts; PR #20 review M6) is still answered.
use std::fmt;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use app::ports::PhasePublisher;
use domain::programme::ProgrammePhase;

use crate::control_http::programme_phase_wire;

/// The loopback programme-phase endpoint (overlay templates spec 2026-10-15 §4.3). The page reads it;
/// the host never exposes it. Values are drift-tested against the vendored contract's `$defs.phaseEndpoint`.
pub const RELAY_PHASE_PORT: u16 = 7419;
pub const RELAY_PHASE_PATH: &str = "/phase";
pub const RELAY_PHASE_LONG_POLL_MAX_MS: u64 = 10_000;
/// The page holds one poll; a CEF reload can hold two for a moment. 4 bounds a stuck client; the 5th gets 503.
pub const PHASE_HTTP_MAX_CONCURRENT: usize = 4;

/// Why `PhaseHttp::bind` refused. Neither variant carries an address or a value worth hiding, but
/// the wiring logs only its own line (`phase endpoint unavailable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseBindError {
    /// The address is not a loopback one: refused before anything is bound ("127.0.0.1 only").
    NotLoopback,
    /// The bind itself failed (the port is taken, most likely).
    Bind(std::io::ErrorKind),
}

impl fmt::Display for PhaseBindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLoopback => f.write_str("not a loopback address"),
            Self::Bind(kind) => write!(f, "bind failed: {kind}"),
        }
    }
}

impl std::error::Error for PhaseBindError {}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The one origin the endpoint answers, shared with the wiring, which moves it to each page's own.
#[derive(Clone, Default)]
pub struct AllowedOrigin(Arc<Mutex<String>>);

impl AllowedOrigin {
    pub fn set(&self, origin: String) {
        *locked(&self.0) = origin;
    }

    pub fn get(&self) -> String {
        locked(&self.0).clone()
    }
}

impl fmt::Debug for AllowedOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AllowedOrigin").field(&*locked(&self.0)).finish()
    }
}

struct Shared {
    /// The latest `(phase, seq)`: `(Waiting, 0)` until the first publish, as C08's driver starts.
    latest: Mutex<(ProgrammePhase, u64)>,
    /// Long-polls wait on it; a publish wakes them all.
    cv: Condvar,
    origin: AllowedOrigin,
    in_flight: AtomicUsize,
}

/// The endpoint: an accept thread over a loopback listener, one thread per admitted request.
pub struct PhaseHttp {
    shared: Arc<Shared>,
    server: Arc<tiny_http::Server>,
    addr: SocketAddr,
}

impl fmt::Debug for PhaseHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PhaseHttp")
            .field("addr", &self.addr)
            .field("origin", &self.shared.origin)
            .finish_non_exhaustive()
    }
}

impl PhaseHttp {
    /// Refuses a non-loopback `addr` (`PhaseBindError::NotLoopback`) before binding. That's the guard
    /// for "127.0.0.1 only".
    pub fn bind(addr: SocketAddr, allowed_origin: String) -> Result<Self, PhaseBindError> {
        if /*MUT*/ false && !addr.ip().is_loopback() {
            return Err(PhaseBindError::NotLoopback);
        }
        let listener = TcpListener::bind(addr).map_err(|e| PhaseBindError::Bind(e.kind()))?;
        let addr = listener
            .local_addr()
            .map_err(|e| PhaseBindError::Bind(e.kind()))?;
        let server = tiny_http::Server::from_listener(listener, None)
            .map_err(|_| PhaseBindError::Bind(std::io::ErrorKind::Other))?;
        let origin = AllowedOrigin::default();
        origin.set(allowed_origin);
        let shared = Arc::new(Shared {
            latest: Mutex::new((ProgrammePhase::Waiting, 0)),
            cv: Condvar::new(),
            origin,
            in_flight: AtomicUsize::new(0),
        });
        let server = Arc::new(server);
        let (s, srv) = (Arc::clone(&shared), Arc::clone(&server));
        thread::Builder::new()
            .name("phase-http".to_owned())
            .spawn(move || accept(&s, &srv))
            .map_err(|e| PhaseBindError::Bind(e.kind()))?;
        Ok(Self { shared, server, addr })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Requests admitted and not yet answered (a parked long-poll counts until its answer).
    pub fn in_flight(&self) -> usize {
        self.shared.in_flight.load(Ordering::SeqCst)
    }

    /// The allowed origin's handle: the wiring moves it to each page's own origin.
    pub fn allowed_origin(&self) -> AllowedOrigin {
        self.shared.origin.clone()
    }
}

impl Drop for PhaseHttp {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

impl PhasePublisher for PhaseHttp {
    fn publish(&mut self, phase: ProgrammePhase, seq: u64) {
        *locked(&self.shared.latest) = (phase, seq);
        self.shared.cv.notify_all();
    }
}

/// The accept loop: a request past `PHASE_HTTP_MAX_CONCURRENT` is answered 503 here, at once; every
/// other one gets its own thread, which may hold it for a long-poll.
fn accept(shared: &Arc<Shared>, server: &tiny_http::Server) {
    for req in server.incoming_requests() {
        if shared.in_flight.fetch_add(1, Ordering::SeqCst) >= PHASE_HTTP_MAX_CONCURRENT {
            shared.in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = req.respond(tiny_http::Response::empty(503));
            continue;
        }
        let s = Arc::clone(shared);
        let spawned = thread::Builder::new()
            .name("phase-poll".to_owned())
            .spawn(move || {
                serve(&s, req);
                s.in_flight.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            // The request went down with the closure; only the count is ours to put back.
            shared.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("an ASCII header")
}

/// The headers every 200 and 204 carries for the allowed `origin`.
fn cors(origin: &str) -> Vec<tiny_http::Header> {
    vec![
        header("Access-Control-Allow-Origin", origin),
        header("Vary", "Origin"),
        header("Cache-Control", "no-store"),
    ]
}

fn respond_status(req: tiny_http::Request, status: u16, headers: Vec<tiny_http::Header>) {
    let mut r = tiny_http::Response::empty(status);
    for h in headers {
        r.add_header(h);
    }
    let _ = req.respond(r);
}

fn serve(shared: &Shared, req: tiny_http::Request) {
    let allowed = shared.origin.get();
    let origin_ok = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Origin"))
        .is_some_and(|h| h.value.as_str() == allowed);
    if !origin_ok {
        return respond_status(req, 403, Vec::new());
    }
    let url = req.url().to_owned();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    if path != RELAY_PHASE_PATH {
        return respond_status(req, 404, Vec::new());
    }
    match req.method() {
        tiny_http::Method::Options => {
            let mut h = cors(&allowed);
            h.push(header("Access-Control-Allow-Methods", "GET"));
            /*MUT*/
            respond_status(req, 204, h)
        }
        tiny_http::Method::Get => {
            let after = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("after="))
                .and_then(|v| v.parse::<u64>().ok());
            let Some(after) = after else {
                return respond_status(req, 400, Vec::new());
            };
            let (phase, seq) = wait_past(shared, after);
            let body = serde_json::json!({ "phase": programme_phase_wire(phase), "seq": seq }).to_string();
            let mut r = tiny_http::Response::from_string(body);
            r.add_header(header("Content-Type", "application/json"));
            for h in cors(&allowed) {
                r.add_header(h);
            }
            let _ = req.respond(r);
        }
        _ => respond_status(req, 405, vec![header("Allow", "GET, OPTIONS")]),
    }
}

/// The current value once its `seq` is past `after`, or after the long-poll bound, whichever first.
fn wait_past(shared: &Shared, after: u64) -> (ProgrammePhase, u64) {
    let until = Instant::now() + Duration::from_millis(RELAY_PHASE_LONG_POLL_MAX_MS);
    let mut latest = locked(&shared.latest);
    loop {
        if /*MUT*/ true || latest.1 > after {
            return *latest;
        }
        let now = Instant::now();
        if now >= until {
            return *latest;
        }
        latest = shared
            .cv
            .wait_timeout(latest, until - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// `url`'s origin in the form CEF sends as `Origin` (F-009): the scheme and host, lowercased, the
/// scheme's default port omitted (`http` 80, `https` 443) and any other kept, no path, no trailing
/// slash. `None` for a URL that is not `http(s)://host[:port]…` (a userinfo or an IPv6 literal
/// included: neither is an APP_URL or an overlay URL web builds).
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return None,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, port) = match rest[..end].split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (&rest[..end], None),
    };
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'));
    if !host_ok {
        return None;
    }
    let host = host.to_ascii_lowercase();
    match port {
        None => Some(format!("{scheme}://{host}")),
        Some(p) if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) => None,
        Some(p) if p == default_port => Some(format!("{scheme}://{host}")),
        Some(p) => Some(format!("{scheme}://{host}:{p}")),
    }
}
