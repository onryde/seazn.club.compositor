//! Proof of play, the frame (R4 sponsors on stream, task SC2; spec 2026-10-27 §3.6.3): one programme
//! frame per sponsor airing, grabbed `FRAME_GRAB_DELAY_MS` after its start, at most one every
//! `FRAME_GRAB_MIN_GAP_MS` across all airings, and uploaded to web with one retry at most. Pure: the
//! app's frame worker hands the plan each sponsor line held for the beat (SC1: only a line the
//! heartbeat forwards, the current page's) and its own clock, and asks it what is due and what to do
//! after each upload.
//!
//! - **One grab waits at a time** (the plan holds one: AGENTS.md §4). A start while one waits, or whose
//!   grab would come inside the gap after the last grab taken, gets none.
//! - **The waiting moment's end cancels its grab**: the card is no longer on screen, so its frame would
//!   prove nothing. SC1 writes that end when the page ends the card, and when the page's build is
//!   retired or crashes (cut short), so a retirement or a crash inside the delay grabs nothing. A
//!   cancelled grab was never taken, so the gap is not spent.
//! - **The gap runs from a grab as taken** (its instant on the worker's clock), so a late take moves
//!   the next one back with it.
//! - **Stopped is for good**: web answered 410, the session is gone.
use crate::config::{FRAME_GRAB_DELAY_MS, FRAME_GRAB_MIN_GAP_MS, FRAME_UPLOAD_ATTEMPTS};
use crate::time::Ms;

/// What the plan made of a sponsor start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartVerdict {
    /// Its grab is due at `due_at`.
    Scheduled { due_at: Ms },
    /// Another start's grab waits: this one gets none.
    AnotherWaits,
    /// Its grab would come inside the gap after the last grab taken, which ends at `gap_ends_at`.
    TooSoon { gap_ends_at: Ms },
    /// Its moment id cannot be one path segment of the upload's URL (`is_path_segment`).
    NotASegment,
    /// The plan is stopped: the session is gone.
    Stopped,
}

/// True when `moment_id`, percent-encoded, is one path segment of `…/airings/{momentId}/frame`: not
/// empty, and not the dot-segments `.` and `..`, which every URL parser resolves away (WHATWG URL;
/// RFC 3986 §5.2.4), sending the POST to another path.
pub fn is_path_segment(moment_id: &str) -> bool {
    !moment_id.is_empty() && moment_id != "." && moment_id != ".."
}

/// The grab that waits, and the gap's anchor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameGrabPlan {
    /// The waiting grab's moment id and its due instant.
    waiting: Option<(String, Ms)>,
    /// When the last grab was taken.
    last_taken_at: Option<Ms>,
    stopped: bool,
}

/// One refusal of a start: the plan, the start's moment id and its grab's due instant in, the
/// verdict out when it refuses.
type StartRule = fn(&FrameGrabPlan, &str, Ms) -> Option<StartVerdict>;

/// A start's refusals, asked in this order (the SC2 dispatch). The first that refuses gives the
/// verdict, and a start that none refuses is scheduled. A table (AGENTS.md §4; the final review's SC2
/// m10), so the order lives in one place, which `tests/frame_grab.rs` sweeps over every reachable
/// combination.
const START_REFUSALS: [StartRule; 4] = [
    |plan, _, _| plan.stopped.then_some(StartVerdict::Stopped),
    |_, moment_id, _| (!is_path_segment(moment_id)).then_some(StartVerdict::NotASegment),
    |plan, _, _| plan.waiting.is_some().then_some(StartVerdict::AnotherWaits),
    |plan, _, due_at| {
        let gap_ends_at = plan.last_taken_at?.saturating_add(FRAME_GRAB_MIN_GAP_MS);
        (due_at < gap_ends_at).then_some(StartVerdict::TooSoon { gap_ends_at })
    },
];

impl FrameGrabPlan {
    /// A sponsor start held for the beat at `now`: refused by the first of `START_REFUSALS` that
    /// refuses it, else its grab waits, due `FRAME_GRAB_DELAY_MS` later. A refused start changes
    /// nothing.
    pub fn on_start(&mut self, moment_id: &str, now: Ms) -> StartVerdict {
        let due_at = now.saturating_add(FRAME_GRAB_DELAY_MS);
        if let Some(refused) = START_REFUSALS
            .iter()
            .find_map(|refuses| refuses(self, moment_id, due_at))
        {
            return refused;
        }
        self.waiting = Some((moment_id.to_owned(), due_at));
        StartVerdict::Scheduled { due_at }
    }

    /// A sponsor end held for the beat: when its moment's grab waits, that grab is cancelled (true).
    pub fn on_end(&mut self, moment_id: &str) -> bool {
        let cancels = self.waiting.as_ref().is_some_and(|(m, _)| m == moment_id);
        if cancels {
            self.waiting = None;
        }
        cancels
    }

    /// When the waiting grab is due, if one waits (none does once stopped: `stop` drops it, and
    /// `on_start` plans no other).
    pub fn next_due(&self) -> Option<Ms> {
        self.waiting.as_ref().map(|(_, due)| *due)
    }

    /// The waiting grab's moment id once it is due at `now`, taken: the gap now runs from `now`.
    pub fn take_due(&mut self, now: Ms) -> Option<String> {
        if self.next_due().is_none_or(|due| now < due) {
            return None;
        }
        let (moment, _) = self.waiting.take()?;
        self.last_taken_at = Some(now);
        Some(moment)
    }

    /// Web answered 410: nothing more is grabbed, the waiting grab included.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.waiting = None;
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }
}

/// How one upload attempt ended, as the policy reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    /// A 2xx (S9 answers 204): web holds the frame.
    Stored,
    /// A 4xx other than 410: web refused this frame (another session's token, a body it will not take).
    Refused,
    /// A 410: the session is gone.
    Gone,
    /// A 5xx, or no answer (a transport error, a timeout).
    Failed,
}

/// What comes after an upload attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterUpload {
    /// The frame is web's.
    Done,
    /// Try once more.
    Retry,
    /// Drop the frame.
    GiveUp,
    /// Drop it and upload nothing more: the session is gone.
    Stop,
}

/// The policy (the SC2 dispatch): a 4xx gives the frame up at once, a 410 stops, and a 5xx or a
/// transport error is retried while attempts remain (`FRAME_UPLOAD_ATTEMPTS`). `attempt` counts from 1.
pub fn after_upload(outcome: UploadOutcome, attempt: u32) -> AfterUpload {
    match outcome {
        UploadOutcome::Stored => AfterUpload::Done,
        UploadOutcome::Refused => AfterUpload::GiveUp,
        UploadOutcome::Gone => AfterUpload::Stop,
        UploadOutcome::Failed if attempt < FRAME_UPLOAD_ATTEMPTS => AfterUpload::Retry,
        UploadOutcome::Failed => AfterUpload::GiveUp,
    }
}
