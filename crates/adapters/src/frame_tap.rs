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
//!   (`appsrc → videoscale → videoconvert → 640×360 → jpegenc → appsink`), on a bus nobody else reads:
//!   an encoder error is this grab's error, never the programme's (whose bus errors are fatal). An
//!   encoder that failed or timed out is torn down on a reaper thread and built afresh at the next grab.
//! - **Bounded waits**: `FRAME_GRAB_WAIT_MS` for the frame, `FRAME_ENCODE_WAIT_MS` for its JPEG.
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
        if jpeg.is_err() {
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
                self.probe.armed.store(false, Ordering::SeqCst);
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
