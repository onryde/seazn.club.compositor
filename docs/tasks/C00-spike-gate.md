# C00: Spike gate (throwaway code, findings kept)

**Repo:** `seazn.club.compositor`, branch `spike/t00`. This branch is **never merged**. Only
`_FINDINGS.md` and the measured numbers carry forward, via a separate docs commit on `main`.
**Depends on:** nothing. Runs in parallel with C01–C06.
**Gate:** if **item 1 or item 2 fails**, STOP. Write the finding and report to the orchestrator, which
takes it to the owner. No further C task starts. If item 3, 4 or 5 fails, record it and continue:
each one changes a default, not the architecture.

## Context

The spec's two riskiest premises are untested. Spike items 1–5 below test them, plus three numbers
the plan needs:
- `gstcefsrc` builds and performs on Fly.
- Cloudflare's SRT playback behaves the way the harness will model it.
- the default score delay (ADR 0006)
- whether SEI survives Cloudflare
- 1080p cost

**Owner prerequisites** (ask the orchestrator for these; never invent them):
- a scratch Fly app `seazn-relay-spike` with its `FLY_API_TOKEN`
- a scratch Cloudflare Stream live input pair (A and B), with A's SRT playback creds and B's RTMPS
  ingest
- a scratch YouTube stream key for a private test broadcast, attached as an Output on B
- a phone running the seazn-capture app, or Larix, pointed at A

Secrets go in Fly secrets or local env only, never in the repo, argv or logs.

## Files

- Create, on `spike/t00` only: `spike/Dockerfile`, `spike/fly.toml`, `spike/run-*.sh`,
  `spike/overlay-test.html`
- Create on `main`, via a docs commit: `_FINDINGS.md`, with entries F-001 onward

## Steps

- [ ] **Step 1: Branch**
  ```bash
  cd /Users/ashokhein/github/seazn.club.compositor && git switch -c spike/t00 origin/main
  ```

- [ ] **Step 2: Image with GStreamer 1.24 + gstcefsrc.** `spike/Dockerfile`:
  ```dockerfile
  FROM ubuntu:24.04 AS build
  ENV DEBIAN_FRONTEND=noninteractive
  RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake git ca-certificates curl pkg-config \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
        libx11-dev libxcomposite-dev libxdamage-dev libxrandr-dev libnss3 libatk-bridge2.0-0 \
        libgbm1 libasound2t64 libxkbcommon0 libdrm2 \
      && rm -rf /var/lib/apt/lists/*
  WORKDIR /src
  # Pin the gstcefsrc commit here and record it in F-001.
  RUN git clone https://github.com/centricular/gstcefsrc && cd gstcefsrc && git checkout <PIN-IN-F-001> \
   && mkdir build && cd build && cmake -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release .. && make -j"$(nproc)"

  FROM ubuntu:24.04
  ENV DEBIAN_FRONTEND=noninteractive
  RUN apt-get update && apt-get install -y --no-install-recommends \
        gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
        gstreamer1.0-plugins-ugly gstreamer1.0-libav xvfb tini ca-certificates \
        libnss3 libatk-bridge2.0-0 libgbm1 libasound2t64 libxkbcommon0 libdrm2 libxcomposite1 libxdamage1 libxrandr2 \
      && rm -rf /var/lib/apt/lists/*
  COPY --from=build /src/gstcefsrc/build/Release /opt/gstcefsrc
  ENV GST_PLUGIN_PATH=/opt/gstcefsrc
  COPY spike/ /spike/
  ENTRYPOINT ["/usr/bin/tini","--"]
  CMD ["sleep","infinity"]
  ```
  `<PIN-IN-F-001>` is the one value this step fills: the commit you checked out. Write it into the
  Dockerfile and into F-001 together. The checkout may need a different CEF download. gstcefsrc's
  CMake fetches CEF itself; record the CEF version it fetched.

- [ ] **Step 3: Deploy the scratch Machine.** Use `spike/fly.toml` with `app = "seazn-relay-spike"`,
  `primary_region = "lhr"`, `[[vm]] size = "performance-4x" memory = "8gb"`, and no `[http_service]`.
  ```bash
  fly deploy -c spike/fly.toml --ha=false && fly ssh console -a seazn-relay-spike
  ```

- [ ] **Step 4: Item 1, cefsrc.** On the Machine, render the test page. The page is
  `spike/overlay-test.html`: a transparent body, a ticking ms clock, and a moving box. Also render a
  real stg overlay URL that the orchestrator supplies.
  ```bash
  xvfb-run -s "-screen 0 1920x1080x24" gst-launch-1.0 -e \
    cefsrc url="file:///spike/overlay-test.html" num-buffers=900 ! video/x-raw,width=1920,height=1080,framerate=30/1 \
    ! fpsdisplaysink video-sink=fakesink text-overlay=false sync=true -v 2>&1 | tail -20
  ```
  Measure and record in **F-001**:
  - fps achieved (pass: ≥29.5 sustained over 30 s)
  - CPU, via `pidstat -u 1 30` on the cef processes (pass: ≤1 core average)
  - RSS of all CEF processes (pass: ≤600 MB)
  - whether alpha is straight or premultiplied, by checking a pixel on the box's semi-transparent edge
    after `videoconvert`
  - **the readiness mechanism**: read the gstcefsrc source for how the element can wait on the page
    (a property such as `listen-for-js-signals` with a JS call, or a console/title hook). Pin the
    exact property and JS call that work, and prove it: a page that never signals must produce no
    "ready" log.
  - the cold-start time from `fly machine start` to the first cefsrc buffer

  **FAIL** = it does not build, fps < 29.5, or > 1.5 cores.

- [ ] **Step 5: Item 2, Cloudflare SRT playback.** With the phone streaming to Input A:
  ```bash
  gst-launch-1.0 -e srtsrc uri="srt://live.cloudflare.com:778?mode=caller&streamid=<A-playback-streamid>&passphrase=<…>&latency=1000" \
    ! tsdemux name=d d. ! h264parse ! avdec_h264 ! videoconvert ! fpsdisplaysink video-sink=fakesink -v
  ```
  Pass the URL through an env var, never as literal argv in shell history (memory trap: secrets in
  argv leak). Record in **F-002**:
  - Does a **second simultaneous reader** kick the first? Run two at once. Which one survives, and
    with what close reason or log line?
  - Burst behaviour: log inter-frame arrival times for 120 s, then record the p50/p99 gap and the
    longest gap.
  - **Camera→compositor delay:** point the phone at a laptop showing a ms clock (e.g. the
    `overlay-test.html` clock in a browser). On the Machine, snapshot frames with
    `… ! jpegenc ! multifilesink`, each stamped with the Machine's NTP-synced wall clock in the
    filename. Read both clocks from the same frame, take 10 samples, and record the median and spread.
    **This median becomes the default `overlayDelayMs` (ADR 0006).**
  - Reconnect after the phone backgrounds (iOS) and returns: is the time-to-first-frame bounded?

  **FAIL** = SRT playback is unusable (no frames, or a gap > 3 s every minute on a good uplink). In that
  case also try the RTMPS playback fallback and record its numbers.

- [ ] **Step 6: Item 3, SEI survival.** Push a test stream that carries H.264
  `user_data_unregistered` SEI wall-clock stamps from the Machine to Input A. Build it with
  `x264enc` + an `h264parse` SEI injection, or with `ffmpeg -bsf:v h264_metadata=sei_user_data=…`.
  Then read Input A's SRT playback and dump SEI NALs (`h264parse` + a pad probe, or
  `ffprobe -show_frames`). Record in **F-003**: survives / stripped / altered.

- [ ] **Step 7: Item 4, RTMPS to Input B → YouTube.** Push `videotestsrc is-live=true` + `audiotestsrc`,
  x264 CBR 720p30 (`key-int-max=60 bframes=0`), voaacenc and flvmux to B's RTMPS ingest. B has the
  YouTube Output attached. Record in **F-004**:
  - Does the YouTube broadcast go live?
  - Is it accepted **without B-frames** (an open question carried from R0)?
  - Time to live
  - Any YouTube health warnings

  Memory trap C14 says SRT *push* from Fly never started a broadcast, so do not use SRT here.

- [ ] **Step 8: Item 5, cost numbers.** Run the full chain in one `gst-launch` (SRT in → compositor with
  a cefsrc overlay → x264 → RTMPS to B) at 720p30 and at 1080p30, for 5 min each. Record in
  **F-005**: total CPU %, RSS, `droppedFrames` (`queue` overruns) and encoder speed.

- [ ] **Step 9: Write `_FINDINGS.md` on main.** Each entry gives an ID, date, item, method, raw
  numbers, verdict (PASS/FAIL), and "what changes": which `config.rs` default or which ADR. Use this
  format:
  ```markdown
  ## F-002 — Cloudflare SRT playback behaviour (2026-10-xx)
  **Method:** …  **Raw:** second reader → first closed after … ; p50 gap … ms, p99 … ms, max … ms;
  camera→compositor delay median … ms (n=10, spread …)
  **Verdict:** PASS
  **Changes:** `Thresholds::default().input_*` unchanged; web `relay/config.ts` default overlay delay = … ms (R2002)
  ```
  ```bash
  git switch main && git add _FINDINGS.md && git commit -m "docs(findings): T00 spike gate F-001..F-005"
  ```

- [ ] **Step 10: Tear down.** `fly scale count 0 -a seazn-relay-spike`, and remove the scratch YouTube
  Output. Record in F-005 that teardown was done.

## Acceptance

- `_FINDINGS.md` has F-001 to F-005, each with raw numbers and a verdict.
- The readiness mechanism is pinned exactly (property name plus JS call).
- The default delay is measured (n ≥ 10).
- GATE: items 1 and 2 pass. Otherwise STOP and report.

## Do not touch

`crates/`, `docs/specs/`, `docs/decisions/`. Findings that contradict an ADR are **reported**, not
edited into the ADR.

## Output cap

The final message stays under 15 lines: the verdicts for items 1–5, the key numbers, the pinned
readiness mechanism, and blockers. No logs or file contents.
