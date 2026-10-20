//! C10's live leg (`_FINDINGS.md` F-012): the real input adapter reading one SRT playback URL, with
//! a minimal stand-in for the domain's rules around it (rebuild after `input_stall_after_ms` with no
//! frame, or `input_connect_timeout_ms` with no connect, or at once on a loss), printing every event.
//!
//! The playback URL comes from the environment and is never printed: `C10_SRT_PLAY_URL`, in
//! Cloudflare's form `srt://host:port?passphrase=…&streamid=…` (its values may be percent-encoded).
//! `C10_LEG_SECONDS` bounds the run (default 60) and `C10_LEG_PROFILE=1080` picks 1080p (default
//! 720p). Each line goes to stderr: `event t_ms=…`, `shape t_ms=…` (the camera slot, every 500 ms),
//! `log …` (the adapter's own lines, after its scrubber; any line still holding a credential is
//! withheld and counted instead), then one `summary` line. A person reads the verdict from them, so
//! this is an example, not a test: CI has no live endpoint.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adapters::frame_store::{FrameStore, AUDIO_CHANNELS, AUDIO_RATE};
use adapters::gst_input::GstInput;
use app::ports::{redacted_origin, Inbound, InputSource, InputSpec};
use domain::config::{OutputProfile, Thresholds};
use domain::supervisor::Event;
use gstreamer as gst;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;

fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// RFC 3986 percent-encoding of every byte outside the unreserved set (the leak check's own copy,
/// so it does not lean on the adapter's).
fn encode(v: &str, lower: bool) -> String {
    v.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                char::from(b).to_string()
            } else if lower {
                format!("%{b:02x}")
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `srt://host:port?passphrase=…&streamid=…` into the contract's three fields.
fn parse(full: &str) -> InputSpec {
    let (url, query) = full.split_once('?').unwrap_or((full, ""));
    let mut spec = InputSpec {
        url: url.to_owned(),
        stream_id: String::new(),
        passphrase: String::new(),
    };
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("streamid", v)) => spec.stream_id = decode(v),
            Some(("passphrase", v)) => spec.passphrase = decode(v),
            _ => {}
        }
    }
    spec
}

fn main() {
    let full = std::env::var("C10_SRT_PLAY_URL").unwrap_or_default();
    if full.is_empty() {
        eprintln!("input_leg: set C10_SRT_PLAY_URL");
        std::process::exit(2);
    }
    let seconds: u64 = std::env::var("C10_LEG_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let profile = match std::env::var("C10_LEG_PROFILE").as_deref() {
        Ok("1080") => OutputProfile::P1080,
        _ => OutputProfile::P720,
    };
    let spec = parse(&full);
    // Every form a credential can take in a line: raw, percent-encoded in either case, and the URI.
    let mut forms = vec![GstInput::srt_uri(&spec, 0)];
    for v in [&spec.stream_id, &spec.passphrase] {
        forms.extend([v.clone(), encode(v, false), encode(v, true)]);
    }
    forms.retain(|v| v.len() >= 8);
    let t = Thresholds::default();
    eprintln!(
        "input_leg: {} streamid {} chars, passphrase {} chars, latency {} ms, {}x{}, {seconds} s",
        redacted_origin(&spec.url),
        spec.stream_id.len(),
        spec.passphrase.len(),
        t.input_srt_latency_ms,
        profile.width,
        profile.height
    );

    gst::init().expect("gstreamer");
    let frames = FrameStore::default();
    let (tx, rx) = crossbeam_channel::bounded(8_192);
    let leaks = Arc::new(AtomicU64::new(0));
    let leaked = Arc::clone(&leaks);
    let t0 = Instant::now();
    let mut input = GstInput::new(frames.clone(), tx, &t, profile.width, profile.height).with_log(Arc::new(
        move |l: &str| {
            if forms.iter().any(|f| l.contains(f.as_str())) {
                leaked.fetch_add(1, Ordering::SeqCst);
                eprintln!(
                    "log t_ms={} <withheld: it holds a credential>",
                    t0.elapsed().as_millis()
                );
            } else {
                eprintln!("log t_ms={} {l}", t0.elapsed().as_millis());
            }
        },
    ));

    let (mut gen, mut built_at, mut last_frame, mut connected) = (1u64, Instant::now(), None, false);
    let (mut frames_seen, mut connects, mut losses) = (0u64, 0u64, 0u64);
    let (mut shapes, mut off_shape) = (0u64, 0u64);
    let mut next_shape = Instant::now();
    input.rebuild(gen, &spec);
    let end = t0 + Duration::from_secs(seconds);
    while Instant::now() < end {
        let now = Instant::now();
        let ms = |i: Instant| i.duration_since(t0).as_millis();
        while let Ok(Inbound::Event(e)) = rx.try_recv() {
            match e {
                Event::InputFrame { .. } => {
                    frames_seen += 1;
                    if last_frame.is_none_or(|l: Instant| now.duration_since(l) > Duration::from_millis(500))
                    {
                        eprintln!("event t_ms={} frames resume (gen {gen})", ms(now));
                    }
                    last_frame = Some(now);
                }
                Event::InputConnected { gen: g } => {
                    connects += 1;
                    connected = g == gen;
                    eprintln!(
                        "event t_ms={} InputConnected gen={g} {} ms after its build",
                        ms(now),
                        now.duration_since(built_at).as_millis()
                    );
                }
                Event::InputLost { gen: g, reason } => {
                    losses += 1;
                    eprintln!("event t_ms={} InputLost gen={g} {reason:?}", ms(now));
                    if g == gen {
                        gen += 1;
                        built_at = Instant::now();
                        connected = false;
                        last_frame = None;
                        eprintln!("event t_ms={} rebuild gen={gen} (loss)", ms(built_at));
                        input.rebuild(gen, &spec);
                    }
                }
                other => eprintln!("event t_ms={} {other:?}", ms(now)),
            }
        }
        // The domain's two timers, without its back-off: a stall, or a connect that never came.
        let stalled = connected
            && last_frame
                .is_some_and(|l| now.duration_since(l) >= Duration::from_millis(t.input_stall_after_ms));
        let no_connect =
            !connected && now.duration_since(built_at) >= Duration::from_millis(t.input_connect_timeout_ms);
        if stalled || no_connect {
            gen += 1;
            built_at = Instant::now();
            connected = false;
            last_frame = None;
            let why = if stalled { "stall" } else { "connect timeout" };
            eprintln!("event t_ms={} rebuild gen={gen} ({why})", ms(built_at));
            input.rebuild(gen, &spec);
        }
        if now >= next_shape {
            next_shape = now + Duration::from_millis(500);
            if let Some(sample) = frames.camera() {
                if let Some(info) = sample
                    .caps()
                    .and_then(|c| gst_video::VideoInfo::from_caps(c).ok())
                {
                    shapes += 1;
                    let ok = info.format() == gst_video::VideoFormat::I420
                        && (info.width(), info.height()) == (profile.width, profile.height)
                        && info.par() == gst::Fraction::new(1, 1)
                        && info.colorimetry().to_string() == "bt709";
                    if !ok {
                        off_shape += 1;
                    }
                    let lumas = sample.buffer_owned().and_then(|b| {
                        let f = gst_video::VideoFrame::from_buffer_readable(b, &info).ok()?;
                        let stride = f.plane_stride()[0] as usize;
                        let d = f.plane_data(0).ok()?;
                        let y = info.height() as usize / 2;
                        let at = |x: usize| d[y * stride + x];
                        Some([at(info.width() as usize / 10), at(info.width() as usize / 2)])
                    });
                    eprintln!(
                        "shape t_ms={} {:?} {}x{} par {} {} ok={ok} luma(edge,centre)={lumas:?} ring={}",
                        ms(now),
                        info.format(),
                        info.width(),
                        info.height(),
                        info.par(),
                        info.colorimetry(),
                        frames.audio_level()
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // 100 ms of the ring, read as interleaved S16LE stereo: the source's tone shows as its pitch.
    let n = (AUDIO_RATE / 10 * AUDIO_CHANNELS * 2) as usize;
    let pcm = frames.take_audio(n);
    let left: Vec<i16> = pcm
        .chunks_exact(4)
        .map(|f| i16::from_le_bytes([f[0], f[1]]))
        .collect();
    let crossings = left.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    eprintln!(
        "summary gens={gen} connects={connects} losses={losses} frames={frames_seen} shapes={shapes} \
         off_shape={off_shape} audio_samples={} pitch_hz={} log_leaks={} refused={} gate_dropped={} \
         frames_dropped={}",
        left.len(),
        crossings * 5,
        leaks.load(Ordering::SeqCst),
        input.refused_rebuilds(),
        input.gate_dropped(),
        input.frames_dropped()
    );
    input.stop();
    drop(input);
    let fail = connects == 0 || leaks.load(Ordering::SeqCst) > 0 || off_shape > 0;
    std::process::exit(i32::from(fail));
}
