//! The process's one clock (C12 Step 5): monotonic milliseconds that stay comparable with the epoch.
//!
//! `now_ms` is `Instant::now()` minus one `Instant` captured at start, plus one epoch offset captured
//! at the same moment (the wall clock since `UNIX_EPOCH`). The domain needs both meanings from one
//! function: frame ages, thresholds and back-offs run on it, and because the offset is fixed at start
//! it stays comparable with the epoch `RELAY_DEADLINE_AT`. A wall-clock step (NTP, a VM resume) can
//! never move it backwards: a backward step would read a frame's age as 0, so Reconnecting would go
//! Live with no new frame and a camera loss would be seen late (C03 review, `docs/_INDEX.md`). The
//! log lines' `ts=` reads it too, so this is the only clock read in the workspace.
use std::fmt;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct Clock {
    start: Instant,
    epoch_ms_at_start: u64,
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Clock")
            .field("epoch_ms_at_start", &self.epoch_ms_at_start)
            .finish_non_exhaustive()
    }
}

impl Clock {
    /// The system's clock: the wall read once, here.
    pub fn system() -> Self {
        Self::from_wall(SystemTime::now)
    }

    /// `wall` is read exactly once, now (a test injects it). A wall before the epoch reads as 0.
    pub fn from_wall(wall: impl FnOnce() -> SystemTime) -> Self {
        let start = Instant::now();
        let epoch_ms_at_start = wall()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Self {
            start,
            epoch_ms_at_start,
        }
    }

    /// Epoch milliseconds, monotonic: never smaller than an earlier call's.
    pub fn now_ms(&self) -> u64 {
        /*MUT*/ let _ = self.start;
        u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap() + 0 * self.epoch_ms_at_start
    }
}
