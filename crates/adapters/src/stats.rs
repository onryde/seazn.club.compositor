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
//! | `camera_frames_replaced` | camera frames no tick put on air; a camera tick counts the frames written since the tick before, less the one it shows, a slate tick none (`ReplacedFrames`) | the first camera tick |
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
    /// The session's total of `ReplacedFrames`. Not on the heartbeat (the contract is strict,
    /// `docs/_INDEX.md` Owed 90): the driver logs it once a beat.
    pub camera_frames_replaced: Option<u64>,
}

/// Camera frames the camera slot replaced before any tick put them on air (the coordinator's
/// recommendation, approved by the owner 2026-10-24; `docs/_INDEX.md` Owed 81 (i); PR #59's
/// stage-by-stage finding). The slot holds only the newest frame (`FrameStore`), and each tick shows
/// whatever it holds, so frames that arrive in a burst between two ticks are overwritten unseen and
/// nothing else counts them: `fps` counts ticks and `droppedFrames` the ladder's overruns.
///
/// Fed once a tick with `written`, the frames the slot has taken so far. A tick that puts the camera
/// on air (`Emit`, `Repeat`) shows the newest of the frames written since the tick before it and
/// counts the rest; a slate tick (`Waiting`, `Reconnecting`, `Final`) counts none, because a slate is
/// on air by the programme's own rule and the phase already says so. Frames the input never wrote
/// (a gated build's, an appsink drop) are not in `written`, so they are not counted here.
///
/// **What it measures:** the frames above one a camera tick, which is input judder only for a source
/// at the programme's rate. A steady source faster than the output reads as replaced too: a 60 fps
/// camera on a 30 fps programme writes 2 a tick and reads 1 a tick, which is replacement, not
/// judder. A slower one reads 0 while ticks repeat (a 25 fps camera: 1 tick in 6). A late tick
/// thread raises it as well: the driver runs late ticks back to back, so a stall of `k` periods
/// counts the `k − 1` frames that arrived in it, with no `ticks skipped` line below 1 s. Read it
/// against the source's rate and the driver's lateness lines (Owed 89).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplacedFrames {
    /// The highest `written` any tick has read.
    read: u64,
    /// `None` until a tick has put the camera on air.
    total: Option<u64>,
}

impl ReplacedFrames {
    /// One tick. `written` never falls (the slot only counts up); a lower one is not a newer frame,
    /// so it counts nothing and leaves the mark where it was.
    pub fn on_tick(&mut self, written: u64, camera_on_air: bool) {
        let new = written.saturating_sub(self.read);
        self.read = self.read.max(written);
        if camera_on_air {
            // The newest of the new frames is the one on air; the rest were overwritten unseen.
            *self.total.get_or_insert(0) += new.saturating_sub(1);
        }
    }

    pub fn total(&self) -> Option<u64> {
        self.total
    }
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
    /// The sink whose readings count: `on_new_sink`'s serial (0 before the first).
    sink: u64,
    /// Sinks started so far.
    sinks: u64,
    egress_seen: bool,
    bitrate: Windowed,
    encoder: Windowed,
    camera: ReplacedFrames,
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

    /// A new sink starts counting from 0: the bitrate window restarts, the egress carries on. Returns
    /// the sink's serial, which its readings carry: from now on only its readings count.
    pub fn on_new_sink(&self) -> u64 {
        let mut s = locked(&self.inner);
        s.egress_before_sink += s.sink_total;
        s.sink_total = 0;
        s.bitrate = Windowed::default();
        s.sinks += 1;
        s.sink = s.sinks;
        s.sink
    }

    /// A poll read sink `sink`'s bytes-sent total. A retired sink's last poll can land after its
    /// successor started (PR #28 review item 1): it is not the current sink's, and changes nothing.
    pub fn on_sink_total(&self, sink: u64, total_bytes: u64) {
        self.on_sink_total_at(Instant::now(), sink, total_bytes);
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

    /// One programme tick's camera slot: `written`, the frames the slot has taken so far, and whether
    /// the tick put the camera on air (`ReplacedFrames::on_tick`).
    pub fn on_camera_tick(&self, written: u64, camera_on_air: bool) {
        locked(&self.inner).camera.on_tick(written, camera_on_air);
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

    pub(crate) fn on_sink_total_at(&self, now: Instant, sink: u64, total_bytes: u64) {
        let mut s = locked(&self.inner);
        // Checked under the lock `on_new_sink` takes, so no reading slips in between.
        if sink != s.sink {
            return;
        }
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
            camera_frames_replaced: s.camera.total(),
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
    use domain::config::OutputProfile;

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
        let first = s.on_new_sink();
        s.on_sink_total_at(t0, first, 0);
        assert_eq!(
            s.snapshot_at(t0).egress_bytes,
            None,
            "a zero total has sent nothing"
        );
        s.on_sink_total_at(t0 + ms(250), first, 125_000);
        s.on_sink_total_at(t0 + ms(1_250), first, 625_000);
        let snap = s.snapshot_at(t0 + ms(1_300));
        assert_eq!(snap.egress_bytes, Some(625_000));
        // Window 0 ran from 0 (the first reading) to 125 000 at its last reading.
        assert_eq!(snap.bitrate_kbps, Some(1_000));
        let second = s.on_new_sink();
        assert_eq!(
            s.snapshot_at(t0 + ms(1_300)).bitrate_kbps,
            None,
            "a new sink has no window"
        );
        s.on_sink_total_at(t0 + ms(1_500), second, 1_000);
        assert_eq!(s.snapshot_at(t0 + ms(1_500)).egress_bytes, Some(626_000));
        // A stale lower reading never lowers the total.
        s.on_sink_total_at(t0 + ms(1_600), second, 10);
        assert_eq!(s.snapshot_at(t0 + ms(1_600)).egress_bytes, Some(626_000));
    }

    // PR #28 review item 1: the retired sink's last poll lands after its successor started. It
    // counts for nothing: not in the egress, and not as the new sink's first bitrate reading.
    #[test]
    fn a_retired_sinks_late_reading_changes_nothing() {
        let s = Stats::default();
        let t0 = Instant::now();
        let old = s.on_new_sink();
        s.on_sink_total_at(t0, old, 4_000_000);
        let new = s.on_new_sink();
        assert_ne!(old, new);
        s.on_sink_total_at(t0 + ms(10), old, 4_100_000);
        assert_eq!(
            s.snapshot_at(t0 + ms(10)).egress_bytes,
            Some(4_000_000),
            "the late reading"
        );
        // The new sink at 1 Mbps: its window 0 runs from its first poll (250 ms, 0 bytes) and ends
        // before 1 250 ms (half-open, as `egress_is_cumulative…` pins), at 125 000 bytes.
        for (at, total) in [(250, 0), (750, 62_500), (1_240, 125_000)] {
            s.on_sink_total_at(t0 + ms(at), new, total);
        }
        let snap = s.snapshot_at(t0 + ms(1_300));
        assert_eq!(snap.egress_bytes, Some(4_125_000));
        assert_eq!(snap.bitrate_kbps, Some(1_000));
        // Before the fix the late reading was the new sink's: egress 8 100 000, bitrate 0.
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

    // ---- camera frames replaced (Owed 81 (i)) ----

    /// Both output profiles (AGENTS.md §5, "the other mode"); the tick grid runs at each one's fps.
    /// Both are 30 fps today, so this is one grid swept twice, not two: the rule reads only write
    /// counts, never the rate, and a profile at another rate would put its own grid here unchanged.
    const PROFILES: [OutputProfile; 2] = [OutputProfile::P720, OutputProfile::P1080];

    /// The driver's tick grid (C08): tick `i` is due at ⌈i·1000 / fps⌉ ms.
    fn due(fps: u32, i: u64) -> u64 {
        (i * 1_000).div_ceil(u64::from(fps))
    }

    /// A frame written this long after a tick lands inside the interval that tick opens: under the
    /// grid's shortest step, so the next tick is the first that can show it.
    const INSIDE_MS: u64 = 10;

    /// A camera schedule in bursts: burst `j` is `sizes[j]` frames, all written `INSIDE_MS` after one
    /// tick, and the next burst comes `sizes[j]` ticks later, so the source keeps one frame a tick on
    /// average however it bunches (PR #59 measured frames held back 0, 1 or 2 periods, so they left
    /// in bursts of 1 to 3). The tick after a burst shows its newest frame, so a burst of `s` loses
    /// `s − 1`: the expected count is read off the schedule, never off the code.
    struct Schedule {
        arrivals: Vec<u64>,
        /// The tick that first sees each burst, and the burst's size.
        seen_by: Vec<(u64, u64)>,
        ticks: u64,
    }

    fn bursts(fps: u32, sizes: &[u64]) -> Schedule {
        let (mut arrivals, mut seen_by, mut tick) = (Vec::new(), Vec::new(), 0);
        for &size in sizes {
            assert!(size >= 1, "a burst holds a frame");
            for _ in 0..size {
                arrivals.push(due(fps, tick) + INSIDE_MS);
            }
            seen_by.push((tick + 1, size));
            tick += size;
        }
        Schedule {
            arrivals,
            seen_by,
            ticks: tick + 1,
        }
    }

    impl Schedule {
        /// The frames lost when the ticks `on_air` picks show the camera: each burst a camera tick
        /// first sees loses all but its newest.
        fn expected(&self, on_air: impl Fn(u64) -> bool) -> u64 {
            self.seen_by
                .iter()
                .filter(|(tick, _)| on_air(*tick))
                .map(|(_, size)| size - 1)
                .sum()
        }

        /// The slot on the tick grid: each tick reads the frames written at or before its instant.
        fn run(&self, fps: u32, on_air: impl Fn(u64) -> bool) -> Option<u64> {
            let s = Stats::default();
            for i in 0..self.ticks {
                let written = self.arrivals.partition_point(|&a| a <= due(fps, i)) as u64;
                s.on_camera_tick(written, on_air(i));
            }
            s.snapshot_at(Instant::now()).camera_frames_replaced
        }
    }

    /// The brief's schedules (bursts of 2 and 3 against a steady 30 fps), plus PR #59's mixed shape,
    /// on every tick on air. Steady reads exactly 0; every burst loses all but its newest.
    #[test]
    fn camera_frames_replaced_counts_each_bursts_unshown_frames() {
        let mixed: Vec<u64> = [1, 2, 3, 1, 1, 2, 1, 3, 2, 1].repeat(30);
        let cases: [(&str, Vec<u64>, u64); 4] = [
            ("steady", vec![1; 300], 0),
            // Also what a steady 60 fps source reads on this 30 fps grid: 2 frames an interval.
            ("pairs", vec![2; 150], 150),
            ("threes", vec![3; 100], 200),
            ("mixed", mixed, 210),
        ];
        let (mut runs, mut frames) = (0, 0);
        for profile in PROFILES {
            let fps = profile.fps;
            assert!(
                INSIDE_MS < due(fps, 1),
                "{fps} fps: a burst must sit inside one interval"
            );
            for (name, sizes, want) in &cases {
                let sched = bursts(fps, sizes);
                // The schedule's own count, by the rule, agrees with the hand count above.
                assert_eq!(
                    sched.expected(|_| true),
                    *want,
                    "{name}: the schedule's own count"
                );
                // One frame a tick on average: a count that read frames minus ticks would say 0 here.
                assert_eq!(sched.arrivals.len() as u64 + 1, sched.ticks, "{name}");
                assert_eq!(sched.run(fps, |_| true), Some(*want), "{name} at {fps} fps");
                runs += 1;
                frames += sched.arrivals.len();
            }
        }
        eprintln!("replaced-frame schedules checked: {runs}, frames: {frames}");
        assert_eq!(runs, PROFILES.len() * cases.len());
        assert!(frames > 0, "anti-vacuity: no frame scheduled");
    }

    /// A slate tick shows no camera, by the programme's rule, so the frames it passes over are not
    /// counted: not the waiting slate's, not a reconnect's, not the final hold's. The first camera
    /// tick after a slate counts only what arrived since the slate tick before it.
    #[test]
    fn camera_frames_replaced_counts_nothing_under_a_slate() {
        let threes = vec![3; 100];
        let mut checked = 0;
        for profile in PROFILES {
            let fps = profile.fps;
            // Steady frames under 30 waiting ticks, then live: the first live tick sees one new frame.
            let steady = bursts(fps, &[1; 120]);
            assert_eq!(
                steady.run(fps, |i| i >= 30),
                Some(0),
                "{fps} fps: waiting, then live"
            );
            checked += 1;
            // Bursts throughout, the camera on air only from tick 150 (the waiting slate before it).
            let sched = bursts(fps, &threes);
            let live_from = |i: u64| i >= 150;
            let want = sched.expected(live_from);
            assert!(
                want > 0 && want < 200,
                "{fps} fps: the slate must cover some bursts, not all"
            );
            assert_eq!(
                sched.run(fps, live_from),
                Some(want),
                "{fps} fps: waiting, then live"
            );
            checked += 1;
            // A reconnect slate in the middle, then live again, then the final slate.
            let on_air = |i: u64| (30..120).contains(&i) || (180..270).contains(&i);
            let want = sched.expected(on_air);
            assert!(want > 0, "{fps} fps: some bursts on air");
            assert_eq!(
                sched.run(fps, on_air),
                Some(want),
                "{fps} fps: live, slate, live, final"
            );
            checked += 1;
        }
        eprintln!("slate schedules checked: {checked}");
        assert_eq!(checked, 3 * PROFILES.len());
    }

    /// Empty inputs: nothing is measured until a tick puts the camera on air (absent, never 0), and
    /// a camera tick with no frame at all is a measured 0.
    #[test]
    fn camera_frames_replaced_is_absent_until_the_camera_is_on_air() {
        let s = Stats::default();
        assert_eq!(
            s.snapshot_at(Instant::now()).camera_frames_replaced,
            None,
            "fresh"
        );
        // Frames under a slate only: the waiting slate's frames are not a measurement.
        for written in [0, 3, 9] {
            s.on_camera_tick(written, false);
        }
        assert_eq!(
            s.snapshot_at(Instant::now()).camera_frames_replaced,
            None,
            "slate only"
        );
        let empty = Stats::default();
        empty.on_camera_tick(0, true);
        assert_eq!(
            empty.snapshot_at(Instant::now()).camera_frames_replaced,
            Some(0),
            "a camera tick with no frame written replaced nothing"
        );
    }

    /// A second tick that finds nothing new changes nothing; a `written` lower than one already read
    /// (the slot only counts up, so it cannot come from `FrameStore`) counts nothing and does not
    /// lower the mark, so the next real count starts from the highest read.
    #[test]
    fn camera_frames_replaced_never_counts_twice_or_backwards() {
        let mut r = ReplacedFrames::default();
        r.on_tick(3, true);
        assert_eq!(r.total(), Some(2), "three frames, one shown");
        r.on_tick(3, true);
        assert_eq!(r.total(), Some(2), "a repeat tick: nothing new");
        r.on_tick(1, true);
        assert_eq!(r.total(), Some(2), "a lower count is not a newer frame");
        r.on_tick(6, true);
        // From the mark 3, not from 1: three new frames, one shown (from 1 it would read 2 + 4).
        assert_eq!(r.total(), Some(4));
    }
}
