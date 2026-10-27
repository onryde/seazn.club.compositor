//! The control worker (C08): its two lanes (the fetch and periodic beats on one plane instance, final
//! beats on another, each on its own thread; review round 1), its three request slots, and the mapping
//! of every result into an `Inbound` message. Every wait is bounded and FAILS on its own message rather
//! than hang.
use std::time::{Duration, Instant};

use app::control_worker::{spawn, ControlHandle};
use app::ports::{ControlQueue, ControlRequest, FetchError, HeartbeatError, Inbound, Measured, Reply};
use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError};
use domain::health::{BeatState, ExitDetail, ExitInfo, GraphicsStatus, HealthReport, LinkStatus};
use domain::ladder::Rung;
use domain::lifecycle::ExitReason;
use domain::programme::ProgrammePhase;
use domain::supervisor::{Desired, Event, HeartbeatRefusal, Refusal};
use testkit::fakes::{session_720p, PlaneCall, PlaneLog, ScriptedPlane};

const WAIT: Duration = Duration::from_secs(3);
const SLOW: Duration = Duration::from_millis(300);
/// A periodic POST stalled past `WAIT` and past the final beat's 2 s bound (review round 1, item 1).
const STALL: Duration = Duration::from_secs(5);

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

/// A final beat (`exit` is `Some`), told apart by its reason and `heartbeat_failures`.
fn final_beat(mark: u32, reason: ExitReason) -> HealthReport {
    HealthReport {
        state: BeatState::Stopped,
        exit: Some(ExitInfo { reason, detail: None }),
        ..periodic(mark)
    }
}

fn beat(h: HealthReport) -> ControlRequest {
    ControlRequest::Heartbeat(h, Measured::default())
}

fn marks(log: &PlaneLog) -> Vec<(u32, bool)> {
    log.beats()
        .into_iter()
        .map(|h| (h.heartbeat_failures, h.exit.is_some()))
        .collect()
}

fn started(log: &PlaneLog) -> usize {
    log.calls()
        .iter()
        .filter(|c| matches!(c, PlaneCall::BeatStarted(_)))
        .count()
}

fn recv(rx: &Receiver<Inbound>) -> Inbound {
    rx.recv_timeout(WAIT).expect("no message from the worker")
}

/// Periodic beats take `SLOW`; final beats answer at once. Both answer `Live`.
fn slow_periodic() -> (ScriptedPlane, PlaneLog) {
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(|h| {
        let d = if h.exit.is_some() { Duration::ZERO } else { SLOW };
        (d, Ok(Desired::Live))
    });
    (plane, log)
}

/// Starts periodic beat `mark` and waits until the plane is inside its call.
fn in_flight(h: &mut ControlHandle, log: &PlaneLog, mark: u32) {
    let before = started(log);
    h.request(beat(periodic(mark)));
    assert!(
        log.wait_for(WAIT, |c| c
            .iter()
            .filter(|c| matches!(c, PlaneCall::BeatStarted(_)))
            .count()
            > before),
        "beat {mark} never started"
    );
}

#[test]
fn fetch_success_arrives_as_session_message() {
    let (tx, rx) = unbounded();
    let (plane, log) = ScriptedPlane::new(session_720p());
    let mut h = spawn(Box::new(plane), tx);
    h.request(ControlRequest::FetchSession);
    assert_eq!(recv(&rx), Inbound::Session(session_720p()));
    assert_eq!(log.calls(), vec![PlaneCall::FetchStarted, PlaneCall::Fetched]);
}

#[test]
fn refusal_arrives_as_bootstrap_refused() {
    let all = [
        Refusal::Gone,
        Refusal::Unauthorized,
        Refusal::NotFound,
        Refusal::WrongMode,
    ];
    let (tx, rx) = unbounded();
    let (plane, _log) = ScriptedPlane::new(session_720p());
    let plane = plane.fetches(all.iter().map(|r| Err(FetchError::Refused(*r))).collect());
    let mut h = spawn(Box::new(plane), tx);
    let mut checked = 0;
    for r in all {
        h.request(ControlRequest::FetchSession);
        assert_eq!(recv(&rx), Inbound::Event(Event::BootstrapRefused(r)), "{r:?}");
        checked += 1;
    }
    eprintln!("refusals checked: {checked}");
    assert_eq!(checked, 4);
}

#[test]
fn transient_arrives_as_bootstrap_failed() {
    let (tx, rx) = unbounded();
    let (plane, _log) = ScriptedPlane::new(session_720p());
    let plane = plane.fetches(vec![Err(FetchError::Transient("connect refused".into()))]);
    let mut h = spawn(Box::new(plane), tx);
    h.request(ControlRequest::FetchSession);
    assert_eq!(recv(&rx), Inbound::Event(Event::BootstrapFailed));
    // The next fetch (the domain's retry) succeeds.
    h.request(ControlRequest::FetchSession);
    assert_eq!(recv(&rx), Inbound::Session(session_720p()));
}

/// Every heartbeat result, on a periodic beat and on a final one alike (Owed 29: C07 returns `Gone`
/// for every 410, the final beat's included).
#[test]
fn heartbeat_reply_and_failure_arrive_as_events() {
    let script: Vec<(Result<Desired, HeartbeatError>, Event)> = vec![
        (Ok(Desired::Live), Event::HeartbeatReply(Desired::Live)),
        (Ok(Desired::Ending), Event::HeartbeatReply(Desired::Ending)),
        (
            Err(HeartbeatError::Transport("HTTP 503".into())),
            Event::HeartbeatFailed,
        ),
        (
            Err(HeartbeatError::Transport("HTTP 404".into())),
            Event::HeartbeatFailed,
        ),
        (
            Err(HeartbeatError::Gone),
            Event::HeartbeatRefused(HeartbeatRefusal::Gone),
        ),
    ];
    let mut checked = 0;
    for is_final in [false, true] {
        for (result, want) in script.clone() {
            let (tx, rx) = unbounded();
            let (plane, log) = ScriptedPlane::new(session_720p());
            let plane = plane.reply(move |_| (Duration::ZERO, result.clone()));
            let mut h = spawn(Box::new(plane), tx);
            let report = if is_final {
                final_beat(1, ExitReason::Ended)
            } else {
                periodic(1)
            };
            h.request(beat(report.clone()));
            assert_eq!(
                recv(&rx),
                Inbound::Event(want.clone()),
                "final={is_final} {want:?}"
            );
            assert_eq!(log.beats(), vec![report]);
            checked += 1;
        }
    }
    eprintln!("results checked: {checked}");
    assert_eq!(checked, 10);
}

/// Owed 108 fix (b): a periodic reply carrying web's phone-live counter posts its `HeartbeatReply`
/// and then `PhoneLiveEdge` with the counter as read, a repeat included (the domain, not the worker,
/// tells an edge from a repeat). The empty cases: a reply with no counter, a failed beat, and a final
/// beat's reply (nothing is rebuilt once the process is ending) post the one event they posted before.
/// A fetch requested after the expected messages proves nothing else came first on the worker's lane;
/// the final lane is read for a while instead.
#[test]
fn a_periodic_reply_with_a_phone_live_counter_posts_the_edge_after_it() {
    let edge = |desired, count| Reply {
        desired,
        phone_live_edge: Some(count),
    };
    let script: Vec<(bool, Result<Reply, HeartbeatError>, Vec<Event>)> = vec![
        (
            false,
            Ok(edge(Desired::Live, 0)),
            vec![
                Event::HeartbeatReply(Desired::Live),
                Event::PhoneLiveEdge { count: 0 },
            ],
        ),
        (
            false,
            Ok(edge(Desired::Ending, 7)),
            vec![
                Event::HeartbeatReply(Desired::Ending),
                Event::PhoneLiveEdge { count: 7 },
            ],
        ),
        (
            false,
            Ok(Desired::Live.into()),
            vec![Event::HeartbeatReply(Desired::Live)],
        ),
        (
            false,
            Err(HeartbeatError::Transport("HTTP 503".into())),
            vec![Event::HeartbeatFailed],
        ),
        (
            true,
            Ok(edge(Desired::Ending, 7)),
            vec![Event::HeartbeatReply(Desired::Ending)],
        ),
    ];
    let mut checked = 0;
    for (is_final, result, want) in script {
        let (tx, rx) = unbounded();
        let (plane, _log) = ScriptedPlane::new(session_720p());
        let plane = plane.replies(move |_| (Duration::ZERO, result.clone()));
        let mut h = spawn(Box::new(plane), tx);
        let report = if is_final {
            final_beat(1, ExitReason::Ended)
        } else {
            periodic(1)
        };
        h.request(beat(report));
        let got: Vec<Inbound> = want.iter().map(|_| recv(&rx)).collect();
        let want: Vec<Inbound> = want.into_iter().map(Inbound::Event).collect();
        assert_eq!(got, want, "final={is_final}");
        if is_final {
            assert!(
                rx.recv_timeout(SLOW).is_err(),
                "the final lane posted after its reply"
            );
        } else {
            h.request(ControlRequest::FetchSession);
            assert_eq!(
                recv(&rx),
                Inbound::Session(session_720p()),
                "nothing between the reply's messages and the next fetch"
            );
        }
        checked += 1;
    }
    eprintln!("replies checked: {checked}");
    assert_eq!(checked, 5);
}

/// The periodic slot holds one beat behind the one in flight; a newer beat replaces it (drop-oldest,
/// AGENTS.md §4: the freshest state is the one worth sending) and the replaced one is counted.
#[test]
fn slow_heartbeat_does_not_queue_a_second_one() {
    let (tx, _rx) = unbounded();
    let (plane, log) = slow_periodic();
    let mut h = spawn(Box::new(plane), tx);
    in_flight(&mut h, &log, 1);
    h.request(beat(periodic(2)));
    h.request(beat(periodic(3)));
    assert_eq!(h.dropped(), 1, "beat 2 was replaced");
    assert!(log.wait_for(WAIT, |c| c
        .iter()
        .filter(|c| matches!(c, PlaneCall::Beat(..)))
        .count()
        >= 2));
    std::thread::sleep(SLOW + SLOW / 2);
    assert_eq!(
        marks(&log),
        vec![(1, false), (3, false)],
        "at most 2 calls: in flight + the freshest"
    );
}

#[test]
fn fetch_is_never_dropped_behind_a_slow_heartbeat() {
    let (tx, rx) = unbounded();
    let (plane, log) = slow_periodic();
    let mut h = spawn(Box::new(plane), tx);
    in_flight(&mut h, &log, 1);
    h.request(beat(periodic(2)));
    h.request(ControlRequest::FetchSession);
    assert_eq!(h.dropped(), 0);
    // Messages: beat 1's reply, then the session (the fetch outranks the pending beat), then beat 2's.
    let got: Vec<Inbound> = (0..3).map(|_| recv(&rx)).collect();
    assert_eq!(
        got,
        vec![
            Inbound::Event(Event::HeartbeatReply(Desired::Live)),
            Inbound::Session(session_720p()),
            Inbound::Event(Event::HeartbeatReply(Desired::Live)),
        ]
    );
    let order: Vec<&str> = log
        .calls()
        .iter()
        .filter_map(|c| match c {
            PlaneCall::Fetched => Some("fetch"),
            PlaneCall::Beat(..) => Some("beat"),
            _ => None,
        })
        .collect();
    assert_eq!(order, vec!["beat", "fetch", "beat"]);
}

/// Owed 28: a final beat is never dropped. It posts on its own lane while the periodic beat in flight
/// runs on, and it retires the pending periodic beat, so no periodic beat starts after it.
#[test]
fn a_final_beat_outranks_and_retires_a_pending_periodic_beat() {
    let (tx, _rx) = unbounded();
    let (plane, log) = slow_periodic();
    let mut h = spawn(Box::new(plane), tx);
    in_flight(&mut h, &log, 1);
    h.request(beat(periodic(2)));
    h.request(beat(final_beat(9, ExitReason::Fatal)));
    assert!(h.flush(WAIT), "the final beat was posted");
    assert_eq!(marks(&log), vec![(9, true)], "beat 1 is still in flight");
    std::thread::sleep(SLOW + SLOW / 2);
    assert_eq!(
        marks(&log),
        vec![(9, true), (1, false)],
        "the pending periodic beat never posts"
    );
    assert_eq!(started(&log), 2, "no periodic beat started after the final one");
    assert_eq!(h.dropped(), 1, "the retired beat is counted");
}

/// A run sends at most two final beats (the drain's, then a fatal error's), and the slot holds two;
/// a third still posts (its `request` waits for room rather than drop it).
#[test]
fn every_final_beat_is_posted_a_third_included() {
    let (tx, _rx) = unbounded();
    let (plane, log) = slow_periodic();
    let mut h = spawn(Box::new(plane), tx);
    in_flight(&mut h, &log, 1);
    for m in [7, 8, 9] {
        h.request(beat(final_beat(m, ExitReason::Signalled)));
    }
    assert!(h.flush(WAIT));
    assert_eq!(
        marks(&log),
        vec![(7, true), (8, true), (9, true)],
        "in order, beat 1 still in flight"
    );
    assert!(log.wait_for(WAIT, |c| c
        .iter()
        .any(|c| matches!(c, PlaneCall::Beat(h, _) if h.heartbeat_failures == 1))));
    assert_eq!(h.dropped(), 0);
}

#[test]
fn flush_with_no_final_beat_returns_true_at_once() {
    let (tx, _rx) = unbounded();
    let (plane, log) = slow_periodic();
    let mut h = spawn(Box::new(plane), tx);
    in_flight(&mut h, &log, 1);
    let t0 = Instant::now();
    assert!(h.flush(WAIT), "nothing to flush");
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "a periodic beat is not waited for"
    );
}

/// `flush` waits for the final beat's call to return, and no longer than its timeout.
#[test]
fn flush_waits_for_the_final_beat_and_never_past_its_timeout() {
    for (takes, timeout, posted) in [(150u64, 2_000u64, true), (1_000, 200, false)] {
        let (tx, _rx) = unbounded();
        let (plane, log) = ScriptedPlane::new(session_720p());
        let plane = plane.reply(move |_| (Duration::from_millis(takes), Ok(Desired::Live)));
        let mut h = spawn(Box::new(plane), tx);
        h.request(beat(final_beat(1, ExitReason::Ended)));
        let t0 = Instant::now();
        assert_eq!(
            h.flush(Duration::from_millis(timeout)),
            posted,
            "takes {takes} ms"
        );
        let waited = t0.elapsed();
        let bound = Duration::from_millis(takes.min(timeout));
        assert!(
            waited + Duration::from_millis(20) >= bound,
            "returned early: {waited:?}"
        );
        assert!(
            waited < bound + Duration::from_millis(250),
            "returned late: {waited:?}"
        );
        assert_eq!(log.beats().len(), usize::from(posted));
    }
}

/// The ack comes before the result is posted: a full inbound channel cannot hold up `flush`.
#[test]
fn flush_is_not_held_up_by_a_full_inbound_channel() {
    let (tx, rx) = bounded(1);
    tx.send(Inbound::Event(Event::InputFrame { gen: 1 }))
        .expect("fill");
    let (plane, log) = ScriptedPlane::new(session_720p());
    let mut h = spawn(Box::new(plane), tx);
    h.request(beat(final_beat(1, ExitReason::Ended)));
    assert!(
        h.flush(WAIT),
        "the final beat posted, and the ack did not wait on the inbound channel"
    );
    assert_eq!(log.beats().len(), 1);
    drop(rx);
}

/// The worker ends when the loop's handle is dropped, so its sender closes.
#[test]
fn the_worker_ends_with_its_handle() {
    let (tx, rx) = unbounded();
    let (plane, _log) = ScriptedPlane::new(session_720p());
    let h = spawn(Box::new(plane), tx);
    drop(h);
    assert_eq!(rx.recv_timeout(WAIT), Err(RecvTimeoutError::Disconnected));
}

/// The fatal text rides the final beat to the plane verbatim: the worker never touches it (C07 is
/// the one redactor).
#[test]
fn the_final_beat_reaches_the_plane_with_its_detail() {
    let (tx, _rx) = unbounded();
    let (plane, log) = ScriptedPlane::new(session_720p());
    let mut h = spawn(Box::new(plane), tx);
    let mut report = final_beat(1, ExitReason::Fatal);
    report.exit = Some(ExitInfo {
        reason: ExitReason::Fatal,
        detail: Some(ExitDetail::new("x")),
    });
    h.request(beat(report.clone()));
    assert!(h.flush(WAIT));
    assert_eq!(log.beats(), vec![report]);
}

/// A lane that died (its plane panicked) can post nothing: `flush` says so at once rather than report
/// a final beat posted that never was, or wait out its timeout for one that cannot come. The plane
/// panics on every beat: a periodic one ends the `control` thread, a first final beat the final lane;
/// with both gone the inbound channel closes, and the next final beat finds its lane gone.
#[test]
fn flush_reports_a_final_beat_the_worker_could_not_take() {
    let (tx, rx) = unbounded();
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(|h| panic!("planted: the plane fails on beat {}", h.heartbeat_failures));
    let mut h = spawn(Box::new(plane), tx);
    // Taken before the final beat comes, which would retire it.
    in_flight(&mut h, &log, 1);
    h.request(beat(final_beat(2, ExitReason::Fatal)));
    assert_eq!(
        rx.recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected),
        "both lanes died"
    );
    assert_eq!(started(&log), 2, "each lane took its beat");
    h.request(beat(final_beat(3, ExitReason::Fatal)));
    let t0 = Instant::now();
    assert!(!h.flush(WAIT), "a final beat nobody took is not posted");
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "and flush says so at once"
    );
    assert!(log.beats().is_empty());
}

/// The worker's own order (Owed 28), apart from the retire: a periodic beat requested AFTER a final
/// one waits behind the beat in flight on its lane, while the final one has already posted on its own.
/// The domain sends no periodic beat after a final one; this pins the worker alone.
#[test]
fn a_final_beat_outranks_a_periodic_beat_requested_after_it() {
    let (tx, _rx) = unbounded();
    let (plane, log) = slow_periodic();
    let mut h = spawn(Box::new(plane), tx);
    in_flight(&mut h, &log, 1);
    h.request(beat(final_beat(9, ExitReason::Signalled)));
    h.request(beat(periodic(2)));
    assert!(h.flush(WAIT));
    assert!(log.wait_for(WAIT, |c| c
        .iter()
        .filter(|c| matches!(c, PlaneCall::Beat(..)))
        .count()
        >= 3));
    assert_eq!(marks(&log), vec![(9, true), (1, false), (2, false)]);
}

/// Review round 1, item 1 (the coordinator's ruling): final beats have their own lane, a second plane
/// instance on its own thread, so a periodic beat in flight, stalled past the bound, never holds one up.
#[test]
fn a_final_beat_is_posted_while_a_periodic_beat_is_stalled_in_flight() {
    let (tx, _rx) = unbounded();
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(|h| {
        let d = if h.exit.is_some() { Duration::ZERO } else { STALL };
        (d, Ok(Desired::Live))
    });
    let mut h = spawn(Box::new(plane), tx);
    in_flight(&mut h, &log, 1);
    let t0 = Instant::now();
    h.request(beat(final_beat(9, ExitReason::Fatal)));
    assert!(h.flush(WAIT), "the final beat was posted");
    let took = t0.elapsed();
    assert!(
        took < Duration::from_secs(1),
        "at once, not behind the stalled beat: {took:?}"
    );
    assert_eq!(
        marks(&log),
        vec![(9, true)],
        "the periodic beat is still in flight"
    );
}

/// Review round 1, item 2: the loop stops reading once it exits, so a full inbound channel must not hold
/// a final beat behind the result of the one before it. The results are not lost: they post, in order,
/// once the channel has room.
#[test]
fn a_second_final_beat_is_posted_while_the_first_result_waits_on_a_full_channel() {
    let (tx, rx) = bounded(1);
    tx.send(Inbound::Event(Event::InputFrame { gen: 1 }))
        .expect("fill");
    let (plane, log) = ScriptedPlane::new(session_720p());
    let plane = plane.reply(|h| {
        let d = if h.heartbeat_failures == 1 {
            Desired::Ending
        } else {
            Desired::Live
        };
        (Duration::ZERO, Ok(d))
    });
    let mut h = spawn(Box::new(plane), tx);
    h.request(beat(final_beat(1, ExitReason::Signalled)));
    h.request(beat(final_beat(2, ExitReason::Fatal)));
    assert!(h.flush(WAIT), "both final beats posted, the channel still full");
    assert_eq!(marks(&log), vec![(1, true), (2, true)]);
    let got: Vec<Inbound> = (0..3).map(|_| recv(&rx)).collect();
    assert_eq!(
        got,
        vec![
            Inbound::Event(Event::InputFrame { gen: 1 }),
            Inbound::Event(Event::HeartbeatReply(Desired::Ending)),
            Inbound::Event(Event::HeartbeatReply(Desired::Live)),
        ],
        "each result posted once there was room, the first beat's first"
    );
}

/// The loop may go (it returned) while the handle lives on: the final lane serves on. A result it
/// cannot post is dropped, and the next final beat still posts.
#[test]
fn the_final_lane_serves_on_after_the_loop_has_gone() {
    let (tx, rx) = unbounded();
    let (plane, log) = ScriptedPlane::new(session_720p());
    let mut h = spawn(Box::new(plane), tx);
    drop(rx);
    h.request(beat(final_beat(1, ExitReason::Signalled)));
    assert!(log.wait_for(WAIT, |c| c
        .iter()
        .any(|c| matches!(c, PlaneCall::Beat(h, _) if h.heartbeat_failures == 1))));
    // Long enough for the lane to try the first result's post, and find the loop gone.
    std::thread::sleep(Duration::from_millis(50));
    h.request(beat(final_beat(2, ExitReason::Fatal)));
    assert!(h.flush(WAIT), "the second final beat posted too");
    assert_eq!(marks(&log), vec![(1, true), (2, true)]);
}
