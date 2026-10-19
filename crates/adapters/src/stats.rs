//! The heartbeat's media numbers (C09; C12 wires them into `ProcMetrics`, and R1's panel health chips
//! and C14/C15 read them). Each is a measurement, and `None` means "not measured yet", never 0
//! (AGENTS.md §4; plan, Global Constraints, "Heartbeat").
//!
//! | Field | Definition | `None` until |
//! |---|---|---|
//! | `fps` | frames `push()`ed in the last complete 1 s window | 1 s of pushes |
//! | `bitrate_kbps` | bytes SENT by the current sink in the last complete 1 s window × 8 / 1000 | one complete window of the current sink's polls |
//! | `egress_bytes` | bytes sent, cumulative across every sink generation | a poll that reads a total above 0 |
//! | `encoder_speed` | encoded video PTS advance in the last complete 1 s window ÷ the window's wall time | 1 s of encoder output |
//!
//! "Bytes sent" is rtmp2sink's `out-bytes-total`, or the file's size for a `file://` target, never
//! acknowledged bytes: CF Input B sends no RTMP acknowledgements (F-005; `docs/_INDEX.md` Owed 13).
//!
//! **Windows.** A field's windows are 1 s long and start at its first measurement. Each window's value
//! is the change of a cumulative quantity (pushes, bytes, encoded PTS) from the last reading at or
//! before its start to the last reading at or before its end. A window with no reading changes
//! nothing, so a stopped encoder reads 0, a measured fact, not an absent one.
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// A window's length (the field definitions above: "the last complete 1 s window").
pub const STATS_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StatsSnapshot {
    pub fps: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub egress_bytes: Option<u64>,
    pub encoder_speed: Option<f64>,
}

#[derive(Default)]
pub struct Stats {
    inner: Mutex<Inner>,
    #[cfg(feature = "test-hooks")]
    forced: Mutex<Option<StatsSnapshot>>,
}

#[derive(Default)]
struct Inner {
    pushes: u64,
    fps: Windowed,
    /// The bytes every earlier sink sent (each sink counts from 0).
    egress_before_sink: u64,
    /// The current sink's latest total.
    sink_total: u64,
    egress_seen: bool,
    bitrate: Windowed,
    encoder: Windowed,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Stats {
    /// One frame pushed into the programme.
    pub fn on_push(&self) {
        self.on_push_at(Instant::now());
    }

    /// One encoded video frame left the encoder with this running-time PTS.
    pub fn on_encoded_video(&self, pts: Duration) {
        self.on_encoded_video_at(Instant::now(), pts);
    }

    /// A new sink starts counting from 0: the bitrate window restarts, the egress carries on.
    pub fn on_new_sink(&self) {
        let mut s = locked(&self.inner);
        s.egress_before_sink += s.sink_total;
        s.sink_total = 0;
        s.bitrate = Windowed::default();
    }

    /// A poll read the current sink's bytes-sent total.
    pub fn on_sink_total(&self, total_bytes: u64) {
        self.on_sink_total_at(Instant::now(), total_bytes);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        #[cfg(feature = "test-hooks")]
        if let Some(s) = locked(&self.forced).take() {
            return s;
        }
        self.snapshot_at(Instant::now())
    }

    /// Test hook for C12's join test only: the next `snapshot()` returns `s`. Behind the `adapters`
    /// feature `test-hooks`, which only dev-dependencies enable; release builds don't have it.
    #[cfg(feature = "test-hooks")]
    pub fn force_snapshot(&self, s: StatsSnapshot) {
        *locked(&self.forced) = Some(s);
    }

    pub(crate) fn on_push_at(&self, now: Instant) {
        let mut s = locked(&self.inner);
        s.pushes += 1;
        let pushes = s.pushes as f64;
        // The first push opens the first window and counts in it.
        s.fps.record(now, pushes, 0.0);
    }

    pub(crate) fn on_encoded_video_at(&self, now: Instant, pts: Duration) {
        let mut s = locked(&self.inner);
        let pts = pts.as_secs_f64();
        s.encoder.record(now, pts, pts);
    }

    pub(crate) fn on_sink_total_at(&self, now: Instant, total_bytes: u64) {
        let mut s = locked(&self.inner);
        // A sink's total never falls; a lower reading is a stale one and changes nothing.
        let total = total_bytes.max(s.sink_total);
        s.sink_total = total;
        let egress = s.egress_before_sink + total;
        if egress > 0 {
            s.egress_seen = true;
        }
        let at_first = egress as f64;
        s.bitrate.record(now, egress as f64, at_first);
    }

    pub(crate) fn snapshot_at(&self, now: Instant) -> StatsSnapshot {
        let s = locked(&self.inner);
        StatsSnapshot {
            fps: s.fps.last_complete(now),
            bitrate_kbps: s
                .bitrate
                .last_complete(now)
                .map(|bytes| (bytes * 8.0 / 1_000.0).round() as u32),
            egress_bytes: s.egress_seen.then_some(s.egress_before_sink + s.sink_total),
            encoder_speed: s
                .encoder
                .last_complete(now)
                .map(|advance| advance / STATS_WINDOW.as_secs_f64()),
        }
    }
}

impl fmt::Debug for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stats")
            .field("now", &self.snapshot_at(Instant::now()))
            .finish()
    }
}

/// A cumulative quantity read at instants, cut into 1 s windows from its first reading.
#[derive(Default)]
struct Windowed {
    origin: Option<Instant>,
    /// The window of the latest reading.
    index: u64,
    /// The quantity at the latest reading.
    value: f64,
    /// The quantity at the start of window `index` (the last reading before it).
    at_start: f64,
    /// The change over window `index − 1`, once one is complete.
    previous: Option<f64>,
}

impl Windowed {
    /// A reading of `value` at `now`. `before_first` is the quantity just before the first reading
    /// (0 pushes before the first push; the first PTS itself for the encoder).
    fn record(&mut self, now: Instant, value: f64, before_first: f64) {
        let Some(origin) = self.origin else {
            self.origin = Some(now);
            self.index = 0;
            self.at_start = before_first;
            self.value = value;
            return;
        };
        let k = window_of(origin, now);
        if k > self.index {
            self.previous = Some(if k == self.index + 1 {
                self.value - self.at_start
            } else {
                0.0 // at least one whole window passed with no reading
            });
            self.at_start = self.value;
            self.index = k;
        }
        self.value = value;
    }

    /// The change over the last window that ended at or before `now`.
    fn last_complete(&self, now: Instant) -> Option<f64> {
        let origin = self.origin?;
        let k = window_of(origin, now);
        if k == self.index {
            self.previous
        } else if k == self.index + 1 {
            Some(self.value - self.at_start)
        } else {
            Some(0.0)
        }
    }
}

fn window_of(origin: Instant, now: Instant) -> u64 {
    let since = now.saturating_duration_since(origin);
    (since.as_nanos() / STATS_WINDOW.as_nanos()) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_fresh_stats_measures_nothing() {
        let s = Stats::default();
        assert_eq!(s.snapshot_at(Instant::now()), StatsSnapshot::default());
    }

    // 30 pushes in the first second read as 30 only once that second is complete.
    #[test]
    fn fps_is_the_last_complete_window_and_none_before_one() {
        let s = Stats::default();
        let t0 = Instant::now();
        for i in 0..30u64 {
            s.on_push_at(t0 + ms(i * 33));
        }
        assert_eq!(s.snapshot_at(t0 + ms(990)).fps, None, "no window complete yet");
        assert_eq!(s.snapshot_at(t0 + ms(1_000)).fps, Some(30.0));
        // The second window holds 10 pushes; while it runs, the first is still the answer.
        for i in 0..10u64 {
            s.on_push_at(t0 + ms(1_000 + i * 33));
        }
        assert_eq!(s.snapshot_at(t0 + ms(1_500)).fps, Some(30.0));
        assert_eq!(s.snapshot_at(t0 + ms(2_000)).fps, Some(10.0));
        // A whole second with no push is a measured 0.
        assert_eq!(s.snapshot_at(t0 + ms(3_100)).fps, Some(0.0));
    }

    #[test]
    fn egress_is_cumulative_across_sinks_and_the_bitrate_restarts_per_sink() {
        let s = Stats::default();
        let t0 = Instant::now();
        s.on_sink_total_at(t0, 0);
        assert_eq!(
            s.snapshot_at(t0).egress_bytes,
            None,
            "a zero total has sent nothing"
        );
        s.on_sink_total_at(t0 + ms(250), 125_000);
        s.on_sink_total_at(t0 + ms(1_250), 625_000);
        let snap = s.snapshot_at(t0 + ms(1_300));
        assert_eq!(snap.egress_bytes, Some(625_000));
        // Window 0 ran from 0 (the first reading) to 125 000 at its last reading.
        assert_eq!(snap.bitrate_kbps, Some(1_000));
        s.on_new_sink();
        assert_eq!(
            s.snapshot_at(t0 + ms(1_300)).bitrate_kbps,
            None,
            "a new sink has no window"
        );
        s.on_sink_total_at(t0 + ms(1_500), 1_000);
        assert_eq!(s.snapshot_at(t0 + ms(1_500)).egress_bytes, Some(626_000));
        // A stale lower reading never lowers the total.
        s.on_sink_total_at(t0 + ms(1_600), 10);
        assert_eq!(s.snapshot_at(t0 + ms(1_600)).egress_bytes, Some(626_000));
    }

    #[test]
    fn encoder_speed_is_pts_advance_over_wall_time() {
        let s = Stats::default();
        let t0 = Instant::now();
        // Real time: one 33 ms frame each 33 ms.
        for i in 0..31u64 {
            s.on_encoded_video_at(t0 + ms(i * 33), ms(10_000 + i * 33));
        }
        let speed = s.snapshot_at(t0 + ms(1_000)).encoder_speed.expect("one window");
        assert!((speed - 0.990).abs() < 1e-9, "{speed}");
        // Starved: nothing for 2 s reads 0.
        assert_eq!(s.snapshot_at(t0 + ms(3_000)).encoder_speed, Some(0.0));
    }
}
