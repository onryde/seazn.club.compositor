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

/// Encodes `pictures` (I420, `l`'s layout) with the camera's x264 settings, decodes them with
/// avdec_h264, scales to `out` when given, and returns each decoded frame's strip reading in PTS
/// order with its PTS index. The count is the caller's to check (a lost frame is a failure).
pub fn x264_round_trip(
    l: &Layout,
    out: Option<(u32, u32)>,
    pictures: Vec<Vec<u8>>,
) -> Vec<(u64, Option<u32>)> {
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
    let mut read = Vec::new();
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
        let pts = b.pts().expect("pts").nseconds();
        read.push(((pts + FRAME_NS / 2) / FRAME_NS, strip::read(&y)));
        if read.len() as u64 == n {
            break;
        }
    }
    pusher.join().expect("pusher");
    wait_eos(&pipeline, "x264 round trip");
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

/// Writes `frames` as a fragmented MP4 (mp4mux, 1 s fragments, as MediaMTX's fMP4 recording) with
/// H.264 and AAC, PTS from 0 at 30 fps.
pub fn write_mp4(path: &Path, l: &Layout, frames: &[SegFrame]) {
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
        let pcm: Vec<u8> = audio::frame_samples(f.audio_counter)
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        a.push_buffer(stamp(pcm, i as u64)).expect("audio push");
    }
    v.end_of_stream().expect("eos");
    a.end_of_stream().expect("eos");
    wait_eos(&pipeline, "mp4 writer");
}
