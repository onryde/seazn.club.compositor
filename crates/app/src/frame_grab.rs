//! The sponsor airings' frame grabs (R4 sponsors on stream, task SC2; spec 2026-10-27 §3.6.3): one
//! programme frame, grabbed `FRAME_GRAB_DELAY_MS` after a sponsor start, at most one every
//! `FRAME_GRAB_MIN_GAP_MS`, posted to web's frame endpoint (S9) as the airing's photo.
//!
//! - **The cue is a line held for the beat.** `Airings::push` hands each sponsor line it holds to
//!   `FrameGrabs::observe`, so a grab is planned only for a start the heartbeat forwards (SC1: the
//!   current page's, never a refused line or a retired or crashed build's), and the waiting moment's
//!   end, the one SC1 writes when its build is retired or crashes among them, cancels it
//!   (`domain::frame_grab`).
//! - **Off unless the binary turns it on**, with the airings (`RELAY_AIRINGS`): off, nothing is
//!   planned, no worker runs, and the binary asks the programme for no tap.
//! - **The worker owns the slow parts**: one thread (`sponsor-frames`) waits for the grab that is due,
//!   then takes the frame (`FrameTap`) and posts it (`FrameUpload`), with no lock held. The loop, the
//!   control worker and the graphics adapter never wait on it: they share one plan behind one lock,
//!   held for a line, a look at what is due or a take, never across a call.
//! - **A frame that fails is dropped, and said once.** A frame the tap could not give (or one larger
//!   than web takes) is one line, once a process; an upload web refused, or that failed twice, is
//!   another, once. A 410 stops the plan for good, with its own line.
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use domain::airings::AiringLine;
use domain::config::FRAME_UPLOAD_MAX_BYTES;
use domain::frame_grab::{after_upload, AfterUpload, FrameGrabPlan, StartVerdict, UploadOutcome};
use domain::time::Ms;

use crate::ports::{FrameTap, FrameUpload, FrameUploadError, LogSink};

/// The plan's clock: monotonic milliseconds (the binary's process clock; a test's manual one). This
/// crate reads no clock of its own.
pub type FrameClock = Arc<dyn Fn() -> Ms + Send + Sync>;

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Shared {
    plan: Mutex<FrameGrabPlan>,
    /// Woken by a line observed and by the worker's handle going.
    wake: Condvar,
    clock: FrameClock,
    /// The worker's handle has gone: its thread ends.
    ended: AtomicBool,
}

/// A handle on the one plan: clones share it. `Default` is off.
#[derive(Clone, Default)]
pub struct FrameGrabs(Option<Arc<Shared>>);

impl fmt::Debug for FrameGrabs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            None => f.write_str("FrameGrabs(off)"),
            Some(s) => {
                let p = locked(&s.plan);
                write!(
                    f,
                    "FrameGrabs(on, due {:?}, stopped {})",
                    p.next_due(),
                    p.is_stopped()
                )
            }
        }
    }
}

/// What `observe` made of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    Start(StartVerdict),
    /// An end: true when it cancelled its moment's waiting grab.
    End(bool),
}

impl FrameGrabs {
    /// Plans nothing (the default).
    pub fn off() -> Self {
        Self(None)
    }

    /// An empty plan on `clock`.
    pub fn on(clock: FrameClock) -> Self {
        Self(Some(Arc::new(Shared {
            plan: Mutex::new(FrameGrabPlan::default()),
            wake: Condvar::new(),
            clock,
            ended: AtomicBool::new(false),
        })))
    }

    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    /// A sponsor line held for the beat, now (`Airings::push`): a start may plan a grab, an end may
    /// cancel its moment's. `None` when off.
    pub fn observe(&self, line: &AiringLine) -> Option<Observed> {
        let s = self.0.as_ref()?;
        let now = (s.clock)();
        let seen = {
            let mut plan = locked(&s.plan);
            match line {
                AiringLine::Start(start) => Observed::Start(plan.on_start(start.moment_id(), now)),
                AiringLine::End(end) => Observed::End(plan.on_end(end.moment_id())),
            }
        };
        s.wake.notify_all();
        Some(seen)
    }

    /// When the waiting grab is due (`None`: none waits, or off).
    pub fn next_due(&self) -> Option<Ms> {
        self.0.as_ref().and_then(|s| locked(&s.plan).next_due())
    }

    /// The waiting grab's moment id once it is due at `now`, taken.
    pub fn take_due(&self, now: Ms) -> Option<String> {
        self.0.as_ref().and_then(|s| locked(&s.plan).take_due(now))
    }

    /// Web answered 410: nothing more is grabbed.
    pub fn stop(&self) {
        if let Some(s) = &self.0 {
            locked(&s.plan).stop();
            s.wake.notify_all();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.0.as_ref().is_some_and(|s| locked(&s.plan).is_stopped())
    }
}

/// How a due grab went (`FrameWorker::run_due`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrabRun {
    /// Web holds the frame, after `attempts` uploads.
    Posted {
        moment_id: String,
        attempts: u32,
        bytes: usize,
    },
    /// The tap gave no frame.
    NotGrabbed { moment_id: String },
    /// The JPEG is larger than web takes (`FRAME_UPLOAD_MAX_BYTES`): never sent.
    TooLarge { moment_id: String, bytes: usize },
    /// Web refused it, or every attempt failed: dropped.
    GaveUp { moment_id: String, attempts: u32 },
    /// Web answered 410: the plan is stopped.
    SessionGone { moment_id: String },
}

/// The grab and upload, one due grab at a time. `run_due` is the whole of it on a given instant; the
/// thread `spawn` starts calls it whenever the plan says a grab is due.
pub struct FrameWorker {
    grabs: FrameGrabs,
    tap: Box<dyn FrameTap>,
    upload: Box<dyn FrameUpload>,
    log: Box<dyn LogSink>,
    grab_failure_said: bool,
    upload_failure_said: bool,
}

impl fmt::Debug for FrameWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameWorker")
            .field("grabs", &self.grabs)
            .finish_non_exhaustive()
    }
}

impl FrameWorker {
    pub fn new(
        grabs: FrameGrabs,
        tap: Box<dyn FrameTap>,
        upload: Box<dyn FrameUpload>,
        log: Box<dyn LogSink>,
    ) -> Self {
        Self {
            grabs,
            tap,
            upload,
            log,
            grab_failure_said: false,
            upload_failure_said: false,
        }
    }

    /// Takes the grab due at `now`, if one is, and runs it: the frame, then its upload under the
    /// policy (`domain::frame_grab::after_upload`). `None` when nothing is due.
    pub fn run_due(&mut self, now: Ms) -> Option<GrabRun> {
        let moment_id = self.grabs.take_due(now)?;
        let jpeg = match self.tap.grab() {
            Ok(jpeg) => jpeg,
            Err(why) => {
                self.say_grab_failure(&format!("the programme gave no frame: {why}"));
                return Some(GrabRun::NotGrabbed { moment_id });
            }
        };
        if jpeg.len() > FRAME_UPLOAD_MAX_BYTES {
            let bytes = jpeg.len();
            self.say_grab_failure(&format!(
                "the JPEG is {bytes} bytes, more than web takes ({FRAME_UPLOAD_MAX_BYTES})"
            ));
            return Some(GrabRun::TooLarge { moment_id, bytes });
        }
        let mut attempts = 0;
        loop {
            attempts += 1;
            let r = self.upload.post_frame(&moment_id, &jpeg);
            let (outcome, why) = match &r {
                Ok(()) => (UploadOutcome::Stored, String::new()),
                Err(FrameUploadError::Gone) => (UploadOutcome::Gone, "status 410".to_owned()),
                Err(FrameUploadError::Refused(code)) => (UploadOutcome::Refused, format!("status {code}")),
                Err(FrameUploadError::Failed(text)) => (UploadOutcome::Failed, text.clone()),
                Err(FrameUploadError::Unaddressable) => (
                    UploadOutcome::Refused,
                    "the moment id is not a path segment".to_owned(),
                ),
            };
            match after_upload(outcome, attempts) {
                AfterUpload::Done => {
                    self.log.info(&format!(
                        "sponsor frame posted moment={moment_id} bytes={} attempts={attempts}",
                        jpeg.len()
                    ));
                    return Some(GrabRun::Posted {
                        moment_id,
                        attempts,
                        bytes: jpeg.len(),
                    });
                }
                AfterUpload::Retry => {}
                AfterUpload::GiveUp => {
                    if !std::mem::replace(&mut self.upload_failure_said, true) {
                        self.log.warn(&format!(
                            "sponsor frame upload failed ({why}) after {attempts} attempt(s): the frame is \
                             dropped (said once)"
                        ));
                    }
                    return Some(GrabRun::GaveUp { moment_id, attempts });
                }
                AfterUpload::Stop => {
                    self.grabs.stop();
                    self.log
                        .info("sponsor frame upload: web answered 410 (session gone); no more frames");
                    return Some(GrabRun::SessionGone { moment_id });
                }
            }
        }
    }

    fn say_grab_failure(&mut self, why: &str) {
        if !std::mem::replace(&mut self.grab_failure_said, true) {
            self.log.warn(&format!(
                "sponsor frame not grabbed: {why}; the frame is dropped (said once)"
            ));
        }
    }

    /// The thread's loop: waits for the grab that is due and runs it, until the handle goes or the
    /// plan stops. The plan's lock is held only to look and to wait, never across a grab.
    fn run_until_ended(mut self, s: &Shared) {
        loop {
            {
                let mut plan = locked(&s.plan);
                loop {
                    if s.ended.load(Ordering::SeqCst) || plan.is_stopped() {
                        return;
                    }
                    let now = (s.clock)();
                    plan = match plan.next_due() {
                        Some(due) if due <= now => break,
                        Some(due) => {
                            s.wake
                                .wait_timeout(plan, Duration::from_millis(due - now))
                                .unwrap_or_else(PoisonError::into_inner)
                                .0
                        }
                        None => s.wake.wait(plan).unwrap_or_else(PoisonError::into_inner),
                    };
                }
            }
            self.run_due((s.clock)());
        }
    }
}

/// The running worker. Dropping it ends the thread (at once when it waits; after its grab and upload
/// when one is in flight). It never joins.
pub struct FrameWorkerHandle(Option<Arc<Shared>>);

impl fmt::Debug for FrameWorkerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "FrameWorkerHandle(running)"
        } else {
            "FrameWorkerHandle(off)"
        })
    }
}

impl FrameWorkerHandle {
    /// True when a thread was started (the plan is on).
    pub fn is_running(&self) -> bool {
        self.0.is_some()
    }
}

impl Drop for FrameWorkerHandle {
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            s.ended.store(true, Ordering::SeqCst);
            s.wake.notify_all();
        }
    }
}

/// Starts `worker`'s thread, `sponsor-frames`. Off (its plan off), no thread is started.
pub fn spawn(worker: FrameWorker) -> std::io::Result<FrameWorkerHandle> {
    let Some(shared) = worker.grabs.0.clone() else {
        return Ok(FrameWorkerHandle(None));
    };
    let s = Arc::clone(&shared);
    thread::Builder::new()
        .name("sponsor-frames".to_owned())
        .spawn(move || worker.run_until_ended(&s))?;
    Ok(FrameWorkerHandle(Some(shared)))
}
