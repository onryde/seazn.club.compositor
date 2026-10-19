//! No credential and no fatal error's text in any `Debug` of a C08 type, nor in any line the driver
//! logs (AGENTS.md §7; Owed 31, 32). The values are planted, made up, and each is searched for in
//! full and by a distinctive fragment. The positive pair: the fatal text did reach the final beat.
use std::time::Duration;

use app::control_worker::spawn;
use app::driver::Driver;
use app::ports::{ControlQueue, ControlRequest, Inbound, InputSpec, Measured, SessionConfig, TargetSpec};
use crossbeam_channel::unbounded;
use domain::config::Thresholds;
use domain::health::ExitDetail;
use domain::supervisor::Event;
use testkit::fakes::{ports, session_720p, Call, FakeProgramme, Log, ScriptedPlane};

const PASSPHRASE: &str = "planted-passphrase-q7Zx";
const STREAM_ID: &str = "planted-stream-id-W3kp";
const STREAM_KEY: &str = "planted-stream-key-M9vb";
const PAGE_TOKEN: &str = "planted-page-token-R4tn";
const FATAL: &str = "planted-fatal-text-srt://host/key=Y8hd";

const ALL: [&str; 5] = [PASSPHRASE, STREAM_ID, STREAM_KEY, PAGE_TOKEN, FATAL];

fn planted_session() -> SessionConfig {
    let s = session_720p();
    SessionConfig {
        input: InputSpec {
            url: s.input.url.clone(),
            stream_id: STREAM_ID.into(),
            passphrase: PASSPHRASE.into(),
        },
        target: TargetSpec {
            url: format!("rtmps://live.cloudflare.com:443/live/{STREAM_KEY}"),
            stream_key: STREAM_KEY.into(),
        },
        overlay_url: format!("https://seazn.club/overlay/fixtures/x?st={PAGE_TOKEN}&delay=3100"),
        ..s
    }
}

/// Every planted value, and its last 4 characters (a distinctive fragment), absent from `text`.
fn assert_clean(what: &str, text: &str, checked: &mut usize) {
    for v in ALL {
        let tail = &v[v.len() - 4..];
        assert!(
            !text.contains(v) && !text.contains(tail),
            "{what} leaks {v:?}: {text}"
        );
        *checked += 1;
    }
}

fn both(what: &str, x: &impl std::fmt::Debug, checked: &mut usize) {
    assert_clean(what, &format!("{x:?}"), checked);
    assert_clean(what, &format!("{x:#?}"), checked);
}

#[test]
fn no_c08_debug_prints_a_credential_or_the_fatal_text() {
    let mut checked = 0;
    let fatal = || Event::Fatal {
        detail: Some(ExitDetail::new(FATAL)),
    };
    both(
        "Inbound::Session",
        &Inbound::Session(planted_session()),
        &mut checked,
    );
    both("Inbound::Event(Fatal)", &Inbound::Event(fatal()), &mut checked);

    // A driver that has wired the planted session, then exited on the planted fatal text.
    let log = Log::default();
    let mut d = Driver::new(
        Thresholds::default(),
        ports(&log, Measured::default()),
        100_000_000,
        0,
    );
    d.tick_due(0);
    assert_eq!(d.step(Inbound::Session(planted_session()), 10), None);
    d.tick_due(10);
    both("Driver (running)", &d, &mut checked);
    assert_eq!(d.step(Inbound::Event(fatal()), 20), Some(70));
    both("Driver (exited)", &d, &mut checked);

    // Every port call it made, the final beat's request included, and every line it logged.
    let calls = log.calls();
    let mut wiring = 0;
    for c in &calls {
        both("Call", c, &mut checked);
        if let Call::Control(r @ ControlRequest::Heartbeat(..)) = c {
            both("ControlRequest", r, &mut checked);
        }
        if matches!(
            c,
            Call::InputRebuild(..) | Call::Swap(..) | Call::GraphicsRestart(..)
        ) {
            wiring += 1;
        }
    }
    assert_eq!(
        wiring, 3,
        "the secret-bearing calls were made, so their Debug was checked"
    );
    for line in log.lines() {
        assert_clean("a log line", &line, &mut checked);
    }

    // The positive pair: the planted values reached the ports and the beat, verbatim.
    assert!(calls
        .iter()
        .any(|c| matches!(c, Call::InputRebuild(_, s) if s.passphrase == PASSPHRASE)));
    assert!(calls
        .iter()
        .any(|c| matches!(c, Call::Swap(_, t) if t.stream_key == STREAM_KEY)));
    assert!(calls
        .iter()
        .any(|c| matches!(c, Call::GraphicsRestart(_, u) if u.contains(PAGE_TOKEN))));
    let detail = calls.iter().find_map(|c| match c {
        Call::Control(ControlRequest::Heartbeat(h, _)) => h.exit.as_ref().and_then(|e| e.detail.clone()),
        _ => None,
    });
    assert_eq!(detail.as_ref().map(ExitDetail::as_str), Some(FATAL));

    // The control worker's handle, and the scripted plane holding the planted session.
    let (tx, _rx) = unbounded();
    let (plane, _plog) = ScriptedPlane::new(planted_session());
    both("ScriptedPlane", &plane, &mut checked);
    let mut h = spawn(Box::new(plane), tx);
    let Some(Call::Control(fin)) = calls
        .iter()
        .find(|c| matches!(c, Call::Control(ControlRequest::Heartbeat(h, _)) if h.exit.is_some()))
        .cloned()
    else {
        panic!("no final beat");
    };
    h.request(fin);
    both("ControlHandle", &h, &mut checked);
    assert!(h.flush(Duration::from_secs(3)));

    // A fake whose start fails with the planted text: its Debug, and the Ports holding it.
    let p = FakeProgramme {
        log: Log::default(),
        fail_start: Some(FATAL.into()),
    };
    let mut ports = ports(&Log::default(), Measured::default());
    ports.programme = Box::new(p);
    both("Ports", &ports, &mut checked);

    eprintln!("absences checked: {checked}");
    assert!(checked > 0);
}
