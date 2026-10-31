//! R4 task SC1: the control worker carries the sponsor airings on every beat it posts, and only a beat
//! answered with a reply (C07's 200, the contract's 2xx) clears what it carried (spec 2026-10-27
//! §3.6.2, "cleared only on a 2xx reply; the final beat flushes"). Driven through the real worker and
//! its two lanes, with testkit's scripted plane recording what each beat carried. Each beat is awaited
//! (its result on the inbound channel) before the next is requested, so no beat replaces another in
//! the periodic slot.
use std::time::Duration;

use app::airings::Airings;
use app::control_worker::{spawn, spawn_with_airings};
use app::ports::{ControlQueue, ControlRequest, HeartbeatError, Inbound, Measured};
use crossbeam_channel::{unbounded, Receiver};
use domain::airings::{AiringBatch, AiringLine, AiringSource, AiringStyle};
use domain::health::{BeatState, ExitInfo, GraphicsStatus, HealthReport, LinkStatus};
use domain::ladder::Rung;
use domain::lifecycle::ExitReason;
use domain::programme::ProgrammePhase;
use domain::supervisor::{Desired, Event, HeartbeatRefusal};
use testkit::fakes::{session_720p, PlaneCall, PlaneLog, ScriptedPlane};

const WAIT: Duration = Duration::from_secs(3);
const SLOW: Duration = Duration::from_millis(300);

fn line(n: u32) -> AiringLine {
    AiringLine::start(
        format!("{n}:sponsor.set_break"),
        "card-1".to_owned(),
        AiringStyle::LowerThird,
        AiringSource::Rotation,
        Some("set_break".to_owned()),
        "2026-10-31T12:00:00.000Z".to_owned(),
    )
    .expect("within web's bounds")
}

fn ids(b: &AiringBatch) -> Vec<String> {
    b.lines().iter().map(|l| l.moment_id().to_owned()).collect()
}

fn want(ns: &[u32]) -> Vec<String> {
    ns.iter().map(|n| format!("{n}:sponsor.set_break")).collect()
}

/// A periodic beat, told apart from the others by `heartbeat_failures`.
fn periodic(mark: u32) -> HealthReport {
    HealthReport {
        state: BeatState::Playing,
        programme_phase: ProgrammePhase::Live,
        input: LinkStatus::Connected,
        output: LinkStatus::Connected,
        graphics: GraphicsStatus::Ready,
        input_kicks: 0,
        input_kick_alert: false,
        pipeline_rebuilds: 0,
        sink_swaps: 0,
        dropped_frames: 0,
        rung: Rung::Full,
        score_stale: None,
        heartbeat_failures: mark,
        phase_channel: None,
        exit: None,
    }
}

fn final_beat(mark: u32) -> HealthReport {
    HealthReport {
        state: BeatState::Stopped,
        exit: Some(ExitInfo {
            reason: ExitReason::Signalled,
            detail: None,
        }),
        ..periodic(mark)
    }
}

fn beat(h: HealthReport) -> ControlRequest {
    ControlRequest::Heartbeat(h, Measured::default())
}

/// What each completed beat carried: its mark, whether it was a final beat, and its moment ids.
fn carried(log: &PlaneLog) -> Vec<(u32, bool, Vec<String>)> {
    log.beat_airings()
        .into_iter()
        .map(|(h, b)| (h.heartbeat_failures, h.exit.is_some(), ids(&b)))
        .collect()
}

fn recv(rx: &Receiver<Inbound>) -> Inbound {
    rx.recv_timeout(WAIT).expect("no message from the worker")
}

/// A plane whose beats answer by mark: `fail` lists the marks that fail, with what.
fn scripted(fail: Vec<(u32, HeartbeatError)>, delay: Duration) -> (ScriptedPlane, PlaneLog) {
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(move |h| {
        let r = fail
            .iter()
            .find(|(m, _)| *m == h.heartbeat_failures)
            .map_or(Ok(Desired::Live), |(_, e)| Err(e.clone()));
        (delay, r)
    });
    (plane, log)
}

fn status(code: u16) -> HeartbeatError {
    HeartbeatError::Transport(format!("heartbeat refused: status {code}"))
}

#[test]
fn a_beat_answered_5xx_keeps_the_items_and_a_2xx_clears_them() {
    let (tx, rx) = unbounded();
    let (plane, log) = scripted(vec![(1, status(503)), (2, status(503))], Duration::ZERO);
    let airings = Airings::on();
    let mut h = spawn_with_airings(Box::new(plane), tx, airings.clone());
    assert!(airings.push(line(1)) && airings.push(line(2)));
    h.request(beat(periodic(1)));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatFailed));
    assert_eq!(airings.held(), 2, "a 5xx keeps them");
    // A second, identical beat after the 5xx resends the same items.
    h.request(beat(periodic(2)));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatFailed));
    h.request(beat(periodic(3)));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatReply(Desired::Live)));
    assert_eq!(airings.held(), 0, "the 2xx cleared them");
    assert!(airings.push(line(3)));
    h.request(beat(periodic(4)));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatReply(Desired::Live)));
    h.request(beat(periodic(5)));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatReply(Desired::Live)));
    assert_eq!(
        carried(&log),
        vec![
            (1, false, want(&[1, 2])),
            (2, false, want(&[1, 2])),
            (3, false, want(&[1, 2])),
            (4, false, want(&[3])),
            (5, false, vec![]),
        ]
    );
    assert_eq!(airings.held(), 0);
}

/// Every outcome that is not a reply keeps the lines: a 5xx, a 4xx, a reply that is none (all C07's
/// `Transport`), and a 410.
#[test]
fn every_outcome_but_a_reply_keeps_the_items() {
    let failures = [
        status(500),
        status(503),
        status(400),
        status(401),
        status(404),
        HeartbeatError::Gone,
    ];
    let mut checked = 0;
    for e in failures {
        let (tx, rx) = unbounded();
        let (plane, log) = scripted(vec![(1, e.clone())], Duration::ZERO);
        let airings = Airings::on();
        let mut h = spawn_with_airings(Box::new(plane), tx, airings.clone());
        airings.push(line(1));
        h.request(beat(periodic(1)));
        let want_event = match e {
            HeartbeatError::Gone => Event::HeartbeatRefused(HeartbeatRefusal::Gone),
            HeartbeatError::Transport(_) => Event::HeartbeatFailed,
        };
        assert_eq!(recv(&rx), Inbound::Event(want_event), "{e:?}");
        assert_eq!(airings.held(), 1, "{e:?} keeps it");
        h.request(beat(periodic(2)));
        assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatReply(Desired::Live)));
        assert_eq!(
            carried(&log),
            vec![(1, false, want(&[1])), (2, false, want(&[1]))],
            "{e:?}: resent"
        );
        assert_eq!(airings.held(), 0, "{e:?}: the reply cleared it");
        checked += 1;
    }
    eprintln!("non-reply outcomes checked: {checked}");
    assert_eq!(checked, 6);
}

#[test]
fn the_final_beat_flushes_the_buffer() {
    let (tx, rx) = unbounded();
    let (plane, log) = scripted(vec![(1, status(503))], Duration::ZERO);
    let airings = Airings::on();
    let mut h = spawn_with_airings(Box::new(plane), tx, airings.clone());
    airings.push(line(1));
    h.request(beat(periodic(1)));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatFailed));
    airings.push(line(2));
    h.request(beat(final_beat(9)));
    assert!(h.flush(WAIT), "the final beat posted");
    assert_eq!(
        carried(&log),
        vec![(1, false, want(&[1])), (9, true, want(&[1, 2]))],
        "the final beat carries every line still held, the failed beat's and the later one"
    );
    assert_eq!(airings.held(), 0, "its 2xx left none");
}

/// The drain's final beat fails, and a fatal error's final beat follows (C06 review m2): the second
/// sends again what the first did not land.
#[test]
fn a_failed_final_beat_leaves_its_lines_for_the_next_final_beat() {
    let (tx, _rx) = unbounded();
    let (plane, log) = scripted(vec![(8, status(502))], Duration::ZERO);
    let airings = Airings::on();
    let mut h = spawn_with_airings(Box::new(plane), tx, airings.clone());
    airings.push(line(1));
    h.request(beat(final_beat(8)));
    assert!(h.flush(WAIT));
    assert_eq!(airings.held(), 1, "the failed final beat kept it");
    h.request(beat(final_beat(9)));
    assert!(h.flush(WAIT));
    assert_eq!(carried(&log), vec![(8, true, want(&[1])), (9, true, want(&[1]))]);
    assert_eq!(airings.held(), 0);
}

/// A line pushed while a beat is in flight is not in that beat's snapshot, so its 2xx keeps it.
#[test]
fn a_line_pushed_while_a_beat_is_in_flight_survives_that_beats_2xx() {
    let (tx, rx) = unbounded();
    let (plane, log) = scripted(vec![], SLOW);
    let airings = Airings::on();
    let mut h = spawn_with_airings(Box::new(plane), tx, airings.clone());
    airings.push(line(1));
    h.request(beat(periodic(1)));
    assert!(
        log.wait_for(WAIT, |c| c.iter().any(|c| matches!(c, PlaneCall::BeatStarted(_)))),
        "beat 1 never started"
    );
    airings.push(line(2));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatReply(Desired::Live)));
    assert_eq!(airings.held(), 1, "line 2 arrived after the snapshot");
    h.request(beat(periodic(2)));
    assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatReply(Desired::Live)));
    assert_eq!(
        carried(&log),
        vec![(1, false, want(&[1])), (2, false, want(&[2]))]
    );
    assert_eq!(airings.held(), 0);
}

/// Off (the default, and `spawn`'s): a push holds nothing, and no beat carries any airing.
#[test]
fn off_no_beat_carries_any_airing() {
    let off = Airings::off();
    assert!(!off.is_on() && !Airings::default().is_on(), "off is the default");
    assert!(!off.push(line(1)), "a push holds nothing");
    assert_eq!((off.held(), off.for_beat()), (0, AiringBatch::default()));
    let mut checked = 0;
    for use_spawn in [true, false] {
        let (tx, rx) = unbounded();
        let (plane, log) = scripted(vec![], Duration::ZERO);
        let mut h = if use_spawn {
            spawn(Box::new(plane), tx)
        } else {
            spawn_with_airings(Box::new(plane), tx, off.clone())
        };
        off.push(line(2));
        h.request(beat(periodic(1)));
        assert_eq!(recv(&rx), Inbound::Event(Event::HeartbeatReply(Desired::Live)));
        h.request(beat(final_beat(9)));
        assert!(h.flush(WAIT));
        let beats = carried(&log);
        assert_eq!(beats.len(), 2);
        assert!(beats.iter().all(|(_, _, ids)| ids.is_empty()), "{beats:?}");
        assert!(!log.calls().iter().any(|c| matches!(c, PlaneCall::Airings(_))));
        checked += beats.len();
    }
    eprintln!("beats checked carrying none: {checked}");
    assert_eq!(checked, 4);
}
