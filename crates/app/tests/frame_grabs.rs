//! R4 task SC2 (spec 2026-10-27 §3.6.3): the frame grabs through the real `Airings` handle, the real plan
//! and the real worker, over testkit's fake tap and scripted upload, on a manual clock. Each instant and
//! bound is `domain::config`'s (pinned to the spec there), each upload verdict the SC2 dispatch's.
//! `run_due` is the worker's whole step at one instant, so the timing here is exact; the last tests
//! drive the worker's own thread.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use app::airings::Airings;
use app::frame_grab::{spawn, FrameClock, FrameGrabs, FrameWorker, GrabRun, Observed};
use app::ports::FrameUploadError;
use domain::airings::{AiringLine, AiringSource, AiringStyle};
use domain::config::{FRAME_GRAB_DELAY_MS, FRAME_GRAB_MIN_GAP_MS, FRAME_UPLOAD_MAX_BYTES};
use domain::frame_grab::StartVerdict;
use domain::time::Ms;
use testkit::fakes::{FakeLog, FakeTap, Log, ScriptedUpload, TapLog, UploadLog, FAKE_FRAME_JPEG};

const DELAY: Ms = FRAME_GRAB_DELAY_MS;
const GAP: Ms = FRAME_GRAB_MIN_GAP_MS;
const WAIT: Duration = Duration::from_secs(3);

fn start(moment: &str) -> AiringLine {
    AiringLine::start(
        moment.to_owned(),
        "card-1".to_owned(),
        AiringStyle::LowerThird,
        AiringSource::Rotation,
        Some("set_break".to_owned()),
        "2026-10-31T12:00:00.000Z".to_owned(),
    )
    .expect("within web's bounds")
}

fn end(moment: &str, cut_short: bool) -> AiringLine {
    AiringLine::end(
        moment.to_owned(),
        "2026-10-31T12:00:01.000Z".to_owned(),
        cut_short,
    )
    .expect("within web's bounds")
}

/// A manual clock: the plan and the worker read it, the test sets it.
fn manual() -> (FrameClock, Arc<AtomicU64>) {
    let now = Arc::new(AtomicU64::new(0));
    let read = Arc::clone(&now);
    (Arc::new(move || read.load(Ordering::SeqCst)), now)
}

struct Rig {
    airings: Airings,
    worker: FrameWorker,
    tap: TapLog,
    uploads: UploadLog,
    log: Log,
    now: Arc<AtomicU64>,
}

impl Rig {
    fn new(tap: FakeTap, tap_log: TapLog, upload: ScriptedUpload, uploads: UploadLog) -> Rig {
        let (clock, now) = manual();
        let grabs = FrameGrabs::on(clock);
        let airings = Airings::on().with_frames(grabs.clone());
        let log = Log::default();
        let worker = FrameWorker::new(
            grabs,
            Box::new(tap),
            Box::new(upload),
            Box::new(FakeLog(log.clone())),
        );
        Rig {
            airings,
            worker,
            tap: tap_log,
            uploads,
            log,
            now,
        }
    }

    fn plain() -> Rig {
        let (tap, tap_log) = FakeTap::new();
        let (upload, uploads) = ScriptedUpload::new();
        Rig::new(tap, tap_log, upload, uploads)
    }

    fn uploading(answer: impl FnMut(usize) -> Result<(), FrameUploadError> + Send + 'static) -> Rig {
        let (tap, tap_log) = FakeTap::new();
        let (upload, uploads) = ScriptedUpload::new();
        Rig::new(tap, tap_log, upload.answer(answer), uploads)
    }

    /// `line` pushed at `at`, as the graphics adapter holds it for the beat.
    fn push(&self, line: AiringLine, at: Ms) {
        self.now.store(at, Ordering::SeqCst);
        assert!(self.airings.push(line), "held");
    }

    fn lines(&self) -> Vec<String> {
        self.log.lines()
    }

    fn said(&self, needle: &str) -> usize {
        self.lines().iter().filter(|l| l.contains(needle)).count()
    }
}

/// The start held at `t0`: nothing is grabbed a millisecond before `t0 + 1500`, one frame then, posted
/// with its moment id and the tap's bytes, and nothing after it.
#[test]
fn a_held_start_grabs_one_frame_1500_ms_later_and_posts_it() {
    let mut r = Rig::plain();
    let t0 = 40_000;
    r.push(start("12:sponsor.over_end"), t0);
    assert_eq!(r.worker.run_due(t0 + DELAY - 1), None);
    assert_eq!((r.tap.grabs(), r.uploads.calls().len()), (0, 0));
    assert_eq!(
        r.worker.run_due(t0 + DELAY),
        Some(GrabRun::Posted {
            moment_id: "12:sponsor.over_end".to_owned(),
            attempts: 1,
            bytes: FAKE_FRAME_JPEG.len(),
        })
    );
    let calls = r.uploads.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].moment_id, "12:sponsor.over_end");
    assert_eq!(calls[0].jpeg, FAKE_FRAME_JPEG);
    assert_eq!(r.tap.grabs(), 1);
    assert_eq!(
        r.lines(),
        vec![format!(
            "sponsor frame posted moment=12:sponsor.over_end bytes={} attempts=1",
            FAKE_FRAME_JPEG.len()
        )]
    );
    // A second call at the same instant, and long after: nothing more.
    assert_eq!(r.worker.run_due(t0 + DELAY), None);
    assert_eq!(r.worker.run_due(t0 + 10 * GAP), None);
    assert_eq!((r.tap.grabs(), r.uploads.calls().len()), (1, 1));
}

/// Off, three ways: the airings off (a push holds nothing, so cues nothing), the airings on with no
/// frame plan (SC1's handle, as every other caller builds it), and the plan itself off. None plans a
/// grab, and an off plan's worker starts no thread.
#[test]
fn off_nothing_is_planned_grabbed_or_posted() {
    let (clock, _) = manual();
    let on = FrameGrabs::on(clock);
    let airings_off = Airings::off().with_frames(on.clone());
    assert!(!airings_off.push(start("a")), "off holds nothing");
    assert_eq!(on.next_due(), None, "and cues nothing");

    let no_frames = Airings::on();
    assert!(no_frames.push(start("b")));
    assert!(!no_frames.frames().is_on());
    assert_eq!(no_frames.frames().next_due(), None);

    let off = FrameGrabs::off();
    assert_eq!(off.observe(&start("c")), None);
    assert_eq!((off.next_due(), off.take_due(Ms::MAX)), (None, None));
    let (tap, taps) = FakeTap::new();
    let (upload, uploads) = ScriptedUpload::new();
    let log = Log::default();
    let worker = FrameWorker::new(
        off,
        Box::new(tap),
        Box::new(upload),
        Box::new(FakeLog(log.clone())),
    );
    let handle = spawn(worker).expect("spawn");
    assert!(!handle.is_running(), "no thread for an off plan");
    assert_eq!((taps.grabs(), uploads.calls().len(), log.len()), (0, 0, 0));
}

/// A start, then its end inside the delay: the card left the air, or its build was retired or crashed
/// (SC1 writes those ends, cut short). No frame is grabbed, and the gap is not spent: the next start is
/// grabbed on time. An end of another moment keeps the grab.
#[test]
fn a_start_then_its_end_inside_1500_ms_grabs_nothing_and_leaves_the_gap_free() {
    let mut checked = 0;
    for cut_short in [true, false] {
        let mut r = Rig::plain();
        r.push(start("a"), 0);
        r.push(end("other", true), 100);
        r.push(end("a", cut_short), DELAY - 1);
        assert_eq!(r.worker.run_due(DELAY), None, "cut_short {cut_short}");
        assert_eq!(r.worker.run_due(10 * GAP), None, "cut_short {cut_short}");
        assert_eq!((r.tap.grabs(), r.uploads.calls().len()), (0, 0));
        r.push(start("b"), DELAY + 10);
        assert!(matches!(
            r.worker.run_due(2 * DELAY + 10),
            Some(GrabRun::Posted { .. })
        ));
        assert_eq!(r.uploads.moments(), vec!["b".to_owned()]);
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// An upload that fails then succeeds: a 5xx, or a transport error, is retried once, and the second
/// attempt's 2xx posts the frame. Nothing is said: no frame was lost.
#[test]
fn an_upload_that_fails_then_succeeds_posts_on_its_second_attempt() {
    let mut checked = 0;
    for first in [
        FrameUploadError::Failed("frame refused: status 503".to_owned()),
        FrameUploadError::Failed("frame transport: connection refused".to_owned()),
    ] {
        let mut r = Rig::uploading(move |n| if n == 0 { Err(first.clone()) } else { Ok(()) });
        r.push(start("a"), 0);
        assert_eq!(
            r.worker.run_due(DELAY),
            Some(GrabRun::Posted {
                moment_id: "a".to_owned(),
                attempts: 2,
                bytes: FAKE_FRAME_JPEG.len(),
            })
        );
        assert_eq!(r.uploads.moments(), vec!["a".to_owned(), "a".to_owned()]);
        assert_eq!(r.tap.grabs(), 1, "one frame, sent twice");
        assert_eq!(r.said("failed"), 0, "{:?}", r.lines());
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// Failed twice, the frame is dropped; a second grab that fails the same way is dropped too, and the
/// failure is said once in the process. A third, answered 2xx, posts: the plan was never stopped.
#[test]
fn an_upload_that_fails_twice_is_dropped_and_said_once() {
    let mut r = Rig::uploading(|n| {
        if n < 4 {
            Err(FrameUploadError::Failed("frame refused: status 502".to_owned()))
        } else {
            Ok(())
        }
    });
    r.push(start("a"), 0);
    assert_eq!(
        r.worker.run_due(DELAY),
        Some(GrabRun::GaveUp {
            moment_id: "a".to_owned(),
            attempts: 2
        })
    );
    r.push(start("b"), GAP);
    assert_eq!(
        r.worker.run_due(GAP + DELAY),
        Some(GrabRun::GaveUp {
            moment_id: "b".to_owned(),
            attempts: 2
        })
    );
    r.push(start("c"), 2 * GAP);
    assert!(matches!(
        r.worker.run_due(2 * GAP + DELAY),
        Some(GrabRun::Posted { attempts: 1, .. })
    ));
    assert_eq!(r.uploads.calls().len(), 5);
    assert_eq!(
        r.said("sponsor frame upload failed (frame refused: status 502) after 2 attempt(s)"),
        1,
        "{:?}",
        r.lines()
    );
    assert_eq!(r.said("said once"), 1, "{:?}", r.lines());
}

/// A 4xx (web will not take this frame: another session's token is a 404, a body it refuses a 413 or a
/// 415) gives the frame up at once, with no retry; the next airing is still grabbed.
#[test]
fn a_4xx_gives_the_frame_up_at_once() {
    let mut checked = 0;
    for code in [400u16, 401, 404, 413, 415, 422] {
        let mut r = Rig::uploading(move |n| {
            if n == 0 {
                Err(FrameUploadError::Refused(code))
            } else {
                Ok(())
            }
        });
        r.push(start("a"), 0);
        assert_eq!(
            r.worker.run_due(DELAY),
            Some(GrabRun::GaveUp {
                moment_id: "a".to_owned(),
                attempts: 1
            }),
            "{code}"
        );
        assert_eq!(
            r.said(&format!("upload failed (status {code}) after 1 attempt(s)")),
            1,
            "{code}"
        );
        r.push(start("b"), GAP);
        assert!(
            matches!(r.worker.run_due(GAP + DELAY), Some(GrabRun::Posted { .. })),
            "{code}"
        );
        assert_eq!(
            r.uploads.moments(),
            vec!["a".to_owned(), "b".to_owned()],
            "{code}"
        );
        checked += 1;
    }
    // The uploader's own refusal of an id that is not a path segment: given up at once too.
    let mut r = Rig::uploading(|n| {
        if n == 0 {
            Err(FrameUploadError::Unaddressable)
        } else {
            Ok(())
        }
    });
    r.push(start("a"), 0);
    assert_eq!(
        r.worker.run_due(DELAY),
        Some(GrabRun::GaveUp {
            moment_id: "a".to_owned(),
            attempts: 1
        })
    );
    checked += 1;
    eprintln!("4xx statuses checked: {checked}");
    assert_eq!(checked, 7);
}

/// A 410: the session is gone. Nothing is retried, the plan stops, and no later start is grabbed.
#[test]
fn a_410_stops_every_later_grab() {
    let mut r = Rig::uploading(|_| Err(FrameUploadError::Gone));
    r.push(start("a"), 0);
    assert_eq!(
        r.worker.run_due(DELAY),
        Some(GrabRun::SessionGone {
            moment_id: "a".to_owned()
        })
    );
    assert!(r.airings.frames().is_stopped());
    assert_eq!(
        r.airings.frames().observe(&start("b")),
        Some(Observed::Start(StartVerdict::Stopped))
    );
    r.push(start("c"), 5 * GAP);
    assert_eq!(r.worker.run_due(10 * GAP), None);
    assert_eq!((r.tap.grabs(), r.uploads.calls().len()), (1, 1));
    assert_eq!(
        r.lines(),
        vec!["sponsor frame upload: web answered 410 (session gone); no more frames".to_owned()]
    );
}

/// A frame the tap could not give, and one larger than web takes, are dropped before any upload, and
/// said once between them. One exactly at web's bound is posted.
#[test]
fn a_frame_not_grabbed_or_too_large_is_dropped_and_said_once() {
    let (tap, taps) = FakeTap::new();
    let tap = tap.answers(vec![
        Err("no programme frame within 1000 ms".to_owned()),
        Ok(vec![0xFF; FRAME_UPLOAD_MAX_BYTES + 1]),
        Ok(vec![0xFF; FRAME_UPLOAD_MAX_BYTES]),
    ]);
    let (upload, uploads) = ScriptedUpload::new();
    let mut r = Rig::new(tap, taps, upload, uploads);
    r.push(start("a"), 0);
    assert_eq!(
        r.worker.run_due(DELAY),
        Some(GrabRun::NotGrabbed {
            moment_id: "a".to_owned()
        })
    );
    r.push(start("b"), GAP);
    assert_eq!(
        r.worker.run_due(GAP + DELAY),
        Some(GrabRun::TooLarge {
            moment_id: "b".to_owned(),
            bytes: FRAME_UPLOAD_MAX_BYTES + 1
        })
    );
    assert_eq!(r.uploads.calls().len(), 0, "neither was sent");
    r.push(start("c"), 2 * GAP);
    assert_eq!(
        r.worker.run_due(2 * GAP + DELAY),
        Some(GrabRun::Posted {
            moment_id: "c".to_owned(),
            attempts: 1,
            bytes: FRAME_UPLOAD_MAX_BYTES
        })
    );
    assert_eq!(r.tap.grabs(), 3);
    assert_eq!(
        r.said("sponsor frame not grabbed: the programme gave no frame: no programme frame within 1000 ms"),
        1,
        "{:?}",
        r.lines()
    );
    assert_eq!(r.said("not grabbed"), 1, "said once: {:?}", r.lines());
}

/// The worker's own thread, on the manual clock: it sleeps until the plan's grab is due (a line
/// observed wakes it to look again), grabs and posts it, and a 410 ends it.
#[test]
fn the_worker_thread_runs_the_due_grab_and_a_410_ends_it() {
    let (clock, now) = manual();
    let grabs = FrameGrabs::on(clock);
    let airings = Airings::on().with_frames(grabs.clone());
    let (tap, taps) = FakeTap::new();
    let (upload, uploads) = ScriptedUpload::new();
    let upload = upload.answer(|n| {
        if n == 0 {
            Ok(())
        } else {
            Err(FrameUploadError::Gone)
        }
    });
    let log = Log::default();
    let worker = FrameWorker::new(
        grabs.clone(),
        Box::new(tap),
        Box::new(upload),
        Box::new(FakeLog(log.clone())),
    );
    let handle = spawn(worker).expect("spawn");
    assert!(handle.is_running());
    now.store(1_000, Ordering::SeqCst);
    assert!(airings.push(start("a")));
    // Not yet due: an unrelated line wakes it, and it waits on.
    now.store(1_000 + DELAY - 1, Ordering::SeqCst);
    airings.push(end("other", false));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(uploads.calls().len(), 0, "nothing before its instant");
    // Due: the next wake runs it.
    now.store(1_000 + DELAY, Ordering::SeqCst);
    airings.push(end("other", false));
    assert!(uploads.wait_for(1, WAIT), "the due grab was posted");
    assert_eq!(uploads.moments(), vec!["a".to_owned()]);
    // A second airing past the gap: answered 410, which stops the plan and the thread.
    now.store(1_000 + DELAY + GAP, Ordering::SeqCst);
    airings.push(start("b"));
    now.store(1_000 + 2 * DELAY + GAP, Ordering::SeqCst);
    airings.push(end("other", false));
    assert!(uploads.wait_for(2, WAIT), "the second grab was posted");
    assert!(grabs.is_stopped());
    now.store(1_000 + 10 * GAP, Ordering::SeqCst);
    airings.push(start("c"));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        (taps.grabs(), uploads.calls().len()),
        (2, 2),
        "nothing after the 410"
    );
    drop(handle);
}

/// Dropping the handle ends the thread: a grab that comes due after it is never taken.
#[test]
fn dropping_the_handle_ends_the_worker() {
    let (clock, now) = manual();
    let grabs = FrameGrabs::on(clock);
    let airings = Airings::on().with_frames(grabs.clone());
    let (tap, taps) = FakeTap::new();
    let (upload, uploads) = ScriptedUpload::new();
    let worker = FrameWorker::new(
        grabs.clone(),
        Box::new(tap),
        Box::new(upload),
        Box::new(FakeLog(Log::default())),
    );
    let handle = spawn(worker).expect("spawn");
    airings.push(start("a"));
    drop(handle);
    std::thread::sleep(Duration::from_millis(50));
    now.store(DELAY, Ordering::SeqCst);
    airings.push(end("other", false));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!((taps.grabs(), uploads.calls().len()), (0, 0));
    assert_eq!(
        grabs.next_due(),
        Some(DELAY),
        "the grab still waits: nobody took it"
    );
}
