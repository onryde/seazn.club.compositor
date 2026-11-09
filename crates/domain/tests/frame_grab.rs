//! R4 task SC2 (spec 2026-10-27 §3.6.3): the frame grab's plan and its upload policy, pure. Every
//! instant and bound comes from `domain::config` (pinned there to the spec's "1.5 s after a start line",
//! "≤ 1 per 20 s"), and every verdict of the upload table from the SC2 dispatch's ruling ("a 4xx means
//! give up on that frame; a 5xx or transport error may retry at most once; a 410 means the session is
//! gone, so stop"), never from the code under test.
use domain::config::{FRAME_GRAB_DELAY_MS, FRAME_GRAB_MIN_GAP_MS, FRAME_UPLOAD_ATTEMPTS};
use domain::frame_grab::{
    after_upload, is_path_segment, AfterUpload, FrameGrabPlan, StartVerdict, UploadOutcome,
};
use domain::time::Ms;

const DELAY: Ms = FRAME_GRAB_DELAY_MS;
const GAP: Ms = FRAME_GRAB_MIN_GAP_MS;

/// One start, due `DELAY` after it: nothing is due a millisecond before, it is taken at its instant,
/// and only once.
#[test]
fn a_start_is_due_1500_ms_later_and_is_taken_once() {
    let mut p = FrameGrabPlan::default();
    assert_eq!(p.next_due(), None, "an empty plan has nothing due");
    assert_eq!(p.take_due(1_000_000), None, "and takes nothing");
    let at = 7_000;
    assert_eq!(
        p.on_start("1:sponsor.set_break", at),
        StartVerdict::Scheduled { due_at: at + 1_500 }
    );
    assert_eq!(p.next_due(), Some(at + DELAY));
    assert_eq!(p.take_due(at + DELAY - 1), None, "a millisecond early");
    assert_eq!(p.take_due(at + DELAY).as_deref(), Some("1:sponsor.set_break"));
    assert_eq!(p.take_due(at + DELAY), None, "a second take of the same instant");
    assert_eq!(p.next_due(), None);
    assert_eq!(p.take_due(at + 10 * GAP), None, "nothing later either");
}

/// The 20 s gap, across all airings, at its edge: a second start whose grab would come 1 ms inside it is
/// refused (and says when the gap ends), one whose grab lands on it is taken. Each case is a fresh plan
/// whose first grab was taken on time.
#[test]
fn a_second_start_inside_20_s_is_refused_and_one_at_20_s_is_taken() {
    let first_taken = DELAY;
    let cases: [(Ms, bool); 6] = [
        (1, false),
        (DELAY, false),
        (GAP / 2, false),
        (GAP - 1, false),
        (GAP, true),
        (GAP + 1, true),
    ];
    let mut checked = 0;
    for (second_at, taken) in cases {
        let mut p = FrameGrabPlan::default();
        assert!(matches!(p.on_start("a", 0), StartVerdict::Scheduled { .. }));
        assert_eq!(p.take_due(first_taken).as_deref(), Some("a"));
        let v = p.on_start("b", second_at);
        if taken {
            assert_eq!(
                v,
                StartVerdict::Scheduled {
                    due_at: second_at + DELAY
                },
                "{second_at}"
            );
            assert_eq!(p.take_due(second_at + DELAY).as_deref(), Some("b"), "{second_at}");
        } else {
            assert_eq!(
                v,
                StartVerdict::TooSoon {
                    gap_ends_at: first_taken + GAP
                },
                "{second_at}"
            );
            assert_eq!(p.next_due(), None, "{second_at}: nothing waits");
            assert_eq!(p.take_due(second_at + 10 * GAP), None, "{second_at}");
        }
        checked += 1;
    }
    eprintln!("gap edges checked: {checked}");
    assert_eq!(checked, cases.len());
}

/// The gap runs from the instant a grab was TAKEN: a grab taken late (the worker woke late) pushes the
/// next one back with it, so two grabs are never closer than the gap.
#[test]
fn the_gap_runs_from_the_grab_as_taken_not_as_due() {
    let mut p = FrameGrabPlan::default();
    p.on_start("a", 0);
    let late = DELAY + 3_500;
    assert_eq!(p.take_due(late).as_deref(), Some("a"));
    assert_eq!(
        p.on_start("b", late + GAP - DELAY - 1),
        StartVerdict::TooSoon {
            gap_ends_at: late + GAP
        }
    );
    assert_eq!(
        p.on_start("c", late + GAP - DELAY),
        StartVerdict::Scheduled { due_at: late + GAP }
    );
}

/// One grab waits at a time (the plan holds one: AGENTS.md §4): a start while another waits is refused,
/// and the waiting one is the one taken.
#[test]
fn a_start_while_another_waits_is_refused() {
    let mut p = FrameGrabPlan::default();
    p.on_start("a", 0);
    assert_eq!(p.on_start("b", 500), StartVerdict::AnotherWaits);
    assert_eq!(
        p.on_start("a", 600),
        StartVerdict::AnotherWaits,
        "the same moment again"
    );
    assert_eq!(p.next_due(), Some(DELAY), "the first one's instant");
    assert_eq!(p.take_due(DELAY + 500).as_deref(), Some("a"));
    assert_eq!(p.take_due(DELAY + 600), None);
}

/// The waiting moment's end before its grab (the page cut the card short, or its build was retired or
/// crashed, whose ends SC1 writes) cancels the grab: the card is no longer on screen. A cancelled grab
/// was never taken, so it leaves the gap free: the next start is scheduled at once.
#[test]
fn the_waiting_moments_end_cancels_its_grab_and_leaves_the_gap_free() {
    let mut p = FrameGrabPlan::default();
    p.on_start("a", 0);
    assert!(p.on_end("a"), "it waited");
    assert_eq!(p.next_due(), None);
    assert_eq!(p.take_due(DELAY), None, "nothing at its instant");
    assert_eq!(
        p.on_start("b", 1_200),
        StartVerdict::Scheduled {
            due_at: 1_200 + DELAY
        }
    );
    assert_eq!(p.take_due(1_200 + DELAY).as_deref(), Some("b"));
}

/// An end of a moment that is not waiting changes nothing: another moment's, or the waiting one's after
/// its grab was taken, or one on an empty plan.
#[test]
fn an_end_of_a_moment_that_does_not_wait_changes_nothing() {
    let mut p = FrameGrabPlan::default();
    assert!(!p.on_end("a"), "an empty plan");
    p.on_start("a", 0);
    assert!(!p.on_end("b"), "another moment");
    assert_eq!(p.next_due(), Some(DELAY));
    assert_eq!(p.take_due(DELAY).as_deref(), Some("a"));
    assert!(!p.on_end("a"), "after its grab");
    assert_eq!(
        p.on_start("c", DELAY + 1),
        StartVerdict::TooSoon {
            gap_ends_at: DELAY + GAP
        }
    );
}

/// A stopped plan (web answered 410: the session is gone) takes nothing, the grab waiting included, and
/// schedules nothing, for good.
#[test]
fn a_stopped_plan_takes_and_schedules_nothing() {
    let mut p = FrameGrabPlan::default();
    p.on_start("a", 0);
    p.stop();
    assert!(p.is_stopped());
    assert_eq!(p.next_due(), None);
    assert_eq!(p.take_due(DELAY), None);
    assert_eq!(p.on_start("b", 10 * GAP), StartVerdict::Stopped);
    assert_eq!(p.take_due(11 * GAP), None);
    p.stop();
    assert!(p.is_stopped(), "a second stop keeps it stopped");
}

/// A moment id that cannot be one path segment of `…/airings/{momentId}/frame` is never grabbed: an empty
/// one names no moment, and `.` and `..` are dot-segments, which every URL parser resolves away (WHATWG
/// URL, RFC 3986 §5.2.4), so the POST would reach another path. Every other id is a segment once
/// percent-encoded, the moment ids web's pages log among them (`12:sponsor.over_end`, `scene:…:…`).
#[test]
fn a_moment_id_that_cannot_be_a_path_segment_is_never_grabbed() {
    let refused = ["", ".", ".."];
    let taken = [
        "12:sponsor.over_end",
        "scene:4c1d2e3f-5a6b-4c7d-8e9f-0a1b2c3d4e5f:179344818",
        "...",
        ".a",
        "a/b",
        "%2e%2e",
    ];
    let mut checked = 0;
    for id in refused {
        assert!(!is_path_segment(id), "{id:?}");
        let mut p = FrameGrabPlan::default();
        assert_eq!(p.on_start(id, 0), StartVerdict::NotASegment, "{id:?}");
        assert_eq!(p.next_due(), None, "{id:?}");
        checked += 1;
    }
    for id in taken {
        assert!(is_path_segment(id), "{id:?}");
        let mut p = FrameGrabPlan::default();
        assert_eq!(
            p.on_start(id, 0),
            StartVerdict::Scheduled { due_at: DELAY },
            "{id:?}"
        );
        checked += 1;
    }
    eprintln!("moment ids checked: {checked}");
    assert_eq!(checked, refused.len() + taken.len());
}

/// The upload policy, every cell, from the dispatch's ruling: a 2xx is done; a 4xx gives the frame up at
/// once; a 410 stops every later upload; a 5xx or a transport error is retried once, then given up.
#[test]
fn the_upload_policy_matches_the_ruling_in_every_cell() {
    assert_eq!(FRAME_UPLOAD_ATTEMPTS, 2);
    let table: [(UploadOutcome, u32, AfterUpload); 8] = [
        (UploadOutcome::Stored, 1, AfterUpload::Done),
        (UploadOutcome::Stored, 2, AfterUpload::Done),
        (UploadOutcome::Refused, 1, AfterUpload::GiveUp),
        (UploadOutcome::Refused, 2, AfterUpload::GiveUp),
        (UploadOutcome::Gone, 1, AfterUpload::Stop),
        (UploadOutcome::Gone, 2, AfterUpload::Stop),
        (UploadOutcome::Failed, 1, AfterUpload::Retry),
        (UploadOutcome::Failed, 2, AfterUpload::GiveUp),
    ];
    let mut checked = 0;
    for (outcome, attempt, want) in table {
        assert_eq!(
            after_upload(outcome, attempt),
            want,
            "{outcome:?} on attempt {attempt}"
        );
        checked += 1;
    }
    // Past the last attempt nothing is ever retried, whatever comes back.
    for outcome in [
        UploadOutcome::Stored,
        UploadOutcome::Refused,
        UploadOutcome::Gone,
        UploadOutcome::Failed,
    ] {
        assert_ne!(
            after_upload(outcome, FRAME_UPLOAD_ATTEMPTS + 1),
            AfterUpload::Retry,
            "{outcome:?}"
        );
        checked += 1;
    }
    eprintln!("upload cells checked: {checked}");
    assert_eq!(checked, 12);
}

/// A sweep: starts on a 250 ms grid over 3 minutes, each step a start with a fixed pseudo-random draw
/// (some ended before their grab), the plan taken at every step. Every grab taken is exactly `DELAY`
/// after a start the plan scheduled, at most one waits at a time, and two grabs are never closer than
/// the gap. The draw is a fixed LCG, so the run is the same every time.
#[test]
fn over_a_sweep_no_two_grabs_are_closer_than_the_gap() {
    let mut p = FrameGrabPlan::default();
    let mut seed: u64 = 0x5eaf_00d5;
    let mut draw = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        seed >> 33
    };
    let mut scheduled: Vec<(String, Ms)> = Vec::new();
    let mut taken: Vec<(String, Ms)> = Vec::new();
    let (mut starts, mut ended) = (0, 0);
    for step in 0..(180_000 / 250) {
        let now: Ms = step * 250;
        if let Some(m) = p.take_due(now) {
            taken.push((m, now));
        }
        let roll = draw() % 10;
        if roll < 3 {
            let id = format!("m{step}");
            starts += 1;
            if let StartVerdict::Scheduled { due_at } = p.on_start(&id, now) {
                assert_eq!(due_at, now + DELAY);
                scheduled.push((id.clone(), now));
            }
        } else if roll == 3 {
            if let Some((id, _)) = scheduled.last() {
                if p.on_end(id) {
                    ended += 1;
                }
            }
        }
    }
    for (m, at) in &taken {
        let started = scheduled.iter().find(|(id, _)| id == m).map(|(_, s)| *s);
        assert_eq!(started.map(|s| s + DELAY), Some(*at), "{m} taken at {at}");
    }
    for w in taken.windows(2) {
        assert!(w[1].1 - w[0].1 >= GAP, "{:?} then {:?}", w[0], w[1]);
    }
    eprintln!(
        "sweep: {starts} starts, {} scheduled, {ended} ended before their grab, {} taken",
        scheduled.len(),
        taken.len()
    );
    assert!(
        taken.len() >= 5,
        "a sweep that took {} grabs proves nothing",
        taken.len()
    );
    assert!(ended >= 1, "no end ever cancelled a grab");
    assert!(starts > scheduled.len() + ended, "no start was ever refused");
}

/// The final review's SC2 m10: a start's refusals have one order, the SC2 dispatch's (a stopped plan,
/// then a moment id that cannot be a path segment, then another grab waiting, then the gap), and the
/// first that holds is the verdict. Swept over every combination the plan's API can reach. A stopped
/// plan has no waiting grab (`stop` drops it), so 12 of the 16. Each refusal leaves the plan as it
/// was; a start none refuses waits, due `DELAY` later. The expected verdict is read off this order,
/// written here, never off the plan's table.
#[test]
fn a_starts_refusals_apply_in_the_dispatchs_order_in_every_combination() {
    const NOW: Ms = 1_000_000;
    let mut checked = 0;
    for stopped in [false, true] {
        for bad_segment in [false, true] {
            for waits in [false, true] {
                for too_soon in [false, true] {
                    if stopped && waits {
                        continue;
                    }
                    let mut p = FrameGrabPlan::default();
                    if too_soon {
                        // A grab taken at NOW: its gap runs to NOW + GAP, past this start's due instant.
                        assert!(matches!(
                            p.on_start("taken", NOW - DELAY),
                            StartVerdict::Scheduled { .. }
                        ));
                        assert_eq!(p.take_due(NOW).as_deref(), Some("taken"));
                    }
                    if waits {
                        // Scheduled clear of the gap, so it waits whether or not the gap runs.
                        let at = NOW + GAP;
                        assert!(matches!(p.on_start("waits", at), StartVerdict::Scheduled { .. }));
                    }
                    if stopped {
                        p.stop();
                    }
                    let moment = if bad_segment { ".." } else { "12:sponsor.over_end" };
                    let want = if stopped {
                        StartVerdict::Stopped
                    } else if bad_segment {
                        StartVerdict::NotASegment
                    } else if waits {
                        StartVerdict::AnotherWaits
                    } else if too_soon {
                        StartVerdict::TooSoon {
                            gap_ends_at: NOW + GAP,
                        }
                    } else {
                        StartVerdict::Scheduled { due_at: NOW + DELAY }
                    };
                    let before = p.clone();
                    let got = p.on_start(moment, NOW);
                    let case = (stopped, bad_segment, waits, too_soon);
                    assert_eq!(got, want, "{case:?}");
                    if matches!(want, StartVerdict::Scheduled { .. }) {
                        assert_eq!(p.next_due(), Some(NOW + DELAY), "{case:?}");
                    } else {
                        assert_eq!(p, before, "{case:?}: a refused start changed the plan");
                    }
                    checked += 1;
                }
            }
        }
    }
    eprintln!("start combinations checked: {checked}");
    assert_eq!(checked, 12);
}
