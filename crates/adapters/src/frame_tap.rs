//! The programme's frame tap (R4 sponsors on stream, task SC2; spec 2026-10-27 §3.6.3): one composed
//! programme frame on request, as a JPEG at `FRAME_GRAB_SIZE` and `FRAME_GRAB_JPEG_QUALITY`.
//!
//! - **The programme is never touched.** The tap is a buffer probe on the programme's encoder caps
//!   (`enc_caps`' src pad: the composed I420 frame, base and overlay, just before the encoder's queue),
//!   added at `start()` when the binary asked for a tap beforehand (`GstProgramme::frame_tap`), so C09's
//!   graph gains no element and is still never rebuilt. Off (no tap asked for), no probe exists.
//! - **The streaming thread never waits.** The probe does one atomic swap per frame; only when a grab
//!   armed it does it take a reference to that frame's buffer and `try_send` it into a slot that holds
//!   one (a `bounded(1)` channel: AGENTS.md §4). A full slot drops it.
//! - **The copy and the encode are the caller's** (the frame worker's thread): the frame is deep-copied
//!   at once, releasing the programme's buffer, then encoded by a pipeline of the tap's own
//!   (`appsrc → videoscale → videoconvert → 640×360 full-range I420 → jpegenc → appsink`), on a bus
//!   nobody else reads: an encoder error is this grab's error, never the programme's (whose bus errors
//!   are fatal). Each grab empties that bus, so nothing piles up on it. An encoder that failed or
//!   timed out is torn down on a reaper thread and built afresh at the next grab.
//! - **Full range** (SC2 polish): the programme is BT.709 limited range, and a JPEG is JFIF's full-range
//!   BT.601 YCbCr (ITU-T T.871 §7), which every decoder assumes. So the encoder's caps ask
//!   `videoconvert` for `JFIF_COLORIMETRY`; encoded as-is, the proof's blacks would decode at 16.
//! - **Bounded waits**: `FRAME_GRAB_WAIT_MS` for the frame, `FRAME_ENCODE_WAIT_MS` for its JPEG.
//! - **No frame left behind**: a grab that times out after the probe took its arm takes the frame the
//!   probe is handing over (within `FRAME_GRAB_WAIT_MS` again) and drops it, so no programme buffer
//!   waits in the slot until the next grab, 20 s later.
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use app::ports::FrameTap;
use crossbeam_channel::{bounded, Receiver, Sender};
use domain::config::{FRAME_ENCODE_WAIT_MS, FRAME_GRAB_JPEG_QUALITY, FRAME_GRAB_SIZE, FRAME_GRAB_WAIT_MS};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::gst_output::{self, Shared};

/// JFIF's YCbCr as GStreamer's colorimetry string (`range:matrix:transfer:primaries`): range 1
/// (`0_255`), matrix 4 (BT.601), transfer and primaries 0 (unknown). ITU-T T.871 §7 defines JPEG's
/// YCbCr as full-range BT.601, and `jpegdec` reads every YCbCr JPEG as exactly this.
const JFIF_COLORIMETRY: &str = "1:4:0:0";

/// The programme's side of the tap: the probe's state.
pub(crate) struct TapProbe {
    /// A grab waits for the next frame.
    armed: AtomicBool,
    /// The slot, which holds one frame.
    slot: Sender<gst::Sample>,
}

impl TapProbe {
    /// On the programme's streaming thread, for every composed frame: when armed, disarms and hands
    /// the frame over, never waiting. True when it handed one over (or tried to: the slot was full).
    pub(crate) fn on_buffer(&self, pad: &gst::Pad, buffer: &gst::Buffer) -> bool {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return false;
        }
        let caps = pad.current_caps();
        let sample = gst::Sample::builder()
            .buffer(buffer)
            .caps_if_some(caps.as_ref())
            .build();
        let _ = self.slot.try_send(sample);
        true
    }
}

/// The JPEG encoder the tap keeps between grabs.
struct Encoder {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    sink: gst_app::AppSink,
    /// The caps it was built for (the programme's, which never change: C09 never rebuilds).
    caps: gst::Caps,
}

impl Encoder {
    fn build(caps: &gst::Caps) -> Result<Self, String> {
        let make = |name: &str| {
            gst::ElementFactory::make(name)
                .build()
                .map_err(|e| format!("frame tap: no {name} element: {e}"))
        };
        let pipeline = gst::Pipeline::new();
        // One frame at a time: each grab pushes one buffer and pulls its JPEG before the next.
        let src = gst_app::AppSrc::builder()
            .caps(caps)
            .format(gst::Format::Time)
            .is_live(false)
            .do_timestamp(false)
            .block(false)
            .build();
        let scale = make("videoscale")?;
        let convert = make("videoconvert")?;
        let size = make("capsfilter")?;
        size.set_property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .field("colorimetry", JFIF_COLORIMETRY)
                .field("width", FRAME_GRAB_SIZE.0 as i32)
                .field("height", FRAME_GRAB_SIZE.1 as i32)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build(),
        );
        let jpeg = make("jpegenc")?;
        jpeg.set_property("quality", FRAME_GRAB_JPEG_QUALITY as i32);
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(1)
            .drop(true)
            .build();
        sink.set_property("async", false);
        pipeline
            .add_many([
                src.upcast_ref(),
                &scale,
                &convert,
                &size,
                &jpeg,
                sink.upcast_ref(),
            ])
            .map_err(|e| format!("frame tap: {e}"))?;
        gst::Element::link_many([
            src.upcast_ref(),
            &scale,
            &convert,
            &size,
            &jpeg,
            sink.upcast_ref(),
        ])
        .map_err(|e| format!("frame tap: the encoder chain: {e}"))?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("frame tap: the encoder did not start: {e}"))?;
        Ok(Self {
            pipeline,
            src,
            sink,
            caps: caps.clone(),
        })
    }

    /// The first error on the encoder's own bus, if one was posted.
    fn bus_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        bus.pop_filtered(&[gst::MessageType::Error])
            .and_then(|m| match m.view() {
                gst::MessageView::Error(e) => Some(format!("frame tap: the encoder failed: {}", e.error())),
                _ => None,
            })
    }

    /// Drops every message on the encoder's bus. Nobody watches that bus, so each grab empties it
    /// (AGENTS.md §4): it holds at most one grab's messages. The encoder is idle between grabs, so
    /// the loop ends.
    fn drain_bus(&self) {
        if let Some(bus) = self.pipeline.bus() {
            while bus.pop().is_some() {}
        }
    }
}

/// The tap the binary hands the frame worker (`app::ports::FrameTap`).
pub struct GstFrameTap {
    probe: Arc<TapProbe>,
    frames: Receiver<gst::Sample>,
    shared: Shared,
    encoder: Option<Encoder>,
    /// Frames pushed into encoders so far: each one's PTS, in seconds, so they only move forward.
    pushed: u64,
}

impl fmt::Debug for GstFrameTap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GstFrameTap")
            .field("armed", &self.probe.armed.load(Ordering::SeqCst))
            .field("encoder", &self.encoder.is_some())
            .field("pushed", &self.pushed)
            .finish()
    }
}

impl GstFrameTap {
    /// The tap and its probe's state, which the programme installs at `start()`.
    pub(crate) fn new(shared: Shared) -> (Self, Arc<TapProbe>) {
        let (slot, frames) = bounded(1);
        let probe = Arc::new(TapProbe {
            armed: AtomicBool::new(false),
            slot,
        });
        (
            Self {
                probe: Arc::clone(&probe),
                frames,
                shared,
                encoder: None,
                pushed: 0,
            },
            probe,
        )
    }

    /// Tears the encoder down on a reaper thread; the next grab builds another.
    fn retire_encoder(&mut self) {
        if let Some(enc) = self.encoder.take() {
            gst_output::reap(
                &self.shared,
                enc.pipeline.upcast(),
                "frame tap encoder".to_owned(),
            );
        }
    }

    fn encode(&mut self, caps: &gst::Caps, mut buffer: gst::Buffer) -> Result<Vec<u8>, String> {
        if self.encoder.as_ref().is_some_and(|e| e.caps != *caps) {
            self.retire_encoder();
        }
        if self.encoder.is_none() {
            self.encoder = Some(Encoder::build(caps)?);
        }
        let Some(enc) = self.encoder.as_ref() else {
            return Err("frame tap: no encoder".to_owned());
        };
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_seconds(self.pushed));
        }
        self.pushed += 1;
        let pushed = enc.src.push_buffer(buffer);
        let jpeg = match pushed {
            Ok(_) => enc
                .sink
                .try_pull_sample(gst::ClockTime::from_mseconds(FRAME_ENCODE_WAIT_MS))
                .ok_or_else(|| {
                    enc.bus_error()
                        .unwrap_or_else(|| format!("frame tap: no JPEG within {FRAME_ENCODE_WAIT_MS} ms"))
                })
                .and_then(|s| {
                    let b = s.buffer().ok_or("frame tap: a JPEG sample with no buffer")?;
                    let m = b
                        .map_readable()
                        .map_err(|e| format!("frame tap: the JPEG is unreadable: {e}"))?;
                    Ok(m.as_slice().to_vec())
                }),
            Err(flow) => Err(enc
                .bus_error()
                .unwrap_or_else(|| format!("frame tap: the encoder refused the frame: {flow:?}"))),
        };
        if jpeg.is_ok() {
            enc.drain_bus();
        } else {
            self.retire_encoder();
        }
        jpeg
    }
}

impl FrameTap for GstFrameTap {
    fn grab(&mut self) -> Result<Vec<u8>, String> {
        // A frame left in the slot is older than this request: dropped.
        while self.frames.try_recv().is_ok() {}
        self.probe.armed.store(true, Ordering::SeqCst);
        let sample = match self
            .frames
            .recv_timeout(Duration::from_millis(FRAME_GRAB_WAIT_MS))
        {
            Ok(sample) => sample,
            Err(_) => {
                // Still armed: disarmed here, and no frame can come. Already disarmed: the probe took
                // the arm, and its frame is on its way (a `try_send`, which never waits). It is taken
                // and dropped, so no programme buffer waits in the slot for the next grab.
                if !self.probe.armed.swap(false, Ordering::SeqCst) {
                    let _ = self
                        .frames
                        .recv_timeout(Duration::from_millis(FRAME_GRAB_WAIT_MS));
                }
                return Err(format!("no programme frame within {FRAME_GRAB_WAIT_MS} ms"));
            }
        };
        let caps = sample
            .caps()
            .map(ToOwned::to_owned)
            .ok_or("the programme frame has no caps")?;
        // A copy of its own (its video meta, strides and offsets kept), so the programme's buffer goes
        // back to it here, before the encode.
        let frame = sample
            .buffer()
            .ok_or("the programme frame has no buffer")?
            .copy_deep()
            .map_err(|e| format!("the programme frame could not be copied: {e}"))?;
        drop(sample);
        self.encode(&caps, frame)
    }
}

impl Drop for GstFrameTap {
    fn drop(&mut self) {
        self.probe.armed.store(false, Ordering::SeqCst);
        self.retire_encoder();
    }
}

/// CI-only (GStreamer): the tap without a running programme. A thread stands in for the probe, taking
/// the arm and handing a frame over as `TapProbe::on_buffer` does.
#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Instant;

    use super::*;
    use crate::frame_store::FrameStore;
    use crate::gst_programme::GstProgramme;

    /// A tap from a programme that never starts: its probe is never installed, so only the stand-in
    /// below reaches it.
    fn tap() -> GstFrameTap {
        gst::init().expect("gstreamer");
        let (tx, _rx) = crossbeam_channel::bounded(64);
        let mut p = GstProgramme::new(FrameStore::default(), tx).with_log(Arc::new(|_: &str| {}));
        p.frame_tap().expect("a tap before start")
    }

    /// A `width`×`height` programme frame as the probe sees one: I420, BT.709 limited range (black).
    fn programme_frame(width: u32, height: u32) -> gst::Sample {
        let info = gstreamer_video::VideoInfo::builder(gstreamer_video::VideoFormat::I420, width, height)
            .build()
            .expect("info");
        let mut buffer = gst::Buffer::with_size(info.size()).expect("buffer");
        {
            let b = buffer.get_mut().expect("writable");
            let mut m = b.map_writable().expect("map");
            let luma = info.stride()[0] as usize * height as usize;
            m.as_mut_slice()[..luma].fill(16);
            m.as_mut_slice()[luma..].fill(128);
        }
        gst::Sample::builder()
            .buffer(&buffer)
            .caps(&info.to_caps().expect("caps"))
            .build()
    }

    /// The probe's part: waits (bounded) for the arm, takes it, waits `late`, then hands `frame`
    /// over. True when the slot took it.
    fn probe_hands_over(tap: &GstFrameTap, late: Duration, frame: gst::Sample) -> thread::JoinHandle<bool> {
        let probe = Arc::clone(&tap.probe);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !probe.armed.swap(false, Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "the grab never armed the probe");
                thread::sleep(Duration::from_millis(1));
            }
            thread::sleep(late);
            probe.slot.try_send(frame).is_ok()
        })
    }

    /// m2 (SC2 polish): the probe took the arm, but its frame lands after the wait ran out. The grab
    /// fails as one with no frame, and takes that frame, so the slot holds nothing.
    #[test]
    fn a_frame_handed_over_after_the_wait_is_taken_and_never_left_in_the_slot() {
        let mut tap = tap();
        let late = probe_hands_over(
            &tap,
            Duration::from_millis(FRAME_GRAB_WAIT_MS + 200),
            programme_frame(1_280, 720),
        );
        assert_eq!(
            tap.grab(),
            Err(format!("no programme frame within {FRAME_GRAB_WAIT_MS} ms"))
        );
        assert!(
            late.join().expect("the probe's thread"),
            "the frame was handed over"
        );
        assert!(tap.frames.is_empty(), "a programme frame waits in the slot");
        assert!(!tap.probe.armed.load(Ordering::SeqCst), "still armed");
    }

    /// m3 (SC2 polish): each grab leaves the encoder's bus empty: what building it posted, and a
    /// message planted between two grabs.
    #[test]
    fn each_grab_leaves_the_encoders_bus_empty() {
        let mut tap = tap();
        let mut checked = 0;
        for planted in [false, true] {
            if planted {
                let bus = tap
                    .encoder
                    .as_ref()
                    .expect("the encoder is kept")
                    .pipeline
                    .bus()
                    .expect("bus");
                bus.post(gst::message::Application::new(gst::Structure::new_empty(
                    "sc2-planted",
                )))
                .expect("posted");
                assert!(bus.peek().is_some(), "the planted message is on the bus");
            }
            let hand = probe_hands_over(&tap, Duration::ZERO, programme_frame(1_280, 720));
            let jpeg = tap.grab().expect("a JPEG");
            assert!(hand.join().expect("the probe's thread"));
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "SOI");
            let bus = tap
                .encoder
                .as_ref()
                .expect("the encoder is kept")
                .pipeline
                .bus()
                .expect("bus");
            assert!(
                bus.peek().is_none(),
                "grab {checked}: the bus holds {:?}",
                bus.peek()
            );
            checked += 1;
        }
        assert_eq!(checked, 2);
    }

    /// The final review's SC2 m9: the encoder is rebuilt for new caps and kept for the same ones. The
    /// programme's caps never change (C09 never rebuilds), so this guard is a "cannot happen" made
    /// reachable here: a 720p frame, then two 1080p ones, each a JPEG. The caps change gets an encoder
    /// built for the new caps, and the repeat keeps that same encoder.
    #[test]
    fn a_frame_with_other_caps_gets_an_encoder_of_its_own() {
        let mut tap = tap();
        let mut before: Option<gst::Pipeline> = None;
        let mut checked = 0;
        for (k, (w, h)) in [(1_280, 720), (1_920, 1_080), (1_920, 1_080)]
            .into_iter()
            .enumerate()
        {
            let frame = programme_frame(w, h);
            let caps = frame.caps().expect("caps").to_owned();
            let hand = probe_hands_over(&tap, Duration::ZERO, frame);
            let jpeg = tap.grab().unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
            assert!(hand.join().expect("the probe's thread"));
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "{w}x{h}: SOI");
            let enc = tap.encoder.as_ref().expect("the encoder is kept");
            assert_eq!(enc.caps, caps, "{w}x{h}: the encoder's caps");
            match k {
                1 => assert_ne!(
                    before.as_ref(),
                    Some(&enc.pipeline),
                    "the caps change: a new encoder"
                ),
                2 => assert_eq!(
                    before.as_ref(),
                    Some(&enc.pipeline),
                    "the same caps: the same encoder"
                ),
                _ => {}
            }
            before = Some(enc.pipeline.clone());
            checked += 1;
        }
        assert_eq!(checked, 3);
    }
}
