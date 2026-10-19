//! The Machine's native slate (C09, amended 2026-10-15 by the overlay templates spec §4.2 and §6):
//! textless, the house backdrop and the Seazn logo, one buffer for every `SlateKind`. It is the base
//! layer in every slate phase, under CEF's overlay; viewers see it only while the overlay is hidden
//! (graphics failed) or has not painted yet, so the fallback never shows untranslated English. No
//! branch decides "show the slate": the composition order does.
//!
//! **No text element is ever built here** (the slate can never carry English). `factories()` lists
//! every element factory the builder asked for, so a test can hold that to its word.
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use domain::programme::SlateKind;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// `#150b36`, seazn.club's `--mk-night` (`apps/web/src/app/globals.css:535`), the stadium night that
/// `--sport-board` resolves to at `:1175`.
pub const SLATE_BACKDROP_RGB: [u8; 3] = [0x15, 0x0b, 0x36];

/// seazn.club `apps/web/public/logo-wide-night.png`, vendored byte for byte: 800 × 280 RGBA, the cream
/// "SEAZN CLUB" wordmark, the lime line and the red ball, and no tagline (the print SVGs carry an
/// English one and are not used). `tests/programme_output.rs` pins its sha256; a re-vendor changes
/// the pin on purpose.
pub const LOGO_PNG: &[u8] = include_bytes!("../assets/logo-wide-night.png");
/// The vendored logo's own size (its PNG header).
pub const LOGO_SIZE: (u32, u32) = (800, 280);

/// The bound on rendering the one slate frame at `start()`: a local still image, which takes well
/// under a second; a pipeline that has not produced it by then never will.
const RENDER_TIMEOUT: Duration = Duration::from_secs(10);

/// The slate's logo box for a `width` × `height` frame: `(x, y, logo width, logo height)`. The logo is
/// `width × 3 / 8` wide, keeps its 800:280 aspect, and is centred (480 × 168 at 720p, 720 × 252 at
/// 1080p: even, as I420 needs).
pub fn logo_box(width: u32, height: u32) -> (u32, u32, u32, u32) {
    let lw = width * 3 / 8;
    let lh = lw * LOGO_SIZE.1 / LOGO_SIZE.0;
    ((width - lw) / 2, (height - lh) / 2, lw, lh)
}

/// The rendered slate: one I420 buffer, shared by every kind.
pub struct Slate {
    sample: Arc<gst::Sample>,
    factories: Vec<&'static str>,
}

impl Slate {
    /// Renders the slate for a `width` × `height` I420 programme, once.
    pub fn render(width: u32, height: u32) -> Result<Slate, String> {
        let mut factories = Vec::new();
        let sample = render(width, height, &mut factories)?;
        Ok(Slate {
            sample: Arc::new(sample),
            factories,
        })
    }

    /// The slate for `kind`: the same buffer for every kind.
    pub fn slate_for(&self, kind: SlateKind) -> Arc<gst::Sample> {
        match kind {
            SlateKind::Waiting | SlateKind::Reconnecting | SlateKind::Final => Arc::clone(&self.sample),
        }
    }

    /// Every element factory the builder asked for, in order.
    pub fn factories(&self) -> &[&'static str] {
        &self.factories
    }
}

impl fmt::Debug for Slate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slate")
            .field("factories", &self.factories)
            .finish()
    }
}

fn make(factories: &mut Vec<&'static str>, name: &'static str) -> Result<gst::Element, String> {
    factories.push(name);
    gst::ElementFactory::make(name)
        .build()
        .map_err(|e| format!("slate: no {name} element: {e}"))
}

fn render(width: u32, height: u32, factories: &mut Vec<&'static str>) -> Result<gst::Sample, String> {
    let (x, y, lw, lh) = logo_box(width, height);
    let [r, g, b] = SLATE_BACKDROP_RGB;
    let argb = 0xff00_0000u32 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);

    let pipeline = gst::Pipeline::new();
    let backdrop = make(factories, "videotestsrc")?;
    backdrop.set_property_from_str("pattern", "solid-color");
    backdrop.set_property("foreground-color", argb);
    backdrop.set_property("num-buffers", 1i32);
    let backdrop_caps = make(factories, "capsfilter")?;
    backdrop_caps.set_property(
        "caps",
        gst::Caps::builder("video/x-raw")
            .field("format", "AYUV")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("colorimetry", "bt709")
            .build(),
    );
    let mixer = make(factories, "compositor")?;
    let convert = make(factories, "videoconvert")?;
    let out_caps = make(factories, "capsfilter")?;
    out_caps.set_property(
        "caps",
        // BT.709, pinned rather than left to the size-based default (HD is BT.709 either way): the
        // programme's base caps are these caps, and x264enc signals them in the stream.
        gst::Caps::builder("video/x-raw")
            .field("format", "I420")
            .field("colorimetry", "bt709")
            .build(),
    );
    let sink = make(factories, "appsink")?;

    let png = make(factories, "appsrc")?;
    let pngdec = make(factories, "pngdec")?;
    let logo_convert = make(factories, "videoconvert")?;
    let logo_scale = make(factories, "videoscale")?;
    let logo_caps = make(factories, "capsfilter")?;
    logo_caps.set_property(
        "caps",
        gst::Caps::builder("video/x-raw")
            .field("format", "AYUV")
            .field("width", lw as i32)
            .field("height", lh as i32)
            // A logo-sized frame defaults to BT.601 (an SD size), which the compositor then mixed
            // into: Y 29 / U 144 / V 129 for the backdrop instead of 30 / 146 / 131 (F-010).
            .field("colorimetry", "bt709")
            .build(),
    );

    pipeline
        .add_many([
            &backdrop,
            &backdrop_caps,
            &mixer,
            &convert,
            &out_caps,
            &sink,
            &png,
            &pngdec,
            &logo_convert,
            &logo_scale,
            &logo_caps,
        ])
        .map_err(|e| format!("slate: {e}"))?;
    gst::Element::link_many([&backdrop, &backdrop_caps, &mixer, &convert, &out_caps, &sink])
        .map_err(|e| format!("slate: {e}"))?;
    gst::Element::link_many([&png, &pngdec, &logo_convert, &logo_scale, &logo_caps])
        .map_err(|e| format!("slate: {e}"))?;
    let logo_pad = mixer
        .request_pad_simple("sink_%u")
        .ok_or("slate: compositor gave no sink pad")?;
    logo_pad.set_property("xpos", x as i32);
    logo_pad.set_property("ypos", y as i32);
    // Pad request order is not the z order (spike trap): the backdrop's pad is 0, the logo's 1.
    logo_pad.set_property("zorder", 1u32);
    if let Some(backdrop_pad) = mixer.static_pad("sink_0") {
        backdrop_pad.set_property("zorder", 0u32);
    }
    logo_caps
        .static_pad("src")
        .ok_or("slate: no logo src pad")?
        .link(&logo_pad)
        .map_err(|e| format!("slate: logo link: {e:?}"))?;

    let png = png
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| "slate: appsrc is not an AppSrc")?;
    png.set_caps(Some(&gst::Caps::builder("image/png").build()));
    png.set_format(gst::Format::Time);
    let sink = sink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "slate: appsink is not an AppSink")?;
    sink.set_property("sync", false);

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("slate: pipeline did not start: {e}"))?;
    let mut logo = gst::Buffer::from_slice(LOGO_PNG);
    logo.get_mut()
        .ok_or("slate: logo buffer not writable")?
        .set_pts(gst::ClockTime::ZERO);
    let pushed = png.push_buffer(logo).and_then(|_| png.end_of_stream());
    let sample = pushed
        .map_err(|e| format!("slate: logo push: {e:?}"))
        .and_then(|_| {
            sink.try_pull_sample(gst::ClockTime::from_nseconds(RENDER_TIMEOUT.as_nanos() as u64))
                .ok_or_else(|| "slate: no frame rendered".to_owned())
        });
    // A local still pipeline: it reaches NULL at once (nothing here can hang the way a renderer can).
    let _ = pipeline.set_state(gst::State::Null);
    sample
}
