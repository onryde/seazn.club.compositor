//! fake-control's start-up self-check, through the binary (PR #48 confirm pass, m3). A session the
//! compositor would refuse must stop the binary with exit 78 before it listens. The unit test in
//! `control.rs` proves `self_check` refuses; this one proves `main` calls it: without the call, the
//! refused session is served, and the binary prints that it is listening instead of exiting.
//!
//! The positive pair runs the vendored fixture unchanged: the binary must say it listens, with the
//! size C07's parser read (1280x720), so a binary that refuses everything cannot pass either case.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
// Test values, not credentials: the binary takes them from the environment, never argv.
const TOKEN: &str = "hjt_fake_control_self_check_test";
const SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";

fn fixture() -> Value {
    let p = format!("{ROOT}/contracts/fixtures/relay-job.v1/session-composed-720p.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn spawn(dir: &Path, session: &Value) -> Child {
    std::fs::create_dir_all(dir).unwrap();
    let fixture = dir.join("session.json");
    std::fs::write(&fixture, serde_json::to_vec(session).unwrap()).unwrap();
    Command::new(env!("CARGO_BIN_EXE_fake-control"))
        .args(["--fixture", fixture.to_str().unwrap()])
        .args(["--page", &format!("{ROOT}/harness/pages/overlay-test.html")])
        .args(["--score", &format!("{ROOT}/harness/pages/score.json")])
        .args(["--beats", dir.join("beats.jsonl").to_str().unwrap()])
        .args(["--listen", "127.0.0.1:0"])
        .args(["--input-origin", "srt://mediamtx:8890"])
        .args(["--target-origin", "rtmp://mediamtx:1935"])
        .args(["--overlay-origin", "http://fake-control:7000"])
        .args(["--stream-id", "read:cam", "--stream-key", "dest"])
        .env("JOB_TOKEN", TOKEN)
        .env("SESSION_ID", SESSION_ID)
        .env("HARNESS_SRT_PASSPHRASE", "0123456789abcdef")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("fake-control runs")
}

/// The binary's first stdout line (it prints one line either way: listening, or its refusal), and
/// its exit code if it exited within the wait. A binary still running is killed.
fn first_line_and_exit(mut child: Child) -> (String, Option<i32>) {
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(30)).unwrap_or_default();
    let mut code = None;
    for _ in 0..100 {
        if let Some(status) = child.try_wait().unwrap() {
            code = status.code();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if code.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    (line, code)
}

fn dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("harness-fake-control-{}-{name}", std::process::id()))
}

#[test]
fn a_session_the_compositor_would_refuse_stops_fake_control_before_it_listens() {
    // The positive pair: the vendored session is served.
    let (line, code) = first_line_and_exit(spawn(&dir("ok"), &fixture()));
    eprintln!("vendored session: {line:?}, exit {code:?}");
    assert!(
        line.starts_with("fake-control: listening on 127.0.0.1:0; the session parses (composed, 1280x720)"),
        "{line:?}"
    );
    assert_eq!(code, None, "the binary keeps serving the vendored session");

    // The overlay URL loses `delay=` (C07: it must appear once and match `overlayDelayMs`). The
    // rewrite keeps the query as it is, so only the self-check can refuse it.
    let mut refused = fixture();
    let url = refused["overlayUrl"].as_str().unwrap().replace("delay=", "pace=");
    assert!(!url.contains("delay="), "the fixture's overlayUrl carried delay=");
    refused["overlayUrl"] = Value::String(url);
    let (line, code) = first_line_and_exit(spawn(&dir("refused"), &refused));
    eprintln!("refused session: {line:?}, exit {code:?}");
    assert_eq!(code, Some(78), "{line:?}");
    assert!(line.contains("the rewritten session does not parse"), "{line:?}");
    assert!(!line.contains(TOKEN) && !line.contains(SESSION_ID), "{line:?}");
}
