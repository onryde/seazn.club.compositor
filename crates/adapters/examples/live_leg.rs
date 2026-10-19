//! C09's live legs (Owed 13 and 16; `_FINDINGS.md` F-011): the real adapter pushing slate ticks
//! at real pace to one RTMP(S) target, printing every `OutputProgress` total it reports.
//!
//! The target comes from the environment and is never printed: `C09_LIVE_URL` (the
//! `rtmp://host/app` part) and `C09_LIVE_KEY` (the stream key). `C09_LIVE_SECONDS` bounds the run
//! (default 20). Each sample is one stderr line, `progress t_ms=<ms since start> total=<bytes>`;
//! errors and the adapter's log lines go through its scrubber. A leg's verdict is read from them
//! by a person, so this is an example, not a test: it needs a live endpoint CI does not have.
use std::sync::Arc;
use std::time::{Duration, Instant};

use adapters::frame_store::FrameStore;
use adapters::gst_programme::GstProgramme;
use app::ports::{Inbound, ProgrammeOutput, TargetSpec};
use domain::config::OutputProfile;
use domain::programme::{FrameChoice, SlateKind};
use domain::supervisor::Event;

fn main() {
    let url = std::env::var("C09_LIVE_URL").unwrap_or_default();
    let key = std::env::var("C09_LIVE_KEY").unwrap_or_default();
    let seconds: u64 = std::env::var("C09_LIVE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    if url.is_empty() || key.is_empty() {
        eprintln!("live_leg: set C09_LIVE_URL and C09_LIVE_KEY");
        std::process::exit(2);
    }
    let profile = OutputProfile::P720;
    let (tx, rx) = crossbeam_channel::bounded(8_192);
    let mut p = GstProgramme::new(FrameStore::default(), tx).with_log(Arc::new(|l: &str| {
        eprintln!("log {l}");
    }));
    if let Err(e) = p.start(&profile) {
        eprintln!("live_leg: start: {e}");
        std::process::exit(1);
    }
    p.swap_sink(1, &TargetSpec { url, stream_key: key });
    let t0 = Instant::now();
    let step = Duration::from_nanos(1_000_000_000 / u64::from(profile.fps));
    let ticks = seconds * u64::from(profile.fps);
    let (mut samples, mut last) = (0u64, 0u64);
    for i in 0..ticks {
        if let Some(wait) = (t0 + step * i as u32).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        p.push(i, FrameChoice::Slate(SlateKind::Waiting));
        while let Ok(m) = rx.try_recv() {
            let Inbound::Event(e) = m else { continue };
            let t = t0.elapsed().as_millis();
            match e {
                Event::OutputProgress { total_bytes, .. } => {
                    samples += 1;
                    last = total_bytes;
                    eprintln!("progress t_ms={t} total={total_bytes}");
                }
                other => eprintln!("event t_ms={t} {other:?}"),
            }
        }
    }
    eprintln!("live_leg: {samples} progress samples, last total {last}");
    if samples == 0 {
        std::process::exit(1);
    }
}
