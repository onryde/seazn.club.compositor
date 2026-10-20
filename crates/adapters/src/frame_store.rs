//! The latest-frame slots the four pipelines share (C09; spec §3.3's refinement). The input pipeline
//! (C10) writes the camera slot and the audio ring, the graphics pipeline (C11) writes the overlay
//! slot, and the programme pipeline (C09) reads all three once per tick. Every slot is bounded: a
//! video slot holds one sample (the newest replaces it), and the audio ring holds at most 400 ms and
//! drops the oldest bytes (plan, Global Constraints: "audio 400 ms … leaky, dropping the oldest").
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gstreamer as gst;

/// The audio the programme encodes: S16LE, 48 kHz, stereo, interleaved (C10's capsfilter; the F-004
/// verdict's AAC-LC 128 kbps 48 kHz stereo is encoded from it).
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u32 = 2;
/// One S16LE stereo frame: two channels of two bytes.
pub const AUDIO_FRAME_BYTES: usize = 4;
/// The ring's cap: 400 ms (plan, Global Constraints, "audio 400 ms") of 48 kHz stereo S16LE,
/// 0.4 × 48 000 × 4 = 76 800 bytes.
pub const AUDIO_RING_BYTES: usize = 76_800;

/// Shared by every pipeline; a clone shares the same slots.
#[derive(Clone, Default)]
pub struct FrameStore {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    camera: Mutex<Option<gst::Sample>>,
    overlay: Mutex<Option<gst::Sample>>,
    audio: Mutex<VecDeque<u8>>,
    /// Audio pushes refused because they were not whole S16LE stereo frames.
    audio_refused: AtomicU64,
    /// The ring's real bytes handed out by `take_audio*` (the padding is not counted).
    audio_taken: AtomicU64,
}

/// A slot's lock, taken even if a writer panicked while holding it: the slot holds plain data, so
/// the value is still whole, and a panicked pipeline thread must not take the others down with it.
fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FrameStore {
    pub fn set_camera(&self, s: gst::Sample) {
        *locked(&self.inner.camera) = Some(s);
    }

    pub fn camera(&self) -> Option<gst::Sample> {
        locked(&self.inner.camera).clone()
    }

    pub fn set_overlay(&self, s: gst::Sample) {
        *locked(&self.inner.overlay) = Some(s);
    }

    pub fn overlay(&self) -> Option<gst::Sample> {
        locked(&self.inner.overlay).clone()
    }

    /// Empties the overlay slot (`hide_overlay`, C05's graphics Failed). It sets nothing else: the
    /// next `set_overlay` puts a frame on air again (Owed 17: no hide is sticky, because no
    /// `show_overlay` port exists to lift one).
    pub fn clear_overlay(&self) {
        *locked(&self.inner.overlay) = None;
    }

    /// Appends `bytes` (whole S16LE stereo frames) and drops the oldest bytes past 400 ms. A push that
    /// is not a whole number of frames is refused and counted: taking it would shift every later
    /// sample by a byte, and the rest of the session would encode as noise.
    pub fn push_audio(&self, bytes: &[u8]) {
        if !bytes.len().is_multiple_of(AUDIO_FRAME_BYTES) {
            self.inner.audio_refused.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut ring = locked(&self.inner.audio);
        ring.extend(bytes.iter().copied());
        let excess = ring.len().saturating_sub(AUDIO_RING_BYTES);
        ring.drain(..excess);
    }

    /// Exactly `n` bytes: the oldest `min(n, level)` bytes of the ring, after `n − level` bytes of
    /// digital silence when the ring holds less than `n`. The programme never encodes these zeros
    /// (F-004): it calls `take_audio_padded` with its noise filler.
    pub fn take_audio(&self, n: usize) -> Vec<u8> {
        self.take_audio_padded(n, |pad| pad.fill(0))
    }

    /// `take_audio` with the padding written by `pad`. The padding comes FIRST and the ring's bytes
    /// last: the bytes kept are the newest of the timeline, so they stay at its end.
    pub fn take_audio_padded(&self, n: usize, pad: impl FnOnce(&mut [u8])) -> Vec<u8> {
        let mut out = vec![0u8; n];
        let mut ring = locked(&self.inner.audio);
        let real = n.min(ring.len());
        let gap = n - real;
        for (slot, b) in out[gap..].iter_mut().zip(ring.drain(..real)) {
            *slot = b;
        }
        // Counted under the ring's lock, so a `clear_audio` that returns has seen every take before it.
        self.inner.audio_taken.fetch_add(real as u64, Ordering::Relaxed);
        drop(ring);
        pad(&mut out[..gap]);
        out
    }

    /// Empties the ring and returns the bytes it dropped. The input calls it when a build stops
    /// passing (its abandon, its loss, its rebuild), with that build's gate shut, so none of the
    /// build's audio still in the ring plays after it (the PR #35 review's m1).
    pub fn clear_audio(&self) -> usize {
        let mut ring = locked(&self.inner.audio);
        let dropped = ring.len();
        ring.clear();
        dropped
    }

    /// The ring's real bytes handed out by `take_audio*` so far; the padding is not counted.
    pub fn audio_taken(&self) -> u64 {
        self.inner.audio_taken.load(Ordering::Relaxed)
    }

    /// The bytes the ring holds now.
    pub fn audio_level(&self) -> usize {
        locked(&self.inner.audio).len()
    }

    /// Audio pushes refused so far (not whole frames).
    pub fn audio_refused(&self) -> u64 {
        self.inner.audio_refused.load(Ordering::Relaxed)
    }
}

impl fmt::Debug for FrameStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameStore")
            .field("camera", &locked(&self.inner.camera).is_some())
            .field("overlay", &locked(&self.inner.overlay).is_some())
            .field("audio_bytes", &self.audio_level())
            .finish()
    }
}
