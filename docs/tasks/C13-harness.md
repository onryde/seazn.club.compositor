# C13: Harness (fake camera, fake CF, fake destination, fake control plane, reference checker)

**Repo:** `seazn.club.compositor`. **Depends on:** C12 (the image). Read spec §9.3 and F-002.

## Context

The harness runs the **real compositor image** against fakes of everything around it, in Docker
Compose. C14 (chaos) and C15 (soak) are scripts over this harness, so this task delivers the harness
and **one** smoke case that proves every part reports facts.

```
fake-camera ──SRT publish──▶ mediamtx (fake CF)  ◀──SRT read── compositor ──RTMP──▶ mediamtx path live/dest (fake dest, records fmp4)
                                   ▲  API :9997 kick/close                 │  ▲
netem-out (tc on compositor's netns) · netem-in (tc on mediamtx's netns)              │  └── GET session / POST heartbeat / overlay page
                                                                           └────▶ fake-control
checker ◀── reads the recorded fmp4 segments + fake-control's heartbeat log + camera's emit log
```

**One MediaMTX plays both CF and destination**, on separate paths, because the impairments target
different ports:
- SRT `8890` is the input
- RTMP `1935` is the output

`tc` filters on those ports so each side can be impaired alone.

## Machine-readable strips (the checker's ground truth)

- **Camera strip.** The bottom 24 px of each 1280×720 camera frame holds 48 cells of 24×24, black or
  white:
  - cells 0–31: the frame counter (u32, MSB first)
  - cells 32–47: a checksum = `counter.wrapping_mul(2654435761) >> 16` (u16)

  A frame whose checksum fails is "no strip": a slate or a corrupted frame.
- **Overlay strip.** `harness/pages/overlay-test.html` (created in C11; this task extends it) draws
  the score timeline:
  - top-right 16 cells of 24×24 at `x = 1920 − 16·24`, `y = 0` in page space. After scaling to
    720p that's 16 px cells at `x = 1024`.
  - each cell is the page's current **score index** (u16), the index of the latest timeline event
    whose effective time (`at + delay`) ≤ the page clock
  - the timeline is served by fake-control at `/score.json` and polled every 1 s, as the real page
    does
  - `?delay=<ms>` sets the delay
- **Audio.** A 1 kHz tone burst of 50 ms starts at every frame whose `counter % 30 == 0`. The checker
  measures the onset → frame offset (A/V target ±40 ms).

The camera writes `emit.jsonl` (`{"counter":n,"wall_ms":t}`, one line per frame). The checker uses
it to turn counters into camera wall time.

## Files

- Create: `crates/harness/` (a workspace member; may depend on gstreamer and `adapters::redact`, never
  on domain internals) with three bins:
  - `fake-camera`: appsrc draws the strip, plus an audio tone, then `x264enc key-int-max=30 ! mpegtsmux ! srtsink`
    to `srt://mediamtx:8890?streamid=publish:cam`. It also serves an HTTP control on `:7001` with
    `POST /pause` (stop emitting; the counter keeps advancing at 30/s, so the gap is visible in `emit.jsonl`), `POST /resume`, `POST /burst?ms=3000` (pause while still counting, then emit the
    backlog as fast as possible with original PTS), and `POST /size?w=&h=`.
  - `fake-control`: tiny_http on `:7000`:
    - `GET /api/internal/relay/sessions/{sid}`: the vendored `session-composed-720p.json`, with
      `input.url`, `target.url` and `overlayUrl` rewritten to harness hosts by an env map
    - `POST …/heartbeat`: appends the body + `received_ms` to `heartbeats.jsonl`, and replies
      `{desiredState}` from its state
    - `POST /control/desired?state=ending|live`
    - `POST /control/score-down?ms=60000`
    - `POST /control/skew?ms=5000` (serve timeline events stamped that far ahead; C14 case 11)
    - `GET /score.json`, `/overlay-test.html`: static from `harness/pages`
    - it checks the bearer token equals the env `JOB_TOKEN` and returns 401 otherwise
  - `checker`: `checker --recordings <dir> --emit <emit.jsonl> --heartbeats <hb.jsonl> --timeline <score.json> --out report.json`
- Create: `harness/compose.yml`, `harness/mediamtx.yml`, `harness/netem.sh`, `harness/run-case.sh`,
  `harness/cases/smoke.sh`
- Create: `harness/pages/score.json` (a scripted 20-event timeline at fixed offsets from the run start;
  includes one void at event 12)
- Test: `crates/harness/tests/{strip.rs,checker.rs}`

## Report schema (`report.json`)

The checker reports facts. **The cases decide pass/fail (C14).** That way one checker serves every
case and the soak.

```json
{
  "frames": 0, "duration_ms": 0, "fps_mean": 0.0,
  "pts_gaps": [{"at_ms": 0, "gap_ms": 0}],
  "segments": [{"file": "", "start_wall_ms": 0, "frames": 0}],
  "output_outages": [{"from_wall_ms": 0, "to_wall_ms": 0}],
  "camera": {
    "strip_frames": 0, "slate_frames": 0,
    "freezes": [{"from_ms": 0, "ms": 0, "counter": 0}],
    "slates": [{"from_ms": 0, "ms": 0}],
    "counter_regressions": 0,
    "resumes": [{"first_emitted_counter": 0, "first_shown_counter": 0, "suppressed_ms": 0}],
    "latency_ms": {"p50": 0, "p95": 0, "max": 0},
    "latency_windows_ms": [{"from_wall_ms": 0, "to_wall_ms": 0, "p50": 0}]
  },
  "overlay": {"score_index_changes": [{"frame_wall_ms": 0, "index": 0, "due_wall_ms": 0}], "late_frames_max": 0, "future_dated": 0},
  "audio": {"av_offset_ms": {"p50": 0, "max_abs": 0}, "silent_gaps": [{"at_ms": 0, "ms": 0}]},
  "heartbeats": {"count": 0, "max_gap_ms": 0, "fields_seen": {}, "last": {}}
}
```

Field meanings:
- **`output_outages`:** wall-clock spans with no recorded frame. These come from segment boundaries:
  MediaMTX starts a new segment per publisher connection, and the filename carries wall time.
- **`latency_ms`:** recording wall time of the frame minus the camera `emit.jsonl` wall time for that
  counter. Measured only on frames with a strip.
- **`counter_regressions`:** a strip frame whose counter is lower than an earlier one. Must be 0 in
  every case, because a burst replay must never show older frames.
- **`resumes`:** one entry per gap > 1 s in `emit.jsonl`. `first_emitted_counter` is the camera's
  first counter after the gap, and `first_shown_counter` is the first strip counter in the recording
  after the slate span that covers the gap. `suppressed_ms` = (shown − emitted) × frame period: how
  long the programme held the slate after frames returned. That's the hysteresis, measured
  independently of latency.
- **`latency_windows_ms`:** `latency_ms.p50` per 10 s wall window. C14's input-impairment case
  proves the impairment landed with it.
- **`late_frames_max`:** for each score index change, the frames between `due_wall_ms` and the first
  frame that shows it.
- **`future_dated`:** frames that show an index whose `due_wall_ms` is after the frame's wall time.

## Steps

- [ ] **Step 1: Write the failing strip test.** `strip.rs`: encode a counter into an I420 frame with
  the shared `strip::draw`, run the frame through `x264enc bitrate=4000 → avdec_h264`, then decode it
  with `strip::read`. Do this for 1 000 random counters and assert all are recovered. Also assert that
  a solid slate frame decodes as `None`. Print `strips checked: N` and fail if `N == 0`.
- [ ] **Step 2: Write the failing checker test.** `checker.rs` builds a synthetic recording:
  - a GStreamer test pipeline writes 2 fmp4 segments, with a 1.5 s wall gap, a 20-frame freeze and
    10 slate frames
  - it writes a matching `emit.jsonl` and heartbeats file

  Assert the report finds exactly 1 outage of ~1.5 s (±100 ms), 1 freeze of 20 frames, 1 slate span
  of 10 frames, and 0 regressions. Then add one out-of-order frame, and assert `counter_regressions == 1`.
  The expected values come from how the fixture was built, never from checker output.
- [ ] **Step 3: Run, expect a FAIL.** `cargo test -p harness`
- [ ] **Step 4: Implement `strip.rs`, the three bins, `compose.yml`, `mediamtx.yml` and `netem.sh`.**
  - **MediaMTX:** pin an exact image tag, recorded as F-0xx with the date. Settings:
    - `api: yes`
    - path `cam`: `source: publisher`
    - path `live/dest`: `record: yes`, `recordFormat: fmp4`,
      `recordPath: /rec/%path/%Y-%m-%d_%H-%M-%S-%f`, `recordSegmentDuration: 10s`
    - a kick is `POST /v3/srtconns/kick/{id}` after listing `/v3/srtconns/list` for the reader; pin
      these API paths from the tag's docs in the F-entry
  - **compositor service:** the C12 image with `COMPOSITOR_DRIVERS=live`,
    `APP_URL=http://fake-control:7000`, a random `SESSION_ID`/`JOB_TOKEN`, and a deadline from the case
    script. `restart: on-failure:2` emulates Fly. Resource limits: `cpus: 4`, `mem_limit: 8g`.
  - **Two netem sidecars.** `tc` qdiscs act on **egress** only, so each leg is impaired at its
    sender:
    - `netem-out`: `network_mode: "service:compositor"`, `cap_add: [NET_ADMIN]`. Filter
      `u32 match ip dport 1935 0xffff`: the compositor's RTMP media to the destination.
    - `netem-in`: `network_mode: "service:mediamtx"`, `cap_add: [NET_ADMIN]`. Filter
      `u32 match ip sport 8890 0xffff match ip dst <compositor ip>`: MediaMTX's SRT media **to the
      compositor**, never to the camera.

    `netem.sh in|out|clear <args>` runs `tc` in the matching sidecar: a prio qdisc, the filter, and a
    netem child on the filtered band. A filter on the compositor's `dport 8890` would only delay SRT
    ACKs, which is why the input leg lives on MediaMTX.
  - **`run-case.sh <case>`:** brings the stack up fresh (`down -v` first), runs `cases/<case>.sh`
    (which drives the controls and sleeps for its duration), brings the stack down, runs the
    checker, and writes `out/<case>/report.json`. It exits with the case's own verdict, which a
    case computes from the report with `jq`.
- [ ] **Step 5: Write `cases/smoke.sh`.** 60 s, no impairment. Its `jq` verdict requires all of:
  - `frames ≥ 1700`
  - `pts_gaps == []`
  - `camera.counter_regressions == 0`
  - `camera.latency_ms.p95 > 0`
  - `overlay.score_index_changes` has ≥ 1 entry
  - `heartbeats.count ≥ 10`
  - `heartbeats.fields_seen` includes `programmePhase`, `input`, `output`, `graphics`

  These are anti-vacuity guards, because a harness that recorded nothing must fail.
- [ ] **Step 6: Run, expect a PASS.** Run `cargo test -p harness`, then `harness/run-case.sh smoke`.
  Paste the report's top-level counts.
- [ ] **Step 7: Mutate once each.**
  1. Make `strip::read` return `Some` for slates. Expected red: `strip.rs`.
  2. In `cases/smoke.sh`, point the checker at an empty recordings dir. Expected: the verdict fails
     on `frames`.
- [ ] **Step 8: Add CI** (`.github/workflows/harness.yml`, on `pull_request` with paths `crates/**`,
  `harness/**`, `docker/**`). Build the image, then run `run-case.sh smoke`, and upload `out/` as an
  artifact. **Commit and open the PR.**
  `feat(harness): fakes, burned-in strips and the reference checker (C13)`

## Acceptance

- The strip and checker tests pass, with counts.
- The smoke case is green locally and in CI.
- The report has every field populated.
- 2 mutants went red.

## Do not touch

The compositor crates, except reading `adapters::redact`.

## Output cap

Under 15 lines: the smoke report numbers, the MediaMTX tag F-id, deviations.
