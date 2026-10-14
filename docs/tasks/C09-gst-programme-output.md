# C09: Programme pipeline (never rebuilt) and output pipeline (swappable)

**Repo:** `seazn.club.compositor`. **Depends on:** C08, and **C00 PASS** (read `_FINDINGS.md` F-001,
F-004, F-005 first).

## Context

Spec §3.3's refinement splits the media path into four pipelines in one process. This task builds two
of them:

- **Programme:** `appsrc(base) + appsrc(overlay) → compositor → videoconvert → x264enc → h264parse → appsink(venc)`,
  plus `appsrc(audio) → audioconvert → voaacenc → aacparse → appsink(aenc)`.
  - Built once in `start()`. **Never rebuilt.**
  - Buffers are timestamped **by tick index**: `pts = index × period`.
  - `push(i, choice)` picks the base buffer as follows:

    | Choice | Base buffer |
    |---|---|
    | `Emit` / `Repeat` | latest camera frame from `FrameStore` |
    | `Slate(kind)` | pre-rendered slate buffer |

  - The overlay buffer is the latest CEF frame, or a transparent frame if the overlay is hidden or
    has never been ready. On rung `OverlayHalfRate`, the overlay buffer is updated only on even
    indices; the compositor reuses the previous one.
  - Audio: take one period of S16LE 48 kHz stereo from the bounded 400 ms ring and pad with silence on
    underrun.
- **Output:** `appsrc(h264) + appsrc(aac) → flvmux streamable=true → rtmp2sink` (or `filesink` for a
  `file://` target).
  - Rebuilt on every `swap_sink(gen, target)`.
  - Fed by a feeder thread from a **bounded 2 s encoded ring** (drop oldest).
  - The feeder starts each new output at a keyframe, and asks the encoder for one via a
    force-key-unit event on swap. It rebases PTS to 0 per connection.
  - Bus `Error` → `Event::OutputError{gen}`. The first acked bytes → `OutputConnected{gen}`. Acked
    bytes increasing (1 s poll) → `OutputAck{gen}`. EOS after `drain()` → `OutputDrained`.

`FrameStore` is shared state between pipelines, written by input (C10) and graphics (C11). It lives
in `crates/adapters/src/frame_store.rs`, and every slot is bounded.

## Files

- Create: `crates/adapters/src/{frame_store.rs,gst_programme.rs,gst_output.rs,slate.rs}`
- Modify: `crates/adapters/Cargo.toml`, adding:
  - `gstreamer = { version = "0.24", features = ["v1_24"] }`
  - `gstreamer-app = { version = "0.24", features = ["v1_24"] }`
  - `gstreamer-video = { version = "0.24", features = ["v1_24"] }`
  - `gstreamer-audio = { version = "0.24", features = ["v1_24"] }`
  - `crossbeam-channel = "0.5"`
- Modify: `.github/workflows/check.yml`. Before the cargo steps, add:
  ```yaml
  - run: sudo apt-get update && sudo apt-get install -y --no-install-recommends libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav
  ```
- Test: `crates/adapters/tests/programme_output.rs`

## Interfaces

- **Consumes:** `app::ports::{ProgrammeOutput, TargetSpec}`, `app::ports::Inbound`,
  `domain::{programme::{FrameChoice, SlateKind}, ladder::Rung, config::OutputProfile, supervisor::Event}`,
  `redact`.
- **Produces:**
  ```rust
  // frame_store.rs
  #[derive(Clone, Default)] pub struct FrameStore { /* Arc<Mutex<…>> slots */ }
  impl FrameStore {
      pub fn set_camera(&self, s: gst::Sample);   pub fn camera(&self) -> Option<gst::Sample>;
      pub fn set_overlay(&self, s: gst::Sample);  pub fn overlay(&self) -> Option<gst::Sample>; pub fn clear_overlay(&self);
      pub fn push_audio(&self, bytes: &[u8]);     // ring capped at 400 ms of S16LE 48k stereo (76_800 bytes); drops oldest
      pub fn take_audio(&self, n: usize) -> Vec<u8>; // exactly n bytes, silence-padded
  }
  // gst_programme.rs
  pub struct GstProgramme { /* … */ }
  impl GstProgramme { pub fn new(frames: FrameStore, tx: crossbeam_channel::Sender<app::ports::Inbound>) -> Self; pub fn builds(&self) -> u32; pub fn overlay_pushes(&self) -> u64; }
  impl app::ports::ProgrammeOutput for GstProgramme { … }
  ```

## Steps

- [ ] **Step 1: Pin element facts** before writing code, and record them in `_FINDINGS.md` as F-0xx.
  On Ubuntu 24.04 (CI image, or the C00 spike image), run:
  ```bash
  gst-inspect-1.0 rtmp2sink | sed -n '/stats/,/^$/p'          # name of the acked-bytes field
  gst-inspect-1.0 x264enc | grep -A3 speed-preset             # is it "changeable in PLAYING state"?
  gst-inspect-1.0 compositor | grep -E "latency|ignore-inactive|start-time-selection"
  ```
  **Decision rule for `speed-preset`.** If it is *not* changeable while PLAYING, the `FastPreset` rung
  becomes "drop the overlay entirely" (`clear_overlay` and stop overlay pushes). Record that as a
  deviation **in the commit body and in `_FINDINGS.md`**. The programme pipeline is still never
  rebuilt.

- [ ] **Step 2: Slate buffers.** `slate.rs` renders one I420 frame per `SlateKind` at `start()`:
  - pipeline: `videotestsrc pattern=solid-color foreground-color=0xff111827 num-buffers=1 ! video/x-raw,width=W,height=H ! textoverlay text="…" valignment=center halignment=center font-desc="Sans 36" ! videoconvert ! video/x-raw,format=I420 ! appsink`
  - texts: `"Starting soon"` (Waiting) and `"Reconnecting…"` (Reconnecting). English-only here; the
    overlay page carries the localised score bug.
  - The two buffers are cached.

- [ ] **Step 3: Write the failing tests.** `crates/adapters/tests/programme_output.rs`. Each test uses
  a `file://` target in a temp dir and decodes the result with a test helper that runs
  `filesrc ! flvdemux ! … ! appsink` and collects video PTS, the keyframe flag on the first buffer, and
  the audio duration.
  ```rust
  // Helper signatures to write in the test file:
  // fn decode(path: &Path) -> Decoded { video_pts_ms: Vec<u64>, first_is_key: bool, audio_ms: u64, frames: Vec<gst_video::VideoFrame<Readable>> /* sampled */ }
  // fn programme(dir: &Path) -> (GstProgramme, crossbeam_channel::Receiver<Inbound>, FrameStore)

  #[test] fn ninety_slate_ticks_give_ninety_frames_at_constant_spacing() { /* push 0..90 Slate(Waiting); drain; wait OutputDrained; decode;
       assert 90 frames; every PTS delta == round(1000/30) ±1 ms; audio_ms within 3000 ± 40 (A/V ±40 ms, PDF rank 4) */ }
  #[test] fn emit_uses_the_latest_camera_frame() { /* set_camera(red I420 1280x720); push Emit; decode; centre pixel is red (YUV→RGB tolerance) */ }
  #[test] fn camera_of_a_different_size_is_letterboxed_not_renegotiated() { /* (Review Focus 1) set_camera(640x360) then (1920x1080);
       pushes keep succeeding; decoded caps stay 1280x720 for every frame; builds() == 1 */ }
  #[test] fn overlay_is_composited_on_top_and_hide_clears_it() { /* set_overlay(BGRA with opaque blue top-left 100x100); push Slate; pixel blue;
       hide_overlay(); push; pixel NOT blue */ }
  #[test] fn half_rate_rung_halves_overlay_pushes() { /* step_down(OverlayHalfRate); push 0..60; overlay_pushes() == 30 */ }
  #[test] fn swap_sink_starts_new_file_at_keyframe_without_rebuilding_programme() { /* swap(1,a.flv); push 0..60; swap(2,b.flv); push 60..120; drain;
       decode b: first_is_key; PTS of b starts at 0; builds() == 1 */ }
  #[test] fn unreachable_rtmp_target_reports_output_error_with_its_generation() { /* swap(3, rtmp://127.0.0.1:9/live/x); recv OutputError{gen:3} within 6 s;
       pushes after the error do not panic or block > 1 period (time 30 pushes < 1.5 s) */ }
  #[test] fn errors_and_logs_never_contain_the_stream_key() { /* target rtmp://127.0.0.1:9/live/SECRETKEY; capture the error text from the event path /
       the adapter's log sink; assert !contains("SECRETKEY") */ }
  #[test] fn file_target_reports_connected_then_acks() { /* first buffer ⇒ OutputConnected{gen}; subsequent ⇒ OutputAck{gen} */ }
  #[test] fn audio_ring_is_bounded_at_400ms_and_pads_silence() { /* unit on FrameStore: push 1 s of audio; take 1 s; first 600 ms worth is silence (zeros) */ }
  ```
  Write each body in full. The comments state exactly what to assert.

- [ ] **Step 4: Run, expect a FAIL.** `cargo test -p adapters --test programme_output`

- [ ] **Step 5: Implement `frame_store.rs`.** Use `Arc<Mutex<Option<gst::Sample>>>` per video slot,
  and an `Arc<Mutex<VecDeque<u8>>>` audio ring capped at `76_800` bytes, dropping from the front.

- [ ] **Step 6: Implement `gst_programme.rs`.** Build with explicit element construction
  (`gst::ElementFactory::make`), not `parse::launch`, so that every element handle is kept.
  - **base appsrc:** `is-live=true`, `format=time`, `do-timestamp=false`, `max-bytes` = 3 frames,
    `block=false`, `leaky-type=downstream` (GStreamer 1.20+; if absent, drop in `push` when
    `current-level-buffers > 3`). Caps: `video/x-raw,format=I420,width=W,height=H,framerate=fps/1`.
  - **overlay appsrc:** caps `video/x-raw,format=BGRA,width=1920,height=1080,framerate=fps/1`. The
    compositor sink pad has `width=W height=H`, so the compositor scales it; set `zorder=1`. Base pad
    `zorder=0`.
  - **x264enc:** `tune=zerolatency speed-preset=veryfast bitrate=video_kbps key-int-max=fps*2 bframes=0 vbv-buf-capacity=1000 option-string="nal-hrd=cbr:filler=1"`
    (POC values; `bframes=0` per F-004). Caps after: `video/x-h264,profile=main`, then
    `h264parse config-interval=-1`.
  - **venc/aenc appsinks:** `sync=false`, with `emit-signals` or `set_callbacks`.
    `new_sample` → push into the encoded ring (`VecDeque<(gst::Sample, is_video)>`, capped at 2 s by
    PTS span).
  - **queue before x264enc:** `leaky=downstream max-size-buffers=fps max-size-bytes=0 max-size-time=0`.
    Connect its `overrun` signal to `tx.send(Inbound::Event(Event::EncoderOverrun))`. That's the
    "drop before the encoder" rule.
  - **`push(i, choice)`:**
    - `pts = i * period_ns`, `duration = period_ns`
    - base = camera or slate
    - **copy the buffer metadata** onto a new buffer that references the same memory
      (`gst::Buffer::copy` is shallow), and set the PTS
    - push base and overlay (or the transparent buffer), and audio (`take_audio(period_bytes)`)
    - never block: `appsrc.push_buffer` errors are logged, redacted and counted, and never panic
  - **`start(profile)`:** build, set PLAYING, increment `builds`. A second call returns
    `Err("programme already started")`, because the pipeline is never rebuilt.
  - **Camera size change** (review focus 1): the base appsrc caps are fixed at W×H. The input pipeline
    (C10) owns scaling, so the programme never sees another size. For this test, the programme
    adapter must also defend itself: if a camera sample's caps differ from W×H, scale it through a
    cached `videoscale` helper (a tiny dedicated pipeline, or `gst_video::VideoConverter`) before
    pushing. That way the programme caps never renegotiate.

- [ ] **Step 7: Implement `gst_output.rs`.** `swap_sink(gen, target)`:
  1. Move the old output pipeline to a reaper thread that sets it to NULL. Never NULL it on the
     driver thread, because that can block.
  2. Build the new one: `appsrc(h264 avc) + appsrc(aac) → flvmux → rtmp2sink location=<url>/<key>`,
     or `filesink`.
  3. Send `gst_video::UpstreamForceKeyUnitEvent` to the programme's `h264parse` src pad.
  4. The feeder thread skips ring entries until the first video keyframe, rebases PTS, and pushes.
  5. A bus watch sends `OutputError{gen}`, which carries the message text after `redact`. A 1 s timer
     reads rtmp2sink `stats` (field name pinned in step 1) and sends `OutputConnected{gen}` on the
     first acked bytes, then `OutputAck{gen}` whenever the count increases.

  `drain()`: EOS on the programme appsrcs. When the encoders flush through the ring, the output's EOS
  reaches the sink, and the bus EOS sends `OutputDrained`.

- [ ] **Step 8: Run, expect a PASS.** `cargo test -p adapters --test programme_output -- --test-threads=1 --nocapture`.
  The tests share GStreamer init, so run them single-threaded.

- [ ] **Step 9: Mutate once each.**
  1. Rebuild the programme inside `swap_sink`. Expected red: `swap_sink_…without_rebuilding_programme`
     (`builds() == 1`).
  2. Remove the keyframe skip. Expected red: `first_is_key`.
  3. Drop `redact` from the error path. Expected red: `errors_and_logs_never_contain_the_stream_key`.

- [ ] **Step 10: CI.** The `check.yml` apt step is added, and the job is green on the PR.
  **Commit:** `feat(media): programme pipeline on its own clock and a swappable output (C09)`

## Acceptance

- All 10 tests pass, single-threaded.
- `builds() == 1` holds across swaps.
- The output starts on a keyframe, with PTS rebased.
- The stream key is never in errors.
- 3 mutants went red.
- `check.yml` is green with GStreamer installed.

## Do not touch

Domain and app crates. If you find a port signature problem, report it as a deviation and don't
change the port.

## Output cap

Under 15 lines: counts, pinned element facts (F-ids), deviations (especially the `speed-preset` rule).
