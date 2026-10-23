//! The media tests' shared pieces: pictures, encoders and decoders built from GStreamer elements the
//! relay image carries (x264enc, avdec_h264, voaacenc, mp4mux). Each test file uses part of it.
#![allow(dead_code)]
use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use harness::audio;
use harness::camera::Layout;
use harness::media::{self, FRAME_NS};
use harness::strip::{self, Plane};

pub fn init() {
    media::init().expect("gstreamer");
}

pub fn make(factory: &str) -> gst::Element {
    gst::ElementFactory::make(factory)
        .build()
        .unwrap_or_else(|e| panic!("no {factory}: {e}"))
}

/// A frame with no strip: one luma value over the whole picture, neutral chroma.
pub fn slate(l: &Layout, luma: u8) -> Vec<u8> {
    let mut data = vec![128u8; l.size];
    data[..l.u_offset].fill(luma);
    data
}

/// A deterministic xorshift32 stream (no `rand` dependency); never yields 0.
pub struct Rng(pub u32);

impl Rng {
    pub fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

fn appsrc(caps: &gst::Caps) -> gst_app::AppSrc {
    gst_app::AppSrc::builder()
        .caps(caps)
        .format(gst::Format::Time)
        .is_live(false)
        .do_timestamp(false)
        .block(true)
        .max_bytes(32 << 20)
        .build()
}

fn stamp(data: Vec<u8>, i: u64) -> gst::Buffer {
    let mut b = gst::Buffer::from_mut_slice(data);
    let m = b.get_mut().expect("writable");
    m.set_pts(gst::ClockTime::from_nseconds(i * FRAME_NS));
    m.set_duration(gst::ClockTime::from_nseconds(FRAME_NS));
    b
}

fn wait_eos(pipeline: &gst::Pipeline, what: &str) {
    let bus = pipeline.bus().expect("bus");
    let msg = bus
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(300),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap_or_else(|| panic!("{what}: no EOS in 300 s"));
    if let gst::MessageView::Error(e) = msg.view() {
        panic!("{what}: {} ({:?})", e.error(), e.debug());
    }
    pipeline.set_state(gst::State::Null).expect("null");
}

/// One decoded frame of a round trip.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Decoded {
    /// Frames since the first decoded PTS.
    pub index: u64,
    pub strip: Option<u32>,
    /// The strip's cell means, for a failure's message ([`explain`]).
    pub cells: Option<Vec<u32>>,
}

/// A strip reading's diagnosis: the bits `want` should show, then what each cell read as (`0`, `1`,
/// or `?` for neither, a wrong `0` as `O` and a wrong `1` as `I`), then the cell means.
pub fn explain(want: Option<u32>, cells: &Option<Vec<u32>>) -> String {
    let Some(means) = cells else {
        return "cells: unreadable".into();
    };
    let want_bits = want.map(strip::bits);
    let read: String = means
        .iter()
        .enumerate()
        .map(|(k, &m)| {
            let r = if m < strip::DARK_BELOW {
                '0'
            } else if m > strip::LIGHT_ABOVE {
                '1'
            } else {
                '?'
            };
            let wrong = want_bits.is_some_and(|b| r != if (b >> (47 - k)) & 1 == 1 { '1' } else { '0' });
            match (wrong, r) {
                (true, '0') => 'O',
                (true, '1') => 'I',
                _ => r,
            }
        })
        .collect();
    let want_s = want_bits.map_or_else(|| "none".to_owned(), |b| format!("{b:048b}"));
    format!("want {want_s}\nread {read}\nmeans {means:?}")
}

/// Encodes `pictures` (I420, `l`'s layout) with the camera's x264 settings, decodes them with
/// avdec_h264, scales to `out` when given, and returns each decoded frame's strip reading in PTS
/// order with its PTS index. The count is the caller's to check (a lost frame is a failure).
///
/// The index counts from the first decoded PTS, not from 0: x264enc offsets every output timestamp
/// by 1 000 hours (GstVideoEncoder's minimum PTS, so a DTS never goes negative), which read as
/// index 108 000 001 for frame 0 on CI (run 39979756301). A lost first frame still fails the caller,
/// whose expected reading for frame 0 is that picture's own counter.
pub fn x264_round_trip(l: &Layout, out: Option<(u32, u32)>, pictures: Vec<Vec<u8>>) -> Vec<Decoded> {
    let pipeline = gst::Pipeline::new();
    let src = appsrc(&media::i420_caps(l.width, l.height).expect("caps"));
    let (enc, parse, dec, convert) = (
        media::x264enc().expect("x264enc"),
        make("h264parse"),
        make("avdec_h264"),
        make("videoconvert"),
    );
    let scale = make("videoscale");
    let (w, h) = out.unwrap_or((l.width, l.height));
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "I420")
        .field("width", w as i32)
        .field("height", h as i32)
        .build();
    let sink = gst_app::AppSink::builder().caps(&caps).sync(false).build();
    let chain: Vec<gst::Element> = vec![
        src.clone().upcast(),
        enc,
        parse,
        dec,
        convert,
        scale,
        sink.clone().upcast(),
    ];
    pipeline.add_many(&chain).expect("add");
    gst::Element::link_many(&chain).expect("link");
    pipeline.set_state(gst::State::Playing).expect("play");
    let n = pictures.len() as u64;
    let pusher = std::thread::spawn(move || {
        for (i, p) in pictures.into_iter().enumerate() {
            src.push_buffer(stamp(p, i as u64)).expect("push");
        }
        src.end_of_stream().expect("eos");
    });
    let mut decoded = Vec::new();
    while let Ok(sample) = sink.pull_sample() {
        let b = sample.buffer().expect("buffer");
        let info = gstreamer_video::VideoInfo::from_caps(sample.caps().expect("caps")).expect("info");
        let map = b.map_readable().expect("map");
        let stride = info.stride()[0] as usize;
        let y = Plane {
            data: &map.as_slice()[..stride * info.height() as usize],
            stride,
            width: info.width(),
            height: info.height(),
        };
        decoded.push((
            b.pts().expect("pts").nseconds(),
            strip::read(&y),
            strip::strip_cell_means(&y),
        ));
        if decoded.len() as u64 == n {
            break;
        }
    }
    pusher.join().expect("pusher");
    wait_eos(&pipeline, "x264 round trip");
    let first = decoded.iter().map(|(pts, _, _)| *pts).min().unwrap_or(0);
    let mut read: Vec<Decoded> = decoded
        .into_iter()
        .map(|(pts, strip, cells)| Decoded {
            index: (pts - first + FRAME_NS / 2) / FRAME_NS,
            strip,
            cells,
        })
        .collect();
    read.sort_unstable();
    read
}

/// What one test segment frame shows: `Some(counter)` for a strip frame, `None` for a mid-grey slate;
/// the audio is always `audio::frame_samples(audio_counter)`.
#[derive(Debug, Clone, Copy)]
pub struct SegFrame {
    pub strip: Option<u32>,
    pub audio_counter: u32,
}

/// The camera's samples for frame `counter`, delayed by `delay` samples (under one frame): the tail of
/// the frame before, then the head of this one. The audio then really is `delay` behind the picture.
pub fn delayed_samples(counter: u32, delay: u32) -> Vec<i16> {
    assert!(delay < audio::SAMPLES_PER_FRAME, "a delay under one frame");
    if delay == 0 {
        return audio::frame_samples(counter);
    }
    let before = audio::frame_samples(counter.saturating_sub(1));
    let this = audio::frame_samples(counter);
    let cut = (audio::SAMPLES_PER_FRAME - delay) as usize;
    before[cut..].iter().chain(&this[..cut]).copied().collect()
}

/// Writes `frames` as a fragmented MP4 (mp4mux, 1 s fragments, as MediaMTX's fMP4 recording) with
/// H.264 and AAC, PTS from 0 at 30 fps; the audio `audio_delay` samples behind the picture.
pub fn write_mp4(path: &Path, l: &Layout, frames: &[SegFrame], audio_delay: u32) {
    let pipeline = gst::Pipeline::new();
    let v = appsrc(&media::i420_caps(l.width, l.height).expect("caps"));
    let a = appsrc(&media::audio_caps());
    let mux = make("mp4mux");
    mux.set_property("fragment-duration", 1_000u32);
    let sink = make("filesink");
    sink.set_property("location", path.to_str().expect("utf-8"));
    let vchain: Vec<gst::Element> = vec![
        v.clone().upcast(),
        media::x264enc().expect("x264enc"),
        make("h264parse"),
        make("queue"),
    ];
    let achain: Vec<gst::Element> = vec![
        a.clone().upcast(),
        make("audioconvert"),
        make("voaacenc"),
        make("aacparse"),
        make("queue"),
    ];
    pipeline
        .add_many(vchain.iter().chain(achain.iter()).chain([&mux, &sink]))
        .expect("add");
    gst::Element::link_many(&vchain).expect("video");
    gst::Element::link_many(&achain).expect("audio");
    vchain[3].link(&mux).expect("video mux");
    achain[4].link(&mux).expect("audio mux");
    mux.link(&sink).expect("sink");
    pipeline.set_state(gst::State::Playing).expect("play");
    for (i, f) in frames.iter().enumerate() {
        let picture = match f.strip {
            Some(c) => harness::camera::frame(l, harness::camera::Pattern::Ball, c),
            None => slate(l, 128),
        };
        v.push_buffer(stamp(picture, i as u64)).expect("video push");
        let pcm: Vec<u8> = delayed_samples(f.audio_counter, audio_delay)
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        a.push_buffer(stamp(pcm, i as u64)).expect("audio push");
    }
    v.end_of_stream().expect("eos");
    a.end_of_stream().expect("eos");
    wait_eos(&pipeline, "mp4 writer");
}
