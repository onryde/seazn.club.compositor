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
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use domain::airings::{AiringBatch, AiringBuffer, AiringLine};

fn locked(m: &Mutex<AiringBuffer>) -> MutexGuard<'_, AiringBuffer> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A handle on the one buffer: clones share it. `Default` is off.
#[derive(Clone, Default)]
pub struct Airings(Option<Arc<Mutex<AiringBuffer>>>);

// Counts only: a line's ids are not printed.
impl fmt::Debug for Airings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            None => f.write_str("Airings(off)"),
            Some(b) => {
                let b = locked(b);
                write!(f, "Airings(on, held {}, dropped {})", b.len(), b.dropped())
            }
        }
    }
}

impl Airings {
    /// Holds nothing and sends nothing (the default).
    pub fn off() -> Self {
        Self(None)
    }

    /// A new, empty buffer.
    pub fn on() -> Self {
        Self(Some(Arc::default()))
    }

    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    /// Holds `line` for the next beat: true, or false when off (nothing is held).
    pub fn push(&self, line: AiringLine) -> bool {
        let Some(b) = &self.0 else {
            return false;
        };
        locked(b).push(line);
        true
    }

    /// The next beat's lines: every one held, oldest first (empty when off).
    pub fn for_beat(&self) -> AiringBatch {
        self.0
            .as_ref()
            .map_or_else(AiringBatch::default, |b| locked(b).take_for_beat())
    }

    /// The beat that carried `batch` was answered with a 2xx: its lines leave.
    pub fn ack_2xx(&self, batch: &AiringBatch) {
        if let Some(b) = &self.0 {
            locked(b).ack_2xx(batch);
        }
    }

    /// Web refused the beat that carried `batch` (`HeartbeatError::Refused`): its lines leave, counted
    /// dropped. The number that left (0 when off).
    pub fn drop_refused(&self, batch: &AiringBatch) -> usize {
        self.0.as_ref().map_or(0, |b| locked(b).drop_refused(batch))
    }

    /// Lines held now (0 when off).
    pub fn held(&self) -> usize {
        self.0.as_ref().map_or(0, |b| locked(b).len())
    }
}
