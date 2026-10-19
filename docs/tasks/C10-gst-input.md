# C10: Input pipeline (rebuildable)

**Repo:** `seazn.club.compositor`. **Depends on:** C09 (`FrameStore`). Read `_FINDINGS.md` F-002
first: it covers single-reader behaviour, bursts and reconnect.

## Context

```
srtsrc uri=<built from InputSpec> → tsdemux ┬→ h264parse → avdec_h264 → videoconvertscale add-borders=true
                                            │     → capsfilter video/x-raw,format=I420,width=W,height=H,pixel-aspect-ratio=1/1 → appsink(video)
                                            └→ aacparse → avdec_aac → audioconvert → audioresample
                                                  → capsfilter audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved → appsink(audio)
```

- `rebuild(gen, spec)` tears down the previous input pipeline on a reaper thread and builds a new one
  tagged with `gen`.
- **Scaling to W×H with letterboxing happens here**, so the programme's caps never change. This is
  review focus 1: a phone rotated mid-match, or a CF transcode change.

**Events** (each carries `gen`):

| Trigger | Event |
|---|---|
| First video sample | `InputConnected{gen}` |
| Every video sample | `FrameStore::set_camera(sample)`, then `InputFrame` |
| Every audio sample | `FrameStore::push_audio(bytes)` |
| Bus `Error` | `InputLost{gen, Error}` |
| Bus `EOS` (CF closed the reader) | `InputLost{gen, Eos}` |

The domain owns stall detection (C04 `check_stall`), so the adapter has **no watchdog of its own**.
One source of truth.

**SRT URI.** Build it as
`srt://{host:port}?mode=caller&streamid={stream_id}&passphrase={passphrase}&latency={ms}`. Three rules:
- URL-encode `streamid` and `passphrase`.
- **Omit the `passphrase` key entirely when it's empty.** libsrt rejects a present passphrase shorter
  than 10 characters, and the test listener runs unencrypted.
- **Never log it.** Only `redact()` output reaches logs or events. GStreamer error messages can echo
  the URI, so run every bus message text through `redact` before logging. This is a memory trap:
  secrets in a URL leak through parse errors.

`latency` comes from `config.rs` as a new `Thresholds` field `input_srt_latency_ms`. The default is
1000 (the POC's `INPUT_LATENCY_MS`), re-pinned by F-002. Add it in this task with a doc line. That
edits `domain/src/config.rs`, and is allowed **only** for this field.

## Files

- Create: `crates/adapters/src/gst_input.rs`
- Modify: `crates/domain/src/config.rs` (the one field)
- Test: `crates/adapters/tests/input.rs`

## Interfaces

- **Consumes:** `app::ports::{InputSource, InputSpec, Inbound}`, `FrameStore`,
  `domain::{supervisor::Event, input_link::LossReason}`, `redact`.
- **Produces:**
  `pub struct GstInput; impl GstInput { pub fn new(frames: FrameStore, tx: Sender<Inbound>, latency_ms: u64, out_w: u32, out_h: u32) -> Self; pub fn srt_uri(spec: &InputSpec, latency_ms: u64) -> String; }`
  and `impl InputSource for GstInput`.

## Steps

- [ ] **Step 1: Write the failing tests.** Every test feeds a local SRT **listener** started in the
  test from a GStreamer source:
  `videotestsrc is-live=true ! x264enc tune=zerolatency key-int-max=30 ! mpegtsmux ! srtsink uri=srt://127.0.0.1:<port>?mode=listener`,
  plus an `audiotestsrc` branch. `InputSpec.url` is `srt://127.0.0.1:<port>`, with an empty
  passphrase for tests. Write a `listener(w, h)` helper in the test file.

  | Test | What it asserts |
  |---|---|
  | `connects_and_delivers_frames_with_its_generation` | `rebuild(1, spec)` → `InputConnected{gen:1}` within 5 s; ≥ 25 `InputFrame` events in the following second; `FrameStore::camera()` caps are W×H I420 |
  | `input_caps_change_does_not_reach_programme` | feed 640×360, then restart the listener at 1920×1080 and `rebuild(2)`; camera samples are W×H in both phases |
  | `listener_closing_reports_eos_loss_with_the_right_generation` | stop the listener → `InputLost{gen, Eos or Error}`; record which one F-002 says CF produces, and assert that one |
  | `rebuild_tears_down_the_old_pipeline` | `rebuild(1)`, then `rebuild(2)`; no `InputFrame` arrives tagged with an old pipeline (frames carry no gen, so assert that after `rebuild(2)` the old pipeline's appsink callback is detached: a `generation_of_last_frame()` test accessor equals 2) |
  | `srt_uri_encodes_and_never_appears_in_logs` | `srt_uri` for a passphrase with `&` and `=` is percent-encoded; an empty passphrase leaves no `passphrase` key in the URI; drive a failing connect to `127.0.0.1:9` and assert the logged and evented error text contains no passphrase |
  | `audio_reaches_the_ring` | after 1 s connected, `FrameStore::take_audio(6400)` is not all zeros |

- [ ] **Step 2: Run, expect a FAIL.**
  `cargo test -p adapters --test input -- --test-threads=1`

- [ ] **Step 3: Implement it.**
  - `tsdemux` pads are dynamic, so link in `pad-added` by caps name (`video/x-h264`, `audio/mpeg`).
  - Every queue between demux and decode is `leaky=downstream`, with `max-size-time` = 1 s (video)
    and 400 ms (audio). These are the plan's bounded-queue values.
  - The appsinks use `max-buffers=2 drop=true sync=false`.
  - `stop()` sets the pipeline to NULL on the reaper thread.

- [ ] **Step 4: Run, expect a PASS.** Paste the counts.

- [ ] **Step 5: Mutate once each.**
  1. Remove `add-borders=true` and the fixed caps. Expected red: `input_caps_change_does_not_reach_programme`.
  2. Log the raw URI. Expected red: `srt_uri_encodes_and_never_appears_in_logs`.

- [ ] **Step 6: Commit.** `feat(media): rebuildable SRT input with letterboxed fixed caps (C10)`

## Acceptance

- 6 tests pass.
- 2 mutants went red.
- The new `config.rs` field cites its source.
- (Amended 2026-10-19, as built) `tests/input.rs` passes 25 tests: the six above (test 3 asserting `Error`), the dispatch's sequences (loss and recovery, two reconnects, a source that never answers, a caps and frame-rate change mid-stream and across rebuilds, EOS against error, a full channel) and the bounded `Drop`. Every guard's mutant is red or argued equivalent (`_INDEX.md`, the session handoff for PR #30).

## Amendment 2026-10-19 (C08 PR #27 review round 1: a bounded `Drop`)

C08's `run()` returns after its final-beat flush, and the driver drops with every port as it returns,
so `GstInput`'s `Drop` runs on the exit path, after the final beat and before the process ends. It must
return within a bound: it never waits on a pipeline, a renderer or a thread that may hang (F-001:
`set_state(NULL)` did not return for 10 s on a hung renderer); teardown that may block goes to a
reaper thread, as `stop` does, or is bounded by a timeout. **Acceptance:** a test drops the adapter
while what it owns is hung (or stands in for hung) and the drop returns within 500 ms.

## Amendment 2026-10-19 (as built, PR #30; F-012)

- **Test 3 asserts `Error`, and is named for it** (`listener_closing_reports_an_error_loss_with_the_right_generation`). `srtsrc` posts an ERROR when its peer closes, never an EOS, and F-002 recorded no close from CF at all (a silent socket; the C10 leg saw the same). The Events table's `Eos` row stays mapped, from the video appsink, but nothing produces it (Owed 46).
- **The listener** is `… ! mpegtsmux alignment=7 ! srtsink …`. At the default alignment no keyframe reaches the caller, and nothing decodes. It sets `srtsink` `authentication=true` too, so that `caller-connecting` reports the stream id the adapter presented.
- **Test 4's witness** holds generation 2's start, so only the retired pipeline is delivering. It then reads the camera slot's PTS and the audio ring. `generation_of_last_frame() == 2` alone stayed green with the video gate removed.
- **Test 5** checks log lines and `Debug`; events carry no text. The values are planted raw, percent-encoded and inside the URI, each as its own error, because `redact` masks everything after a `?`.
- **Step 3 additions:** `srtsrc` `auto-reconnect=false`; the camera caps also pin `colorimetry=bt709`; both appsinks set `async=false`.
- **Step 5, mutant 2** is the raw URI written past the scrubber. Through the adapter's log path the scrubber masks it, and it stays green.

## Do not touch

`gst_programme.rs`/`gst_output.rs` (report problems, don't change them), and other domain files.

## Output cap

Under 15 lines.
