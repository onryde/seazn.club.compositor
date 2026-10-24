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
    **(Amended 2026-10-18, Owed 13: replaced by one `OutputProgress{gen, total_bytes}` from
    `out-bytes-total`; see "Amendment 2026-10-18".)**

`FrameStore` is shared state between pipelines, written by input (C10) and graphics (C11). It lives
in `crates/adapters/src/frame_store.rs`, and every slot is bounded.

## Files

- Create: `crates/adapters/src/{frame_store.rs,gst_programme.rs,gst_output.rs,slate.rs,stats.rs}`
- Modify: `crates/adapters/src/lib.rs` (`pub mod` for each new file, including `stats`)
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
  impl GstProgramme { pub fn new(frames: FrameStore, tx: crossbeam_channel::Sender<app::ports::Inbound>) -> Self; pub fn builds(&self) -> u32; pub fn overlay_pushes(&self) -> u64; pub fn stats(&self) -> std::sync::Arc<Stats>; }
  impl app::ports::ProgrammeOutput for GstProgramme { … }
  // stats.rs — the heartbeat's media numbers (C12 wires them; R1's panel health chips and C14/C15 read them)
  #[derive(Debug, Default)] pub struct Stats { /* atomics + a Mutex'd 1 s window; adapters may read Instant */ }
  #[derive(Debug, Clone, Copy, PartialEq, Default)]
  pub struct StatsSnapshot { pub fps: Option<f64>, pub bitrate_kbps: Option<u32>, pub egress_bytes: Option<u64>, pub encoder_speed: Option<f64> }
  impl Stats {
      pub fn snapshot(&self) -> StatsSnapshot;
      /// Test hook for C12's join test only: the next snapshot() returns `s`. Behind the `adapters`
      /// feature `test-hooks`, which only dev-dependencies enable. Release builds don't have it.
      #[cfg(feature = "test-hooks")] pub fn force_snapshot(&self, s: StatsSnapshot);
  }
  ```

  **Field definitions.** Each is a measurement, and `None` means "not measured yet", never 0:

  | Field | Definition | `None` until |
  |---|---|---|
  | ~~`fps`~~ `encoded_frames` (amended 2026-10-24, Owed 86) | ~~frames `push()`ed into the programme in the last complete 1 s window~~ video frames out of the encoder, this session (the beat's `fps` is C12's `BeatRate` over it) | ~~1 s of pushes~~ the first encoded frame |
  | `bitrate_kbps` | acked output bytes in the last complete 1 s window × 8 / 1000 (rtmp2sink `stats` poll, or the file size for `file://` targets) | the first ack poll on the current sink |
  | `egress_bytes` | acked output bytes, cumulative across every sink generation | the first ack |
  | `encoder_speed` | encoded video PTS advance in the last complete 1 s window ÷ wall time of that window (1.0 = real time; below 1 = the encoder is falling behind or starved) | 1 s of encoder output |

## Steps

- [ ] **Step 1: Pin element facts** before writing code, and record them in `_FINDINGS.md` as F-0xx.
  On Ubuntu 24.04 (CI image, or the C00 spike image), run:
  ```bash
  gst-inspect-1.0 rtmp2sink | sed -n '/stats/,/^$/p'          # name of the acked-bytes field
  gst-inspect-1.0 x264enc | grep -A3 speed-preset             # is it "changeable in PLAYING state"?
  gst-inspect-1.0 compositor | grep -E "latency|ignore-inactive|start-time-selection"
  ```
  ~~**Decision rule for `speed-preset`.** If it is *not* changeable while PLAYING, the `FastPreset` rung
  becomes "drop the overlay entirely" (`clear_overlay` and stop overlay pushes). Record that as a
  deviation **in the commit body and in `_FINDINGS.md`**. The programme pipeline is still never
  rebuilt.~~ **SUPERSEDED 2026-10-18:** no `FastPreset` rung exists, and there is no decision rule.
  See "Amendment 2026-10-18" below.

- [ ] **Step 2: Slate buffers.** ~~`slate.rs` renders one I420 frame per `SlateKind` at `start()`:~~
  - ~~pipeline: `videotestsrc pattern=solid-color foreground-color=0xff111827 num-buffers=1 ! video/x-raw,width=W,height=H ! textoverlay text="…" valignment=center halignment=center font-desc="Sans 36" ! videoconvert ! video/x-raw,format=I420 ! appsink`~~
  - ~~texts: `"Starting soon"` (Waiting) and `"Reconnecting…"` (Reconnecting). English-only here; the
    overlay page carries the localised score bug.~~
  - ~~The two buffers are cached.~~

  **SUPERSEDED 2026-10-15:** the slate is textless (house backdrop + Seazn logo), one buffer for every
  `SlateKind`. See "Amendment 2026-10-15 (overlay templates spec)" below the Audio section, which is
  untouched.

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
  #[test] fn stats_are_absent_before_anything_is_measured() { /* fresh GstProgramme: snapshot() == StatsSnapshot::default() (all None) */ }
  #[test] fn stats_fps_counts_pushes_per_second() { /* superseded 2026-10-24, Owed 86: stats_encoded_frames_are_the_frames_the_output_carries */ /* push Slate ticks at real 33 ms pace for 2.2 s; fps within 30 ± 1 (expected from OutputProfile::P720.fps) */ }
  #[test] fn stats_egress_and_bitrate_follow_acked_bytes() { /* file target, 3 s real-pace pushes, drain, wait OutputDrained;
       the drain path takes one FINAL stats sample after the sink's EOS (step 7), so egress_bytes == final file size exactly;
       bitrate_kbps within ±50 % of OutputProfile::P720.video_kbps (CBR filler keeps it near the target) */ }
  #[test] fn stats_encoder_speed_tracks_real_time_and_drops_when_starved() { /* 2 s real pace ⇒ speed in ENCODER_SPEED_REALTIME_MIN..=1.1; stop pushing 1.5 s ⇒ speed < 0.5;
       resume 2 s ⇒ back in ENCODER_SPEED_REALTIME_MIN..=1.1 (domain::config). This is C14 case 6's witness that the field can move. */ }
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
     reads the acked byte count and sends `OutputConnected{gen}` on the first acked bytes, then
     `OutputAck{gen}` whenever the count increases. The count is rtmp2sink `stats` (field name
     pinned in step 1) for `rtmp(s)://` targets, and the file's size on disk for `file://` targets,
     which is what `file_target_reports_connected_then_acks` exercises. The same poll feeds `Stats`.

  `drain()`:
  1. Send EOS on the programme appsrcs. The encoders flush into the ring.
  2. The **feeder thread**, on seeing the programme's EOS on its appsinks and the ring empty, calls
     `end_of_stream()` on **both output appsrcs**. The two pipelines are joined only by the ring, so
     without this step no EOS reaches the sink.
  3. On the output bus's `Eos`, take one **final** acked-bytes sample into `Stats` (so the last
     snapshot equals what the sink wrote), then send `OutputDrained`.

  `ninety_slate_ticks…` and the C12 fake-run both depend on this path.

- [ ] **Step 8: Run, expect a PASS.** `cargo test -p adapters --test programme_output -- --test-threads=1 --nocapture`.
  The tests share GStreamer init, so run them single-threaded.

- [ ] **Step 9: Mutate once each.**
  1. Rebuild the programme inside `swap_sink`. Expected red: `swap_sink_…without_rebuilding_programme`
     (`builds() == 1`).
  2. Remove the keyframe skip. Expected red: `first_is_key`.
  3. Drop `redact` from the error path. Expected red: `errors_and_logs_never_contain_the_stream_key`.
  4. Remove the feeder's `end_of_stream()` on the output appsrcs. Expected red: `ninety_slate_ticks…` (it never sees `OutputDrained`).
  5. Report `fps: Some(0.0)` before the first window. Expected red: `stats_are_absent_before_anything_is_measured`.
  6–7. **(Amendment 2026-10-15)** mutants 6–7 from the amendment section.

- [ ] **Step 10: CI.** The `check.yml` apt step is added, and the job is green on the PR.
  **Commit:** `feat(media): programme pipeline on its own clock and a swappable output (C09)`

## Acceptance

- All ~~14~~ 17 tests pass, single-threaded (amended 2026-10-15: plus the three slate tests).
- Every `Stats` field has a test that drives it, and starts as `None`.
- `builds() == 1` holds across swaps.
- The output starts on a keyframe, with PTS rebased.
- The stream key is never in errors.
- ~~5 mutants~~ ~~7 mutants~~ 9 mutants went red (amended 2026-10-15; 2026-10-18 adds `clear_overlay` and the sticky flag).
- `check.yml` is green with GStreamer installed.
- (Amended 2026-10-18, owner "no lite mode") `the_bottom_rung_keeps_the_overlay_on_air` passes. No
  rung clears the overlay, and the `clear_overlay` mutant went red.
- (Amended 2026-10-18, Owed 13) Progress is `OutputProgress` from `out-bytes-total` (the file size
  for `file://`), with `bitrate_kbps` and `egress_bytes` from the bytes-sent total. The grow/freeze
  test prints a non-zero count, and the live CF leg is recorded.
- (Amended 2026-10-18, Owed 15) The poll choice is recorded~~, and
  `stall_is_detected_within_budget_of_the_last_byte` holds the chosen bound with the measured gap
  printed~~ (amended 2026-10-24, Owed 87: the test is retired with the stall rule it timed).
- (Amended 2026-10-24, Owed 87) Every rtmp2sink is built with `timeout` = `OUTPUT_SINK_TIMEOUT_S`,
  read back from the element: `the_sink_timeout_is_rtmp2sinks_own_default`,
  `every_rtmp_sink_gets_the_domains_timeout` and
  `a_sink_timeout_other_than_the_default_reaches_every_later_sink` pass, and each mutant went red on
  CI: the set removed, the constant at 20 s, and another value handed to the programme.
- (Amended 2026-10-18, Owed 16) Both live legs, command bytes and trickle, are recorded with
  verdicts. A failing leg reopens C04, and the reopening is named in the PR.
- (Amended 2026-10-18, Owed 17) `hide_then_set_overlay_composites_again` passes, and the
  sticky-flag mutant went red.
- (Amended 2026-10-24, Owed 81 (i)) `camera_frames_replaced_counts_the_frames_no_tick_showed`
  passes: the camera slot's write count reaches `Stats` on every tick, a camera tick counts the
  frames written since the tick before it less the one it shows (a `Repeat` tick too: it is the
  camera on air), and a slate tick counts none.
- (Amended 2026-10-24, Owed 86) `StatsSnapshot::encoded_frames` replaces `fps`: every video buffer at
  the encoder's appsink counts one, `None` before the first, and `push` feeds `Stats` no count.
  `stats_encoded_frames_are_the_frames_the_output_carries` (90 paced ticks to a file decode to 90
  frames, and the count reads 90) replaces `stats_fps_counts_pushes_per_second`, and
  `the_beats_fps_falls_while_the_encoder_hangs_though_every_push_keeps_its_pace` (the PR #69 review's
  I1) hangs the encoder 2 s while every tick is pushed on time: a healthy window holds the band, the
  hung one reads under the floor, and after a whole window of real pace the next holds the band again;
  the push count (the frame counted at the tick) goes red on it.

## Audio (C00 audio A/B, 2026-10-15; F-004 verdict, overrides anything above)

**The digital-silence AAC track makes YouTube report "Poor"** ("not receiving enough video"), even
though video, egress, timing and transport are clean. Real audio in the same session, scene, encode
and route stayed Excellent for 10 min. voaacenc encodes digital zeros as ~95 % 0x00 bytes.

- **Default:** pass the input's real audio through, decoded and re-encoded to **AAC-LC 128 kbps,
  48 kHz, stereo**. This is the verified fix. The audio branch follows the input across rebuilds; on
  camera loss (slate / frozen frame), the programme continues the audio track with no gap in PTS.
- **No input audio, or camera lost:** **never** send digital-zero AAC. Use low-level noise
  (~−60 dBFS pink noise encodes normally, ~3.9 % zero bytes) as the PROVISIONAL filler. It is untested
  on YouTube; a 10 min direct YouTube test of noise filler, plus a no-audio-track variant, is owed
  before this task closes. Record the result in `_FINDINGS.md`.
- **Startup:** flvmux waits for audio caps, which costs 4–54 encoder drops in the first 10 s. Start the
  audio branch with the programme so caps exist before the first video buffer.
- **Acceptance additions:** the harness checker reports audio frames/s (≈46.9 at 48 kHz), audio kbps
  (≈128) and the **zero-byte share** of the AAC payload (fail if > 50 %), for the live-audio case and
  for the camera-lost case. Mutant: switch the filler to digital silence; the zero-byte check must go
  RED.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.2 (verbatim, `29fa9f59c`): "Fallback when the overlay renderer is dead (`graphics: failed`): the
compositor's native slate (C09) shows no text, only the house backdrop colour and the Seazn logo, so
the fallback never shows untranslated English." §6, C09 row: "The native slate becomes textless (house
backdrop + logo); it is used only when graphics have failed." **The Audio section above is unchanged and still
overrides**: the slate is a video layer only.

**What "used only when graphics have failed" means here.** In every slate phase (Waiting,
Reconnecting, and C06's new Final), the slate is the base layer and CEF's overlay is composited on top.
While the overlay page is alive, its scene (R2005: an opaque night backdrop + card) covers the slate
completely, so viewers never see the slate. They see it only when the overlay is hidden
(`HideOverlay`, graphics failed) or has not produced a frame yet. No branch decides "show the slate";
the composition order does. The tests below pin both halves.

**The slate:**
- **Backdrop:** `#150b36`, seazn.club's `--mk-night` (`apps/web/src/app/globals.css:535`, the stadium
  night that `--sport-board` resolves to at `:1175`). One constant, `SLATE_BACKDROP_RGB: [u8; 3] = [0x15, 0x0b, 0x36]`
  in `slate.rs`, with that doc line.
- **Logo:** vendor `seazn.club` `apps/web/public/logo-wide-night.png` byte-for-byte as
  `crates/adapters/assets/logo-wide-night.png` (800 × 280 RGBA: the cream "SEAZN CLUB" wordmark,
  the lime line and the red ball, **no tagline**; the print SVGs carry an English tagline and are
  **not** used). Its sha256 at pin is
  `bee2648f76fa2590ac81d2c1e14050fe4c62687b8630437b0d6e6b080c2f90e8`. Embed it with `include_bytes!`.
- **Geometry:** logo width = `W × 3 / 8`, height = width × 280 / 800, centred. That is 480 × 168 at
  720p and 720 × 252 at 1080p, both even, as I420 needs.
- **Pipeline** (built once at `start()`, one buffer, shared by every `SlateKind`):
  `videotestsrc pattern=solid-color foreground-color=0xff150b36 num-buffers=1 ! video/x-raw,format=AYUV,width=W,height=H ! compositor name=c sink_1::xpos=X sink_1::ypos=Y ! videoconvert ! video/x-raw,format=I420 ! appsink`
  with `appsrc (the PNG bytes, caps image/png) ! pngdec ! videoconvert ! videoscale ! video/x-raw,format=AYUV,width=LW,height=LH ! c.sink_1`.
  `pngdec` is in `gstreamer1.0-plugins-good`, which C12's image already installs.
- `SlateKind::ALL` (C06) lists every kind. `slate_for(kind)` returns the same `Arc` for each.
- **No `textoverlay` element anywhere in `slate.rs`.** The slate can never carry English.

**Tests** (add to `programme_output.rs`; write them failing first):
1. `slate_is_textless_house_backdrop_and_logo_for_every_kind`:
   - for each `SlateKind::ALL` entry, the buffer is the same `Arc` (pointer-equal)
   - decode it: pixels at (10, 10) and (W−10, H−10) equal `SLATE_BACKDROP_RGB` within ±4 per channel
     after I420 → RGB (BT.601, the expected value computed from the constant)
   - the centre `LW × LH` box has at least 10 % of its pixels more than 40 away from the backdrop on
     any channel (the logo is there)
   - the slate builder's element list (collect the factory names it creates) contains no
     `textoverlay`
   - print `slate kinds checked: 3`; fail on 0
2. `vendored_logo_matches_its_pinned_sha256`: hash `include_bytes!` and compare with the sha above.
   A re-vendor must change the pin on purpose.
3. `an_opaque_overlay_covers_the_slate_and_hiding_it_reveals_the_slate`:
   - `set_overlay` with a full-frame opaque BGRA in a colour that is not the backdrop; push
     `Slate(SlateKind::Reconnecting)`; the decoded centre pixel is the overlay's colour
   - `hide_overlay()`; push again; the centre pixel is now the logo region and a corner pixel is the
     backdrop

**Mutants:**
6. Restore a `textoverlay` for `SlateKind::Reconnecting`. Expected red: test 1 (the factory-name check,
   and the buffers stop being pointer-equal).
7. Backdrop back to `0xff111827`. Expected red: test 1's corner pixels.

## Amendment 2026-10-18 (owner ruling on the ladder, `docs/_INDEX.md` Owed 20)

Owner, 2026-10-18, in chat, relayed by the coordinator: "no lite mode; if CPU impacts, measure and
raise the CPU size". The score is never removed.

- **The `speed-preset` decision rule is void.** C05's `Rung` is `Full | OverlayHalfRate`, and
  `FastPreset` is deleted. F-001 already pinned that x264enc's `speed-preset` cannot change in
  PLAYING. Step 1 still records the `gst-inspect` line with the other element facts, but nothing
  branches on it, and no deviation is owed.
- **`step_down(r)` matches `Rung` exhaustively.** `OverlayHalfRate` is the half-rate overlay update
  above (`half_rate_rung_halves_overlay_pushes`). `Full` restores every-tick overlay updates: the
  domain never emits it, but the match is total, and a test pins it. **No rung ever calls
  `clear_overlay`, or stops overlay pushes:** only `hide_overlay()` (graphics Failed, C05) clears the
  slot.
- **New test:** `the_bottom_rung_keeps_the_overlay_on_air`. Call `set_overlay` (opaque blue
  top-left, as in `overlay_is_composited_on_top_and_hide_clears_it`), then
  `step_down(OverlayHalfRate)`, then push 0..60. The decoded frames at even and odd indices are all
  blue, and `overlay_pushes() == 30`.
- **New mutant:** `step_down(OverlayHalfRate)` also calls `clear_overlay`. Expected red: the new
  test.

### Owed checks as acceptance criteria (Owner, 2026-10-18, in chat, relayed by the coordinator: "apply rec 4-9")

**Owed 13: progress comes from `out-bytes-total`, never acked bytes.** The acked bytes stay 0 on CF
(F-005).
- The output poll reads rtmp2sink `out-bytes-total`: the sink's own count, from 0 per sink. For
  `file://` targets it reads the file's size. Each poll sends `OutputProgress { gen, total_bytes }`.
- `OutputConnected` and `OutputAck` are not sent. C04 replaced them.
- `Stats.bitrate_kbps` and `Stats.egress_bytes` are defined from the bytes-sent total, not from
  acked bytes. Rename `stats_egress_and_bitrate_follow_acked_bytes` to
  `stats_egress_and_bitrate_follow_bytes_sent`.
- New test `progress_total_grows_while_pushing_and_freezes_when_the_sink_stops`, on a `file://`
  target:
  - The totals sent while pushing strictly increase.
  - After a `drain()`, no total exceeds the last one.
  - Print the progress events read, and fail on 0.
- **Live leg (Step 1's real Input B, CF):** the same assertion against a real `rtmps://` sink.
  Record in `_FINDINGS.md` that `out-bytes-total` grows on a healthy push and stops growing when
  `netem out loss 100%` cuts it.

**Owed 15: the stall fires within its budget of the last real byte.** `last_progress` is the poll
that SAW growth, so a 1 s poll adds up to 1 s.
- Either poll `out-bytes-total` at `OUTPUT_PROGRESS_POLL_MS` ≤ 250 ms, keeping the PDF's 5 s, or
  keep 1 s and change the claim to ≤ `output_stall_after_ms + poll` (6 s). Then C14 case 5's bound
  and the PDF claim in spec §6 move with it.
- Choose one, record which in `_FINDINGS.md`, and cite it at the const.
- New test `stall_is_detected_within_budget_of_the_last_byte`:
  - Cut a `file://` sink by blocking its writes.
  - Measure from the last growing total to the `SwapSink` that the domain-in-the-loop harness
    returns.
  - Assert ≤ the chosen bound, and print the measured gap.

**Owed 16: what counts as progress is proved on a real sink.** Two live legs, recorded in
`_FINDINGS.md`:
- **(a) Command bytes.** Point the sink at an RTMP endpoint that accepts `connect` and `createStream`
  but refuses `publish` (mediamtx with a publish-denied path). Record whether `out-bytes-total` goes
  above 0.
  - If it does, the first nonzero total is not media. C04's Connected rule then needs a byte
    floor, so reopen C04 and do not merge C09's progress feed alone.
- **(b) Trickle.** Push through `netem out rate 8kbit`. Record the totals over 10 s.
  - If they keep growing, a trickling sink would stay Connected forever. C14 case 12 is then the
    red witness, and C04 reopens for a rate floor.
- Each leg prints its samples. Acceptance needs both recorded, with the verdict and the C04 routing.

**Owed 17: the hide is not sticky.** New test `hide_then_set_overlay_composites_again`:
- `set_overlay` (opaque blue), `hide_overlay()`, push: not blue.
- Then `set_overlay` (opaque blue) again, push: **blue**.
- `hide_overlay` clears the slot. It sets no flag that a later `set_overlay` would have to lift: no
  `show_overlay` port exists, so a sticky flag would keep C05's recoverable Failed off air forever.
- **Mutant:** make `hide_overlay` set a `hidden` flag that `set_overlay` does not clear. Expected
  red: the new test.

## Amendment 2026-10-19 (C08 PR #27 review round 1: a bounded `Drop`)

C08's `run()` returns after its final-beat flush, and the driver drops with every port as it returns,
so `GstProgramme`'s `Drop` runs on the exit path, after the final beat and before the process ends. It must
return within a bound: it never waits on a pipeline, a renderer or a thread that may hang (F-001:
`set_state(NULL)` did not return for 10 s on a hung renderer); teardown that may block goes to a
reaper thread, as `stop` does, or is bounded by a timeout. **Acceptance:** a test drops the adapter
while what it owns is hung (or stands in for hung) and the drop returns within 500 ms.

## Amendment 2026-10-24 (Owed 81 (i): the camera frames no tick showed)

The coordinator's recommendation, approved by the owner 2026-10-24 (in chat, relayed by the
coordinator: "apply your rec in both"): build a beat counter for camera frames that are replaced
before they are shown (`docs/_INDEX.md` Owed 81; no smoothing input buffer, Owed 89).
The camera slot counts the frames written to it, and `push` hands that count to
`Stats::on_camera_tick` on every tick, a slate tick included, so `StatsSnapshot::camera_frames_replaced`
holds the camera frames no camera tick put on air (`adapters::stats::ReplacedFrames` states the rule).
It is `None` until the first camera tick. The heartbeat does not carry it (the contract is strict,
Owed 90): the driver logs it once a beat. It counts the frames above one a camera tick, so a source
faster than the programme and a late tick thread raise it too (`ReplacedFrames` says how).

## Amendment 2026-10-24 (Owed 86: the honest beat)

The coordinator's recommendation, approved by the owner 2026-10-24 (in chat, relayed by the
coordinator: "Ok"): the beat reports the programme's REAL output frame rate, measured from the
frames actually put out over the beat window and never the nominal configured fps, and raises a
starvation alarm when it falls below a floor; the ladder does not change (`docs/_INDEX.md` Owed 86,
decision log "Owed 86: the honest beat", where the floor, the clear level and the minimum window are
recorded as the implementer's medium choices).
The old `fps` counted `push()` calls in the last complete 1 s window. The driver runs every late tick
back to back, so the count tracked the tick grid and read 29 to 31 while C14 case 6's output carried
11.5 and 24.7 fps (F-024). `StatsSnapshot::fps` is gone: `Stats::on_encoded_video` (the seam
`encoder_speed` reads) also counts the frame into `encoded_frames`, and `push` no longer calls
`on_push` (removed). The rate is not the stats' job: C12's `ProcMetrics` turns two samples' totals
into the beat's `fps` (`adapters::stats::BeatRate`, C12's amendment of this date). The ladder's input
is unchanged: one `EncoderOverrun` per `overrun`.

## Amendment 2026-10-24 (Owed 87: rtmp2sink's timeout is set, not inherited)

Owner, 2026-10-24, in chat, relayed by the coordinator: "A": the stall rule is retired, and rtmp2sink's own
socket timeout is the only detector of a destination stall (`docs/_INDEX.md` Owed 87; C04's
amendment of this date).
- `gst_output.rs` sets every rtmp2sink's `timeout` (whole seconds) to `OUTPUT_SINK_TIMEOUT_S`, from
  `domain::config::OUTPUT_SINK_TIMEOUT_MS`. That is today's default (GStreamer 1.24.2,
  `gst/rtmp2/gstrtmp2locationhandler.c:35`, `#define DEFAULT_TIMEOUT 5`), so a GStreamer whose
  default moves does not move the detector. A const assert keeps it whole seconds and in the
  property's range.
- Owed 15's `stall_is_detected_within_budget_of_the_last_byte` and Owed 71's
  `the_stall_bound_still_refuses_a_late_detection` are retired with the rule they timed.
  `OUTPUT_PROGRESS_POLL_MS` (250 ms) stays: it times the Connected proof and the bitrate, and its pin
  is `the_progress_poll_is_owed_15s_choice`.
- Tests: the element's default equals the constant; every RTMP sink of a swap reads it back, and a
  filesink has none; a value other than the default, set through the `test-hooks` seam
  `GstProgramme::set_output_sink_timeout_s`, reads back from every sink built after it and never from
  one built before. Only that last test sees the set, since the production value equals the default.

## Do not touch

Domain and app crates. If you find a port signature problem, report it as a deviation and don't
change the port. (Amended 2026-10-15) The Audio section above: this amendment is video only.
(Amended 2026-10-24, Owed 87) The sink timeout's value lives in `domain::config`, by the owner's
ruling; the domain's side of that change is C04's amendment.

## Output cap

Under 15 lines: counts, pinned element facts (F-ids), deviations (especially the `speed-preset` rule).
