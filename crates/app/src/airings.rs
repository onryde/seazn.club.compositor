//! The sponsor airings' shared buffer (R4 sponsors on stream, task SC1; spec 2026-10-27 §3.6.2). The
//! graphics adapter pushes each sponsor line the current page logs (`AiringLine`, read and bounded
//! there); the control worker takes a snapshot for every beat it posts, the final beat's included, and
//! acknowledges it when that beat's call returns a reply (`control_worker`'s `post_beat`: C07's 200
//! with a heartbeat reply, the contract's 2xx). Every other outcome keeps the lines for the next beat.
//! One lock, held for a push, a snapshot or an acknowledgement only, never across a call; a poisoned
//! lock is recovered, so a panic elsewhere never stops a beat.
//!
//! **Off unless the binary turns it on** (`Airings::on`, from the Machine's `RELAY_AIRINGS`). Web's
//! heartbeat is strict: a beat carrying `airings` before web declares the key is a 400, and the whole
//! beat is lost. Off, a push holds nothing and every snapshot is empty, so no beat carries the keys.
//!
//! **Each line held is the frame grabs' cue** (R4 task SC2, `with_frames`): a push hands the line to
//! the frame plan once it is held, so only a line the heartbeat forwards can plan or cancel a grab.
//! Off, nothing is held, so nothing is planned either.
//!
//! **Web refusing a beat that carried only `airingsDropped` turns it off for the process** (PR C final
//! review, I1; `AiringBuffer::drop_refused`): a web that does not know the keys would refuse every beat
//! after it. From then on a push holds and cues nothing, and no beat carries either key.
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use domain::airings::{AiringBatch, AiringBuffer, AiringLine};

use crate::frame_grab::FrameGrabs;

fn locked(m: &Mutex<AiringBuffer>) -> MutexGuard<'_, AiringBuffer> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A handle on the one buffer: clones share it, and the frame plan it cues. `Default` is off.
#[derive(Clone, Default)]
pub struct Airings {
    buffer: Option<Arc<Mutex<AiringBuffer>>>,
    frames: FrameGrabs,
}

// Counts only: a line's ids are not printed.
impl fmt::Debug for Airings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.buffer {
            None => f.write_str("Airings(off)"),
            Some(b) => {
                let b = locked(b);
                write!(
                    f,
                    "Airings(on, held {}, dropped {}, {:?})",
                    b.len(),
                    b.dropped(),
                    self.frames
                )
            }
        }
    }
}

impl Airings {
    /// Holds nothing and sends nothing (the default).
    pub fn off() -> Self {
        Self::default()
    }

    /// A new, empty buffer, cueing no frame grab.
    pub fn on() -> Self {
        Self {
            buffer: Some(Arc::default()),
            frames: FrameGrabs::off(),
        }
    }

    /// The same buffer, each line it holds from now on cueing `frames` (R4 task SC2).
    pub fn with_frames(self, frames: FrameGrabs) -> Self {
        Self { frames, ..self }
    }

    /// The frame plan the held lines cue (off unless `with_frames` set one).
    pub fn frames(&self) -> &FrameGrabs {
        &self.frames
    }

    pub fn is_on(&self) -> bool {
        self.buffer.is_some()
    }

    /// Holds `line` for the next beat and cues the frame plan with it: true, or false when off or shut
    /// (nothing is held, and nothing cued). The plan is cued outside the buffer's lock.
    pub fn push(&self, line: AiringLine) -> bool {
        let Some(b) = &self.buffer else {
            return false;
        };
        if locked(b).is_shut() {
            return false;
        }
        self.frames.observe(&line);
        locked(b).push(line)
    }

    /// Counts `n` sponsor lines lost before they reached the buffer (the graphics adapter's note
    /// queue, full: the final review's m5) in `airingsDropped`. Nothing when off or shut.
    pub fn count_lost(&self, n: u32) {
        if let Some(b) = &self.buffer {
            locked(b).count_lost(n);
        }
    }

    /// The next beat's lines: every one held, oldest first (empty when off).
    pub fn for_beat(&self) -> AiringBatch {
        self.buffer
            .as_ref()
            .map_or_else(AiringBatch::default, |b| locked(b).take_for_beat())
    }

    /// The beat that carried `batch` was answered with a 2xx: its lines leave.
    pub fn ack_2xx(&self, batch: &AiringBatch) {
        if let Some(b) = &self.buffer {
            locked(b).ack_2xx(batch);
        }
    }

    /// Web refused the beat that carried `batch` (`HeartbeatError::Refused`): its lines leave, counted
    /// dropped. A batch that carried only the count shuts the buffer instead (the final review's I1).
    /// The number that left (0 when off, or on the shut).
    pub fn drop_refused(&self, batch: &AiringBatch) -> usize {
        self.buffer.as_ref().map_or(0, |b| locked(b).drop_refused(batch))
    }

    /// Lines held now (0 when off).
    pub fn held(&self) -> usize {
        self.buffer.as_ref().map_or(0, |b| locked(b).len())
    }
}
