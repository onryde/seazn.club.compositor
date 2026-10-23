//! The camera's audio and the checker's scan of the programme's.
//!
//! **The camera** (C13 brief, "Audio"): mono S16 at 48 kHz, 1 600 samples per frame at 30 fps, each
//! frame's samples stamped with that frame's PTS. A 1 kHz tone burst of 50 ms starts at every frame
//! whose `counter % 30 == 0`. Under it, always, a quiet 250 Hz floor: the programme carries real
//! input audio or low-level noise and never digital silence (F-004; `domain::programme`), so the
//! camera never sends digital silence either, and a silence the checker finds is the programme's.
//!
//! **The scan** reads 5 ms windows (240 samples, so 1 kHz is an exact Goertzel bin). A burst's onset
//! is a window whose 1 kHz amplitude passes [`TONE_DETECT`] after at least 100 ms without one, refined
//! to the first sample past half the burst's amplitude. Digital silence is a run of windows whose
//! peak is at most [`SILENCE_PEAK`]; a hole is a jump in the samples' timestamps. Both are reported
//! from 100 ms.
use std::f64::consts::PI;

/// The sample rate, camera and scan.
pub const RATE: u64 = 48_000;
/// Samples per camera frame at 30 fps.
pub const SAMPLES_PER_FRAME: u32 = 1_600;
/// A burst starts at every frame whose counter is a multiple of this.
pub const BURST_EVERY_FRAMES: u32 = 30;
/// The burst's length: 50 ms.
pub const BURST_SAMPLES: u32 = 2_400;
/// The burst's tone.
pub const TONE_HZ: f64 = 1_000.0;
/// The burst's amplitude, as a fraction of full scale.
pub const TONE_AMPLITUDE: f64 = 0.5;
/// The floor's tone, far from the burst's bin.
pub const FLOOR_HZ: f64 = 250.0;
/// The floor's amplitude (about −40 dBFS).
pub const FLOOR_AMPLITUDE: f64 = 0.01;
/// The scan's window: 5 ms.
pub const WINDOW: usize = 240;
/// A window whose 1 kHz amplitude passes this fraction of full scale holds a burst.
pub const TONE_DETECT: f64 = 0.1;
/// A window whose peak is at most this is digital silence.
pub const SILENCE_PEAK: i32 = 4;
/// Silences and holes are reported from this long (100 ms).
pub const REPORT_FROM_NS: u64 = 100_000_000;
/// The quiet before an onset: 100 ms of windows with no burst.
const QUIET_WINDOWS: u32 = 20;

/// The camera's samples for frame `counter` (counted from 1).
pub fn frame_samples(counter: u32) -> Vec<i16> {
    let base = u64::from(counter.saturating_sub(1)) * u64::from(SAMPLES_PER_FRAME);
    // The latest frame at or before this one that starts a burst (0: none yet), and how far into
    // that burst this frame's first sample is.
    let phase = counter % BURST_EVERY_FRAMES;
    let burst_frame = counter - phase;
    let into = phase * SAMPLES_PER_FRAME;
    (0..SAMPLES_PER_FRAME)
        .map(|i| {
            let t = (base + u64::from(i)) as f64 / RATE as f64;
            let mut v = FLOOR_AMPLITUDE * (2.0 * PI * FLOOR_HZ * t).sin();
            if burst_frame > 0 && into + i < BURST_SAMPLES {
                v += TONE_AMPLITUDE * (2.0 * PI * TONE_HZ * t).sin();
            }
            (v * f64::from(i16::MAX)).round() as i16
        })
        .collect()
}

/// A span of the scanned stream, in its own timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_ns: u64,
    pub dur_ns: u64,
}

/// What a scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioFacts {
    pub samples: u64,
    pub onsets_ns: Vec<u64>,
    pub silences: Vec<Span>,
    pub holes: Vec<Span>,
}

/// The streaming scan: feed it buffers in timestamp order, then `finish`.
#[derive(Debug, Default)]
pub struct Scan {
    facts: AudioFacts,
    /// The timestamp of the first sample since the last rebase, and the samples since it.
    base_ns: Option<u64>,
    since_base: u64,
    window: Vec<i16>,
    previous: Vec<i16>,
    quiet: u32,
    silent_from: Option<u64>,
}

fn ns_of(samples: u64) -> u64 {
    samples * 1_000_000_000 / RATE
}

fn tone_amplitude(w: &[i16]) -> f64 {
    let n = w.len() as f64;
    let k = (n * TONE_HZ / RATE as f64).round();
    let coeff = 2.0 * (2.0 * PI * k / n).cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in w {
        let s0 = f64::from(x) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0);
    2.0 * power.sqrt() / n / f64::from(i16::MAX)
}

impl Scan {
    pub fn new() -> Self {
        Self::default()
    }

    fn now_ns(&self) -> u64 {
        self.base_ns.unwrap_or(0) + ns_of(self.since_base)
    }

    fn close_silence(&mut self, end_ns: u64) {
        if let Some(from) = self.silent_from.take() {
            if end_ns.saturating_sub(from) >= REPORT_FROM_NS {
                self.facts.silences.push(Span {
                    start_ns: from,
                    dur_ns: end_ns - from,
                });
            }
        }
    }

    /// One buffer of mono samples whose first sample is stamped `pts_ns`.
    pub fn feed(&mut self, pts_ns: u64, samples: &[i16]) {
        match self.base_ns {
            None => {
                self.base_ns = Some(pts_ns);
                self.since_base = self.window.len() as u64;
            }
            Some(_) => {
                let expected = self.now_ns();
                if pts_ns > expected + REPORT_FROM_NS {
                    self.facts.holes.push(Span {
                        start_ns: expected,
                        dur_ns: pts_ns - expected,
                    });
                }
                if pts_ns.abs_diff(expected) > REPORT_FROM_NS {
                    // Rebase on the new timestamps; the partial window and any silence end here.
                    self.close_silence(expected);
                    self.window.clear();
                    self.previous.clear();
                    self.quiet = 0;
                    self.base_ns = Some(pts_ns);
                    self.since_base = 0;
                }
            }
        }
        for &s in samples {
            self.window.push(s);
            self.since_base += 1;
            self.facts.samples += 1;
            if self.window.len() == WINDOW {
                self.window_done();
            }
        }
    }

    fn window_done(&mut self) {
        let end_ns = self.now_ns();
        let start_ns = end_ns - ns_of(WINDOW as u64);
        let peak = self.window.iter().map(|&s| i32::from(s).abs()).max().unwrap_or(0);
        if peak <= SILENCE_PEAK {
            self.silent_from.get_or_insert(start_ns);
        } else {
            self.close_silence(start_ns);
        }
        if tone_amplitude(&self.window) > TONE_DETECT {
            if self.quiet >= QUIET_WINDOWS {
                // The first sample past half the burst's amplitude, in the previous window or this one.
                let half = (TONE_AMPLITUDE / 2.0 * f64::from(i16::MAX)) as i32;
                let prev_len = self.previous.len() as u64;
                let at = self
                    .previous
                    .iter()
                    .chain(self.window.iter())
                    .position(|&s| i32::from(s).abs() > half)
                    .map_or(0, |i| i as u64);
                let first_ns = start_ns.saturating_sub(ns_of(prev_len));
                self.facts.onsets_ns.push(first_ns + ns_of(at));
            }
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(1);
        }
        self.previous = std::mem::take(&mut self.window);
    }

    pub fn finish(mut self) -> AudioFacts {
        let end = self.now_ns();
        self.close_silence(end);
        self.facts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_pts(counter: u32) -> u64 {
        // Frame n starts (n − 1) frames after the first, at 30 fps.
        u64::from(counter - 1) * 1_000_000_000 / 30
    }

    #[test]
    fn bursts_start_on_every_thirtieth_frame_and_last_50_ms() {
        // From the brief, not from the scan: frame n with n % 30 == 0 starts a 2 400-sample burst.
        let loud = |c: u32| {
            frame_samples(c)
                .iter()
                .filter(|s| s.unsigned_abs() > 8_000)
                .count()
        };
        assert_eq!(loud(29), 0, "frame 29 is floor only");
        assert!(loud(30) > 1_000, "frame 30 carries 1 600 burst samples");
        let f31 = frame_samples(31);
        assert!(
            f31[..800].iter().filter(|s| s.unsigned_abs() > 8_000).count() > 500,
            "frame 31's first 800"
        );
        assert_eq!(
            f31[800..].iter().filter(|s| s.unsigned_abs() > 8_000).count(),
            0,
            "then floor"
        );
        assert_eq!(loud(1), 0, "frame 1 follows no burst");
        assert_eq!(loud(32), 0, "the burst is over by frame 32");
        // The floor alone: quiet, and never digital silence in any scan window.
        let floor = frame_samples(7);
        let mut windows = 0;
        for w in floor.chunks(WINDOW) {
            let peak = w.iter().map(|s| i32::from(*s).abs()).max().unwrap();
            assert!(peak > SILENCE_PEAK && peak < 400, "a floor window's peak {peak}");
            windows += 1;
        }
        assert!(windows > 0);
    }

    #[test]
    fn the_scan_finds_every_burst_at_its_frame() {
        let mut scan = Scan::new();
        for c in 1..=300u32 {
            scan.feed(frame_pts(c), &frame_samples(c));
        }
        let f = scan.finish();
        let expected: Vec<u64> = (1..=300u32).filter(|c| c % 30 == 0).map(frame_pts).collect();
        eprintln!("onsets: {} found, {} expected", f.onsets_ns.len(), expected.len());
        assert_eq!(f.onsets_ns.len(), expected.len());
        for (got, want) in f.onsets_ns.iter().zip(&expected) {
            assert!(
                got.abs_diff(*want) < 1_000_000,
                "onset {got} ns against frame {want} ns"
            );
        }
        assert!(f.silences.is_empty() && f.holes.is_empty(), "{f:?}");
        assert_eq!(f.samples, 300 * u64::from(SAMPLES_PER_FRAME));
    }

    #[test]
    fn digital_silence_and_holes_are_reported_from_100_ms() {
        let mut scan = Scan::new();
        let mut pts = 0u64;
        let frame_ns = 1_000_000_000 / 30;
        for c in 1..=10u32 {
            scan.feed(pts, &frame_samples(c));
            pts += frame_ns;
        }
        // 9 frames (300 ms) of zeros, then 2 of zeros (66 ms) after the floor again: one silence.
        let silent_from = pts;
        for _ in 0..9 {
            scan.feed(pts, &[0; SAMPLES_PER_FRAME as usize]);
            pts += frame_ns;
        }
        for c in 20..=25u32 {
            scan.feed(pts, &frame_samples(c));
            pts += frame_ns;
        }
        for _ in 0..2 {
            scan.feed(pts, &[0; SAMPLES_PER_FRAME as usize]);
            pts += frame_ns;
        }
        // A 500 ms jump in the timestamps: one hole.
        let hole_from = pts;
        pts += 500_000_000;
        for c in 30..=40u32 {
            scan.feed(pts, &frame_samples(c));
            pts += frame_ns;
        }
        let f = scan.finish();
        assert_eq!(f.silences.len(), 1, "{:?}", f.silences);
        assert!(f.silences[0].start_ns.abs_diff(silent_from) <= 5_000_000);
        assert!(
            f.silences[0].dur_ns.abs_diff(300_000_000) <= 10_000_000,
            "{:?}",
            f.silences[0]
        );
        assert_eq!(f.holes.len(), 1, "{:?}", f.holes);
        assert!(f.holes[0].start_ns.abs_diff(hole_from) <= 1_000_000);
        assert!(f.holes[0].dur_ns.abs_diff(500_000_000) <= 1_000_000);
    }

    #[test]
    fn an_empty_scan_finds_nothing() {
        assert_eq!(Scan::new().finish(), AudioFacts::default());
    }
}
