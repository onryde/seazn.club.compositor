# C00 spike gate: findings

- **Branch:** `spike/t00` (never merged; only this file and its numbers carry forward)
- **Run:** 2026-10-15, on Fly app `seazn-relay-spike`, region lhr
- **Machines:**
  - Main: performance-4x, 8 GB, AMD EPYC, 4 vCPU, `48edd39ead9098`
  - Source feed: performance-2x, `800eeda6e99948`
  - Cold start: one throwaway Machine, `80e9d00a1267e8`
- **Cloudflare (CF) Stream:** Input A `c496c37a636e2930297eed6a5265f3d1`; Input B `68fd8677e76a572f8d1030c898a176f8`, which has the YouTube Output `8aeb97454ea0686779c8726012dd5d9a`
- **Image:** `registry.fly.io/seazn-relay-spike:deployment-01M4ZBW92TN0GPMPC9S0KK5TQ3`, digest `sha256:15890e25c22a…`. Ubuntu 24.04, GStreamer 1.24.2.
- **Evidence:** every number below comes from a command run in this session. Secrets appear only as their names.

## Scope and gate (owner rulings 2026-10-15; they amend the brief)

- **In scope:**
  - item 1 (cefsrc)
  - item 2, partial: the second-reader kick, plus bursts and inter-frame gaps, using a feed generated on a Machine
  - item 3 (SEI survival)
  - item 5 (720p30 vs 1080p30 cost)
- **Deferred to an owner session:**
  - item 2: the camera→compositor delay median and the phone background/reconnect test
  - item 4 (YouTube go-live). The owner nevertheless watched YouTube Studio during the item-5 pushes. Those observations are recorded in F-004.
- **Gate change, item 1:** CEF CPU is a *recorded diagnostic*. Item 1 fails only if gstcefsrc does not build, fps < 29.5 sustained, or CEF uses more than 2.0 cores on average. The brief's "> 1.5 cores" criterion is superseded.
- **Gate change, item 5 (the real cost gate):** at 720p30, the full chain must average ≤ 3.0 of the Machine's 4 cores, with no sustained dropped frames and encoder speed ≥ 0.95. The full chain is SRT in → decode → compositor + cefsrc overlay → x264 → RTMPS to Input B. 1080p30 is recorded only.
- **Portability:** the spike is a plain Docker image with env-var config and no Fly paths inside the container. Fly-specific behaviour is listed under Platform notes.

---

## F-001: gstcefsrc builds and performs on Fly (2026-10-15)

**Method:**
- `spike/Dockerfile` builds gstcefsrc inside the image.
- `cef_probe.py` builds the pipeline `cefsrc ! BGRA 1920x1080 ! tee`:
  - one branch goes to an appsink, which records arrival times, PTS and probe pixels;
  - one branch goes through a compositor over black to an appsink.
- `proc_sample.py` samples the process tree (utime+stime, RSS and PSS) every 1 s, plus machine-wide CPU.
- Display: Xvfb `:99`. User: non-root `spike` (uid 10001).

### Build

| Item | Value |
|---|---|
| gstcefsrc pin | `204b01b02df23d143316b11e51516171ab7fae0a` (main HEAD, 2026-08-24, "cefsrc: discard data in the PAUSED state") |
| CEF | `139.0.28+g55ab8a8+chromium-139.0.7258.139` linux64, fetched by gstcefsrc's CMake |
| Compile time | ≈182 s; the Fly remote build took ≈5 min |
| Size | `libcef.so` is 1,463,731,440 B unstripped; `/opt/gstcefsrc` is 1.5 G (note for C12) |

**False premise:** the brief's build-dependency list **fails to link** `gstcefsubprocess`. The linker reports undefined `ipp*`, `cups*`, `pango*` and `cairo*` symbols. Adding `libcups2t64`, `libpango-1.0-0` and `libcairo2` to the build stage fixes it.

### Caps

**False premise:** `video/x-raw,…,framerate=30/1` (the brief, and C11's pipeline line) does **not** negotiate:

> could not link cefsrc0 to fpsdisplaysink0, cefsrc0 can't handle caps video/x-raw, width=(int)1920, height=(int)1080, framerate=(fraction)30/1

- cefsrc advertises `framerate=0/1` (since gstcefsrc commit c89c2ce).
- Working form: the property `max-video-framerate=30/1`, with caps that carry no framerate.
- cefsrc emits a buffer **only when the page paints**. The static stg overlay `/overlay/fixtures/02786dd0-…` gave 4 buffers in 40 s (0.27–0.68 fps). The compositor must therefore hold the last overlay frame.

### Frame rate (animated test page, `overlay-test.html`)

| Run | Result |
|---|---|
| appsink, 30 s | 905 buffers; 30.13 fps by arrival, 30.00 by PTS; per-second minimum 30; gaps p50 33.41 / p99 34.61 / max 37.8 ms |
| gst-launch fpsdisplaysink, 900 buffers | average 29.69 (includes startup), current 30.00, 0 dropped |
| Inside the item-5 chain | cef fps 30.0 average over 290 s, at both 720p and 1080p |

### CPU and memory (the recorded diagnostic)

| Page | CEF tree cores avg (p95) | PSS MB | Machine mem max MB |
|---|---|---|---|
| test page, appsink (1c2) | 0.142 (0.157) | 374–380 | 523 |
| test page, gst-launch (1b) | 0.133 | ≈368 | 529 |
| `?stress=1`, full-frame semi-transparent repaint every frame | 0.502 (0.538, max 0.567) | 418–424 | 528 |
| stg overlay (static) | 0.004 | 387 | — |
| test page inside the item-5 chain | 0.103 at 720p / 0.105 at 1080p (renderer + gpu + utility) | — | — |

- The RSS sum of the tree is 778–797 MB, but RSS double-counts shared pages; PSS is the honest number.
- Stress-page breakdown: renderer 0.307, gpu-process 0.15, host 0.045 cores.
- CEF child processes (all with comm `gstcefsubproces`): zygote ×2, gpu-process, utility ×2, renderer ×2. Chromium rewrites argv, so identify children by comm plus `--type=`.

### Alpha is PREMULTIPLIED

| Patch | Raw cefsrc BGRA |
|---|---|
| white at 50 % | (128,128,128,128) |
| red at 50 % | (0,0,128,128) |
| green at 25 % | (0,64,0,64) |
| empty | (0,0,0,0) |
| opaque blue | (255,0,0,255) |

- The GStreamer `compositor` assumes straight alpha. Over black, white50 → (64,64,64,255), red50 → (0,0,64,255) and green25 → (0,16,0,255): the edges are double-darkened.
- Visible in the item-5 snapshot. **C11 must unpremultiply**, as its conditional step already anticipates.
- Trap: compositor `zorder` follows pad *request order*, not the pad name. The first composite came out all black until `sink_0::zorder=0 sink_1::zorder=1` was set explicitly.

### Readiness mechanism: PINNED to the console channel

- Mechanism: `listen-for-js-signals=false`, and the page calls `console.info("seazn:ready")`.
- The message arrives as a GStreamer log line in category **`cefconsole`** at INFO (`OnConsoleMessage`, gstcefsrc.cc:442). Frames flow regardless of readiness, and the same channel can carry "score stale".
- Verified via `GST_DEBUG=cefconsole:4`. The Rust `gst::log` hook was not exercised; that is owed to C11.

Why not the built-in JS signal (`listen-for-js-signals=true` + `window.gstSendMsg({request:"ready", onSuccess, onFailure})`)?

- **It works.** The router is injected by gstcefsubprocess; `OnQuery` accepts only `"ready"` and `"eos"`, and any other request gets the warning "js signal processing error". The page signalled at 0.797–0.808 s, with the ack `{ "success": "true", "cmd": "ready" }`. Readiness is observable three ways: the INFO line "js signal processed successfully: ready", the bus PROGRESS complete/"open"/"CEF browser created", and the first buffer.
- **Negative proof (`?noready=1`):** 0 buffers. `set_state(PLAYING)` never returned, and **`set_state(NULL)` did not return within 10 s either**, so teardown hangs. `start()` blocks with no timeout; only a process kill recovers.
- **Bad buffers:** 5 buffers of the wrong size and without PTS arrive at start; with js-signals off there are 0.
- **Beside another live source in the same pipeline** it errors:
  - "gst_video_frame_map_id: invalid buffer size 0 < 8294400"
  - videoaggregator "Need timestamped buffers!"

### Guardrails

- **nice:** `nice 10` for all CEF children works via `GST_CEF_SUBPROCESS_PATH=/spike/cef-nice.sh`, a wrapper that execs `nice -n 10 "$CEF_SUBPROCESS_PATH"`. The host process stays at nice 0.
- **oom_score_adj:** Chromium overrides the wrapper's value (zygote 0, gpu/utility 200, renderer 300). A non-root user *can* raise each child to 1000 afterwards (write ok for all).
- **Sandbox:** no-sandbox is the Linux default.
- **Log noise:** dbus "Failed to connect to the bus" and a CEF "ssl handshake failed".

### Cold start (`run-coldstart.sh` as the Machine command; Xvfb + cefsrc, test page)

| Path | Time |
|---|---|
| `fly machine run` invoked → first CEF buffer, fresh Machine | **25.07 s**. Of that: image pull 14.38 s; init 1.06 s; script start → first buffer 5.69 s; cefsrc start → first buffer 2.781 s |
| `fly machine start` of the stopped Machine → first buffer | **7.48 s** (no pull). Script start uptime 1.04 s; cefsrc start → first buffer 2.804 s |

### Element facts for later tasks

- `x264enc speed-preset` is flagged "readable, writable" with no changeable-in-PLAYING flag, so it cannot be changed live (C09).
- A sink behind an appsrc that is fed from another branch of the *same* pipeline needs `async=false`, or the pipeline stays PAUSED.

**Verdict:** **PASS** under the amended gate: it builds; fps is 30.0 sustained; CEF averages 0.10–0.50 cores, ≤ 2.0.

**Changes:**
- **C11 pipeline line:** replace `framerate=fps/1` in the caps with the property `max-video-framerate=fps/1`, and hold the last frame.
- **C11 readiness:** the readiness property is `listen-for-js-signals=false`. `CefReady` comes from a `cefconsole` log line `seazn:ready`. C11's test `page_without_ready_signal_times_out` ("frames arrive but no CefReady") holds **only** under this console mechanism; under js-signals no frames arrive and `start()` hangs.
- **C11:** unpremultiply before the compositor.
- **C12:** add `libcups2t64`, `libpango-1.0-0` and `libcairo2` to the build stage, and strip `libcef.so` (1.46 GB).
- **C09:** `speed-preset` is not settable in PLAYING.

---

## F-002: Cloudflare SRT playback behaviour (2026-10-15), PARTIAL

**Method:**
- **Source:** the performance-2x Machine pushes 1280x720@30 into Input A over RTMPS. Encoding: x264 veryfast/zerolatency, 3000 kbps CBR, keyint 60, no B-frames, plus AAC 128k. This mimics capture `Link.kt`. An SEI stamp is inserted in every AU (`source_feed.py`).
- **Readers:** `srt_reader.py` runs `srtsrc` (latency 1000) → `tsdemux` → `h264parse` and logs per-AU arrival, PTS and SEI.
  - Readers ran on the main Machine and, for the cross-IP test, on the source Machine.
  - This is a **datacentre-uplink baseline**, not a phone.

**Self-inflicted false negative (fixed, recorded because it is a trap for the harness):**
- The first feed was audio-only. x264enc output sits in a 1000 h segment, and the appsink→appsrc hop sent the raw PTS under appsrc's default segment, so flvmux never picked the video pad.
- **CF still reported the input "connected".** The symptoms were:
  - CF videos showed `ERR_LIVE_VIDEO_TRANSITION_FAILED` with width/height -1;
  - HLS returned 424, and lifecycle stayed `initializing`;
  - **SRT playback completed the handshake, with KM SECURED both ways, and then delivered 0 bytes for 30–40 s with no close, no EOS and no error.**
- This was seen with two clients (GStreamer srtsrc and ffmpeg), two Machines/IPs, both ingest protocols, and recording off and on.
- A no-appsrc control feed went live at once. Re-timing the buffers to running time fixed the real feed (`spike/source_feed.py`).

**Raw:**

| Measurement | Result |
|---|---|
| Time to first video AU, 9 fresh connects with a printed value | 2.22–3.89 s. In the full chain, first *decoded* camera frame 2.93 s and 3.26 s (720p runs), 4.19 s (1080p) |
| 120 s burst (`burst120b`, 30 fps source) | 3550 AUs; arrival 30.12 fps; gap p50 33.28 / p90 40.44 / p99 64.36 / max 69.55 ms; **0 gaps > 100 ms**; per-minute max 69.5 / 69.5 ms; PTS gap 33/34 ms; keyframe every 60 frames; SRT packets lost 0 / retransmitted 0 / dropped 0; 48.18 MB received (3.2 Mbps) |
| Earlier 120 s run (source at ≈26 fps, CPU-bound before decoupling queues) | gap p50 41.89 / p99 66.7 / max 108.8 ms; 2 gaps > 100 ms; 0 gaps > 250 ms |
| **Second reader, same IP** (`run-kick.sh 150 40 60`) | **No kick.** r1 (150 s) and r2 (60 s, from +40 s) both ran to their own timeout. r1 kept 0 gaps > 100 ms and a 67.6 ms maximum through the overlap; r2's first frame came at 2.31 s. Neither saw EOS or an error |
| **Second reader, different IP** (r2 on the source Machine) | **No kick.** r1 120 s and r2 60 s both ran to timeout. r2 had 10 packets lost and 10 retransmitted (recovered); r1 max gap 71.5 ms |
| Source-encoder wall → reader wall (from the SEI stamp; two Fly Machines' NTP clocks) | RTMPS ingest median 1408–1409 ms (n=3550; p99 1418; max 1875). An earlier source session gave a median of 1127 ms, so it varies per session. SRT ingest median 2348 ms (n=1109). This is **not** the camera→compositor delay |

**Verdict:** **PASS** for the in-scope parts. Playback is usable; the worst gap is 70–109 ms against the 3 s fail line.

### Owner session, with the real phone (2026-10-15)

**Step A: camera→compositor delay (sets the ADR 0006 default).**

*Method:*
- The owner's phone ran the seazn capture app on stg fixture `cb72e011-…`, session `c1405214-…`. Mode: passthrough. Ingest: SRT into CF input `f23f022e303a0badff34b9cb2816a71e`.
- The phone camera pointed at a laptop showing a UTC clock and epoch ms. The laptop runs +15 ms ahead of time.apple.com (±8 ms).
- On a performance-4x Machine, `delay_snap.py` ran `srtsrc` (`SPIKE_FIX_SRT_PLAY_URL`, latency 1000) → `tsdemux` → `h264parse` → `avdec_h264` → `jpegenc`. One frame was written every 5 s, named with the Machine's wall-clock ms when the frame left the decoder.
- I read the clock digits in each JPEG myself. Each frame shows two superimposed display states, so a reading is good to ±10 ms.
- Delay = machine_ms − clock_ms + 15 (the laptop offset).
- Machine clock by SNTP: −0.7 to +2.7 ms vs time.cloudflare.com and +0.2 to +2.3 ms vs time.google.com (rtt 2–11 ms); +6 ms vs time.apple.com (rtt 14 ms). Treated as 0 ±6 ms. `chronyc` is not in the image.

*Raw:* n = 16 frames over 75 s (11:55:53–11:57:08 London).

| Statistic | Value |
|---|---|
| Delays, ms | 3318, 3323, 3325, 3307, 3323, 3312, 3311, 3298, 3322, 3332, 3324, 3316, 3330, 3323, 3318, 3303 |
| **Median** | **3320 ms** |
| Spread | 3298–3332 ms (34 ms) |
| Mean / sd | 3317.8 / 9.5 ms |
| First frame after reader start | 2.614 s |
| Decoded rate | 2340 frames in 80 s (29.3 fps) |

The laptop display's own render/scan-out lag (≈1 refresh) was **not** subtracted, so 3320 ms is an upper bound by about 10–20 ms. The path includes the SRT playback latency of 1000 ms.

**Second session.** The owner went live again, which created session `d13d8b23-…` with input `d98e9879e99187cd8d2d66c007a1911f`.
- The new session went live at 10:59:24Z, only 26 s after the first session ended at 10:58:58Z.
- Second sample, n=9, 12:07:06–12:07:48 London: 3284, 3267, 3263, 3269, 3272, 3271, 3270, 3270, 3266 ms. **Median 3270 ms** (3263–3284, sd 5.9).
- Both sessions together, n=25: median 3311 ms (3263–3332).
- The delay moves about 50 ms between sessions and is stable to ±10 ms within one.

~~Recommendation: default `overlayDelayMs` = 3300 ms~~ is **SUPERSEDED** by the redo below. The decoder-out reader turned out to overstate the in-chain delay by about 300 ms.

**Redo, fourth session** (`5346c02c-…`, input `3a358122…`):
- Decoder-out sample (same `delay_snap.py`), n=10, 14:43:35–14:44:20 London: 3416, 3412, 3422, 3399, 3412, 3401, 3409, 3408, 3418, 3413 ms. **Median 3412 ms** (3399–3422, sd 7.1).
- Across the three decoder-out sessions, n=35: median 3322 ms. **Session medians differ by up to 140 ms** (3270 / 3320 / 3412).

**In-chain delay (the quantity ADR 0006 actually needs).**
- `overlay-test.html` prints the CEF `Date.now()` in the overlay.
- In a composite frame, overlay time minus the clock seen through the camera, +15 ms laptop offset, is exactly how far the camera lags the overlay in what the viewer sees.

| Session | London | Overlay ms − clock ms + 15 |
|---|---|---|
| `afe2c9f9…` (rerun 1) | 14:17:27 | **3083 ms** |
| `5346c02c…` (redo) | 14:47:28 | **3105 ms** |

- On the redo session, the decoder-out reader read **3412 ms**, against **3105 ms** in-chain on the same session: the separate reader is about **307 ms higher**.
- A 60 s in-chain calibration run (`chain.py --no-push --snapshot-every 5`, 14:55:00–14:56:00, 11 frames) was **owed**: the phone was no longer on the clock (the laptop showed a terminal).

**Recommendation:**
- Default `overlayDelayMs` = **3100 ms**, taken from the in-chain readings. n=2, which is thin.
- **Owed:** an in-chain n≥10 sample (`chain.py --no-push --snapshot <p> --snapshot-at 10 --snapshot-every 5`, phone on the clock) before C11 freezes the default.
- Expect ±100 ms session-to-session drift (decoder-out session medians spread by 140 ms). This argues for the per-match nudge ADR 0006 already provides.
- If C10 ships a different SRT `latency`, the default moves with it.

**Step B: phone background/reconnect. PENDING the owner's report.**

The owner was asked to background the capture app at 12:08:00 London for 20 s.

*What the Machine saw:* `bg_watch.py` read 12:07:00–12:11:00 London, with a 5 s stall rebuild armed.
- One connection, never closed and never stalled; 7090 AUs (≈30 fps).
- The only gap over 300 ms was **328 ms** (12:08:36.296–12:08:36.624).
- CF showed input `d98e98…` connected throughout, with no disconnect in its history.

*Two readings, undecided:*
- If the app was backgrounded, the capture app keeps streaming in the background: no reconnect, worst gap 328 ms.
- Otherwise the background did not happen, and B is still owed.

*Related observations:*
- When the phone itself stops publishing, the CF playback socket stays open and silent, as in F-002. When the first session ended (10:58:58Z), the reader got 0 frames until it was rebuilt onto the new input.
- The new session's input was live 26 s after the old one ended.

**Step B, rerun (14:09–14:13 London, session `afe2c9f9…`):**
- CF kept input `4e7a53…` connected from 13:05:33Z to the owner's stop at 13:21:34Z.
- The watcher saw no gap over 300 ms between 14:09:09 and 14:13:00 (6932 AUs).
- The owner's own background/foreground times were **never reported back**. So B ends with no reconnect observed in either attempt. If the backgrounds happened, the capture app keeps publishing while backgrounded (worst gap 328 ms). **The time to first frame after an iOS background stays OWED**, pending owner confirmation that the app was really backgrounded.

**Steps C and D: ran 12:13:00–12:21:50 London WITHOUT an owner view. YouTube side OWED: rerun.** (Superseded by F-004 "Redo".) The owner stepped away, and the HOLD arrived after the run. The Machine-side numbers below are valid; nothing on the YouTube side of C or D was observed.

*Step C* (5 min to Input B):
- Settings as C09 ships: 720p30, x264 4000 kbps, `vbv-buf-capacity=1000`, `nal-hrd=cbr:filler=1`, keyint 60, bframes 0, scenecut on.
- Camera = SRT playback of the phone, plus the `overlay-test.html` overlay.
- Results:
  - filler share **0.21 %** (real video 3989 kbps); keyframes 148 in 290 s (≈1.96 s);
  - egress 3931–4367 kbps per 10 s window; encoder ≥ 29.8 fps after the first window; 0 queue overruns;
  - Machine 1.168 cores average (p95 1.38, max 1.557).
- The scene content was not verified (no snapshot); it was presumably still the laptop/clock scene. Even so, real camera footage carries almost no filler at 4000 kbps, unlike the 56–78 % of the test-pattern runs in F-004.

*Step D* (each stop is SIGKILL of the chain, i.e. a crashed compositor; each restart opens a new RTMPS connection to B). Wall times (London):

| Gap | Kill | Restart | First 10 s window after restart |
|---|---|---|---|
| 5 s | 12:18:00.831 | 12:18:05.837 | encoder 28.45 fps, camera 23.1 fps |
| 15 s | 12:19:05.854 | 12:19:20.860 | encoder 28.39 fps, camera 20.3 fps |
| 30 s | 12:20:20.882 | 12:20:50.888 | encoder 28.44 fps, camera 20.7 fps |

- Final kill at 12:21:50.914.
- Each run lasted 60 s, with 0 overruns and egress 3938–4241 kbps.
- CF Input B's status history only keeps the latest connect: 11:20:52.023Z, then `client_disconnect` at 11:21:51.249Z. CF does not retain a per-gap history, so the YouTube side of each gap is unobserved.
- The phone input itself disconnected at 11:21:38.413Z. In the last D30 window (12:21:31–41) the camera fell to 22.6 fps, and **the compositor output fell with it to 25.7 fps.** A plain GStreamer `compositor` in live mode is not fully clock-driven when the camera starves, which supports C09's own frame-tick design.

**Changes:**
- **Spec, reported only (ADRs and spec not edited).** The spec says "CF SRT playback is single-reader / a second reader kicks the first" (§3.1 topology diagram; §3.2 "Exactly one reader per SRT playback stream"; §4.2 InputLink kicks; §6 failure table; §9.1 Q2; risk table). That is **false as measured**: CF served two concurrent readers from the same IP and from different IPs, with no kick. "One reader" stays a valid *design* choice. C04's kick counting stays for CF-side closes and maintenance, but C13's fake CF should not model "a second reader kicks the first" as CF behaviour.
- **`Thresholds::input_stall_after_ms` = 5 000:** unchanged and confirmed necessary. CF holds an encrypted, silent playback socket open indefinitely when the ingest carries no usable video.
- **`input_connect_timeout_ms` = 5 000:** unchanged, but the margin is thin. The worst first AU was 3.89 s and the worst first decoded frame 4.19 s. C10 should time "connected" to the first decoded frame, never to the socket handshake.
- **C13:** the harness owes a "connected but silent" input case.

---

## F-003: SEI survives CF Input A → SRT playback (2026-10-15)

**Method:**
- `sei.py` inserts an H.264 `user_data_unregistered` SEI NAL into **every** AU: UUID `SEAZN-C00-SEI-v1`, payload `wall_ms=<epoch ms>;seq=<n>`.
- `srt_reader.py` parses every AU. `run-item3.sh` dumps 20 s of TS, which is cross-checked with `ffprobe -show_frames` and ffmpeg `-bsf:v trace_headers`, an independent parser.

**Raw:**

| Check | Result |
|---|---|
| RTMPS ingest (item3, 35 s) | 944/944 AUs carry the SEI; 0 unparseable, 0 seq gaps, 0 seq backwards |
| ffprobe | 617/617 frames have unregistered-SEI side data |
| trace_headers | the UUID bytes `…S E I - v 1` are present in all 617 |
| Byte-identical payload | e.g. `wall_ms=1792057506842;seq=15648` |
| Longer runs | burst120b 3550/3550, kick-r1 4447/4447 |
| SRT push ingest | 1109/1109 |

- NAL counts per AU show that CF **passes the H.264 bitstream through** on SRT playback. x264's HRD SEI and the filler NALs (type 12, 944 of them) survive. Nothing is re-encoded.
- x264's own version SEI is seen 0 times, because it is only on the very first IDR, before any reader joins.

**Verdict:** **PASS**. The SEI survives, unaltered, in every AU, over both RTMPS and SRT ingest.

**Changes:**
- **ADR 0006** takes its "if it survives" branch. The R3 capture change can stamp NTP time into frames, the compositor can measure delay per frame, and the R4 calibration UX can be dropped. The ADR itself is not edited here.
- **Memory trap C14** says "SRT push from Fly never started a broadcast". With a valid stream it **did** start one: CF reported `ingestProtocol: srt`, connected, and the reader got 1109 AUs. C14 may have been the same audio-only defect (hypothesis, not verified).

---

## F-004: RTMPS → YouTube. VERDICT: the SILENT AAC track causes YouTube "Poor" ("not receiving enough video"); real audio is Excellent for 10 min (same session, scene, encode and route). Gaps up to 15 s keep the same broadcast; about 60–90 s of no data ends it

The full item (go-live, B-frames question, time to live, health) is owed to an owner session. The rows below are the owner's YouTube Studio observations during the item-5 pushes, relayed by the coordinator, set next to this session's own measurements.

| Run | London time | YouTube latency | x264 settings | Background | Egress (sent, every 10 s window) | Filler share (real video kbps) | Keyframe interval | Owner-observed health |
|---|---|---|---|---|---|---|---|---|
| item5-720 | 10:49:30–10:54:24 | Low | `bitrate=4000 vbv-buf-capacity=1000 option-string="nal-hrd=cbr:filler=1"` | ball camera via A + overlay | 4164–4168 kbps | not measured that run | 60 frames, configured | LIVE; **Poor** at 10:53 (3:58 elapsed): "YouTube is not receiving enough video to maintain smooth streaming. As such, viewers will experience buffering." |
| yt-owner2500 | 10:56:49–10:59:53 | Low | `bitrate=2500 option-string="nal-hrd=cbr:force-cfr=1"`, vbv default | ball camera via A + overlay | 2652–2656 kbps | **56.4 %** (1090 kbps) | 2.000 s (measured) | 10:57 "Stream is healthy — excellent" → **10:58 Error**, same message |
| yt-snow2500 | 11:04:00–11:07:03 | Low | same as yt-owner2500 | videotestsrc **snow** + overlay | 2651–2659 kbps | **0.0 %** (2501 kbps) | **0.200 s** (scenecut IDR every 6 frames) | 11:04 "Stream is healthy — excellent"; still Excellent at 11:06:27 (LIVE 2:20); no Error through the owner's last report |
| item5-1080 | 11:10:01–11:14:54 | **Normal** | `bitrate=6000 vbv-buf-capacity=1000 option-string="nal-hrd=cbr:filler=1"` | ball camera via A + overlay | 6178–6182 kbps | **77.7 %** (1342 kbps) | 2.000 s (measured) | 11:10 Excellent (LIVE 0:26) → **11:11 Error / Poor** (LIVE 1:47, frame f3032) |

**Our side was flat through every Poor flip.** Per-minute numbers for yt-owner2500:

| Minute | Egress kbps | Encoder fps (min) | Queue overruns | Machine cores avg |
|---|---|---|---|---|
| 10:57 | 2652.6–2655.5 | ≥29.17 (startup window) | 0 | 0.853 |
| 10:58 | 2654.3–2654.8 | ≥29.9 | 0 | 0.836 |
| 10:59 | 2654.3–2654.8 | ≥30.0 | 0 | 0.828 |

- In every run: media-time per wall second 1.0002–1.0003, and encoder speed ≥ 0.9963 in the worst 10 s window.
- CF Output status for the yt-owner2500 run: connected 09:56:50.9Z → `client_disconnect` 09:59:52Z, with no reconnect in between.

**Other observations:**
- **Each push that stops ends the YouTube broadcast.** The next push is a new broadcast. YouTube auto-prepares the next broadcast in a "No data" state: seen at 11:01 and 11:08, before the pushes.
- The owner changed YouTube latency from Low to Normal before item5-1080. Owner ruling: organisers will be told to use Normal.
- The snow picture is heavily blocky (incompressible at 2.5 Mbps). That is expected, not a defect.

**Cause (morning, HISTORY; superseded by the Redo: filler hypothesis REFUTED):**
- **Hypothesis SUPPORTED by owner observation through LIVE 2:20 of the snow run, not pinned.** The hypothesis: YouTube judges health on *real* video bits, so filler-padded CBR on low-entropy content reads as "not receiving enough video".
- Across bitrates 2.5/4/6 Mbps and latency Low/Normal, every filler-heavy run (56–78 % filler) went Poor about 1 min in. Only the 0 %-filler run stayed Excellent.
- **Open confound:** the snow run also had an IDR every 0.2 s against 2.0 s in all the others.
- **Deciding experiment owed (owner session):** real phone camera footage, with realistic motion, 2 s keyframes and scenecut as C09 will ship, while logging filler share. No further B pushes were spent disambiguating, per the coordinator.

**Changes (morning proposals, HISTORY; see the Redo for current proposals):**
- **C09 slate and frozen frame.** A static slate encodes to almost pure filler, which is exactly the case YouTube flags Poor. Proposal: composite a full-frame, low-amplitude, animated noise/grain layer, or a continuously moving element, under every slate and frozen-frame output, so real bits approach the target. The exact amplitude is unmeasured.
- **C09 acceptance test:** measure the **non-filler** bitrate (bytes outside NAL type 12) on a slate-only run. The current test, "bitrate within ±50 % (CBR filler keeps it near the target)", passes on the wire while YouTube flags Poor.
- **R2 restart policy (ADR 0004):** a compositor restart, or any output stop, ends the organiser's YouTube broadcast. Whether YouTube resumes the *same* broadcast after a short reconnect is unmeasured.

### Audio A/B: both legs DIRECT to YouTube; real audio vs silent AAC (2026-10-15, 17:17–17:40 London). THE DECIDING RUN

**Setup:**
- Session `36f00d16-…`, input `4c7298e7…`, fresh Machine `784ee345b637d8`.
- Scene: the phone on a **garden** (natural motion), mic on, for both legs.
- The fixture's YouTube output was disabled and B's output paused. Both legs pushed `SPIKE_YT_DIRECT_URL` (r2-c0).
- Identical C09 video encode on both legs. The **only** variable is `chain.py --audio`.
- Input tracks (`probe_tracks.py`): **audio PRESENT**, AAC (MPEG-4, ADTS), 48 kHz, stereo, about 47 frames/s; video H.264 1280x720 (framerate 0/1, i.e. variable).

| | **A1: real audio** (17:17:00–17:27:00) | **A2: silent AAC, the C09 control** (17:30:00–17:40:00) |
|---|---|---|
| Audio path | tsdemux audio → decodebin → audioconvert → audioresample → 48 kHz stereo → `voaacenc bitrate=128000` → aacparse | `audiotestsrc is-live=true wave=silence` → 48 kHz stereo → `voaacenc bitrate=128000` → aacparse |
| Audio kbps per 10 s window | 127.3–128.9 (median 128.0) | 127.8–128.3 (median 128.1) |
| AAC frames/s | 46.6–47.2 (median 46.9) | 46.8–47.0 (median 46.9) |
| Audio PTS − wall | +464 to +526 ms, flat (the camera's own timeline) | +1.4 to +3.3 ms, flat (the pipeline clock) |
| Video out PTS − wall | −46 to −132 ms, flat | −0.5 to −53 ms, flat |
| Egress | 4050–4310 kbps | 4106–4228 kbps |
| Encoder fps / overruns | about 30 / 4 (startup only, before the first audio caps) | about 30 / 0 |
| Video filler | 0.0 % | 0.0 % |
| send-q > 1 KB samples / max | 3 of 60 / 21.6 KB | 5 of 60 / 24.1 KB |
| TCP retransmits | 11 of 262k segments | 10 of 261k segments |
| Machine cores average | 1.136 | 1.083 |
| **Owner, r2-c0 health** | **Excellent from start to 17:27, the whole 10 min**; garden audio audible on the player | Excellent at 17:31:22 → **Poor at about 17:31:50 (about 1:50 in)**: "YouTube is not receiving enough video to maintain smooth streaming. As such, viewers will experience buffering." Stayed Poor through at least 17:38:11, no recovery |

- The broadcast ended in the 3 min gap between the legs (more than 30 s with no data), as expected.

**What the silent track looks like on the wire:**
- It is **not** degenerate by rate or timing: 128 kbps CBR, 46.9 frames/s, continuous PTS, no drift.
- It **is** degenerate by content. An offline encode of the same branch (`audiotestsrc wave=silence ! voaacenc bitrate=128000 ! aacparse`, 470 frames) gives 348 B/frame, but **94.7 % of the bytes are 0x00 and only 20 distinct byte values occur**. voaacenc pads digital silence to the CBR target, much as x264 pads with filler NALs.
- The same encode of `wave=pink-noise volume=0.001` (about −60 dBFS) gives 347 B/frame, 3.9 % zero bytes, and all 256 byte values: real coded content.

**Verdict:**
- **The silent AAC track causes YouTube's "Poor".** A1 and A2 shared one session, one scene, one encode and one route; only the audio changed, and only the silent leg went Poor (about 2 min in, persistent).
- **YouTube's wording "not receiving enough *video*" is misleading**: the cause tracks the *audio* track.
- This also explains every earlier Poor: all of them used the silent AAC, whatever the filler share, the route (CF or direct) or the channel.
- It does not establish *what* YouTube keys on. Candidates are a near-empty AAC payload (zero-padded frames) or digital-zero content.

**C09 recommendation (C09 is not edited):**
- **Pass the input's real audio through:** demux → decode → resample to 48 kHz stereo → AAC-LC 128 kbps, on the camera's own timeline. Verified Excellent for 10 min.
- **When the input has no audio:** do NOT send digital-zero AAC (that is the A2 failure). Exactly which substitute makes YouTube happy is **not shown by this data**. The bitrate, sample rate, frame rate and timestamp continuity of A2 were already normal, so changing them is not the fix.
- The candidate with on-wire evidence: **continuous low-level noise**, e.g. pink noise at about −60 dBFS (`audiotestsrc wave=pink-noise volume=0.001`) into the same AAC 128k 48 kHz stereo encoder. It yields fully coded frames, not zero padding. **Owed:** a 10 min direct push of that variant, plus a no-audio-track variant, before C09 fixes the rule.
- **C09's acceptance test:** add audio checks for frames/s ≈ 46.9, kbps ≈ 128, and a zero-byte share well below 94 %. The current test cannot see this defect.

**Also measured:** with `--audio camera`, the first 10 s show 4–54 encoder-queue drops while flvmux waits for the first audio caps. That is startup only; C09 should pre-roll or accept it.

**Superseded by this run:** the "structure" candidates listed in the direct-vs-CF A/B below (the silent `voaacenc` track was one of them; it is now confirmed). Leg 2 of that A/B (via CF), which had no owner health reading, is no longer needed for the verdict.

### A/B: direct to YouTube vs via CF Output, one phone session (2026-10-15, 15:51–16:14 London)

**Setup:**
- Session `dab4c6c3-…`, input `1602d142…`, fresh Machine `891e574f434d38`. Phone on the clock.
- The fixture's own YouTube output was disabled.
- Both legs used the identical C09 encode (720p30, 4000k CBR+filler, keyint 2 s, bframes 0) from the phone's SRT playback. The only difference was the destination env (`--out-env`).
- **Leg 1 DIRECT:** `SPIKE_YT_DIRECT_URL` (rtmps://a.rtmps.youtube.com:443/live2/<r2-c0 key>), with B's YouTube output **paused**.
- **Leg 2 VIA CF:** `SPIKE_B_RTMPS_URL`, with B's output resumed in the 3 min gap.
- `run-ab.sh` retries an early exit up to 3 times, 2 s apart. Both legs connected on attempt 1, and there were no `rtmp2sink` errors.
- A scheduling bug (a start time already in the past) launched both legs at 15:47:10 and killed them 15 ms later, before python started. No bytes were sent. `run-ab.sh` now refuses a start time in the past.

| | Leg 1 DIRECT, 15:51:00–16:01:00 | Leg 2 VIA CF, 16:04:00–16:14:00 |
|---|---|---|
| Egress per 10 s window | 4100–4206 kbps; at 15:55 3938–4239; at 15:56 3970–4369 | 4126–4194 kbps |
| Encoder fps after warm-up | ≥29.89 | ≥29.69 (16:11 camera dip to 26.5 fps) |
| Queue overruns | 0 | 0 |
| Filler, cumulative | ≤0.15 % (real video about 3994 kbps) | 0.0 % (real video about 3999 kbps) |
| Machine cores average / p95 | 1.185 / 1.363 | 1.142 / 1.232 |
| Output media − wall (encoder PTS vs arrival) | not instrumented (added after leg 1) | **−73 to −120 ms, flat across all 10 min** |
| Camera PTS − wall | not instrumented | **+395 to +473 ms, flat** |
| Encoded frames − 30×wall (PTS-independent) | not instrumented | **−1.2 to −2.6 frames, flat** |
| RTMPS socket send-q (`/proc/net/tcp`; no `ss` in the image) | not sampled | 0 in 56 of 60 samples; one spike of 128 KB at 16:06:30; otherwise ≤376 B |
| TCP retransmits, machine-wide | not sampled | 14 segments out of 263k (0.005 %) |
| **Owner, r2-c0 health** | **"3:55 PM Error — YouTube is not receiving enough video to maintain smooth streaming. As such, viewers will experience buffering."** (about 4 min in) | **pending from the owner** |

**Verdict (partial; leg 2's owner health is pending):**
- **The CF relay is NOT the cause.** The DIRECT push went Poor about 4 min in, with our side flat. That is the fourth clean-path Poor (14:18, 14:48, 15:55), each 1–4 min into a push.
- On leg 2, the stream we send is provably real-time: the media-time offset is constant, not growing, and the frame count matches 30×wall to within about 2 frames. Nothing queues on the socket (send-q about 0) and retransmits are negligible.
- So **"not receiving enough video" is not explained by us sending less than 1 s of media per second, nor by an upstream bottleneck from Fly lhr**, at the 10 s resolution measured.
- Rule for leg 2:
  - if leg 2 also went Poor, the remaining suspects are our stream's *structure* or YouTube itself;
  - if leg 2 stayed Excellent, the Poor flips are transient.
- Structure means anything still untested: FLV timestamps, the silent `voaacenc` track, `profile=main`, CBR+HRD signalling, and 2 s GOPs against YouTube's recommendation of 2 s *or less*.

**Next probes (not run):**
1. A real-audio or no-audio variant (the silent AAC track is a candidate).
2. An ffmpeg-encoded control from the same Machine and the same source, to split GStreamer's mux from x264 settings.
3. A 30–60 min soak to see whether Poor is persistent or episodic.

### Redo, on a clean channel (2026-10-15, 14:43–14:56 London). THIS SUPERSEDES the morning readings and rerun 1's YouTube side

**Setup:**
- Session `5346c02c-…`, input `3a358122…`, fresh Machine `7811141db04698`.
- The fixture's own YouTube output was **disabled**; Input B → `r2-c0` was **enabled** (coordinator-verified at 14:37).
- Phone on the laptop clock: real camera, low-motion scene.
- Settings: C09 (720p30, `bitrate=4000 vbv-buf-capacity=1000 nal-hrd=cbr:filler=1`, keyint 60, bframes 0).
- D restarts retried up to 3 times, 2 s apart, on an early exit. No retry was needed: every attempt connected first time.

**Step C, 14:45:00–14:50:00. Our side, per 10 s window, set against owner observations:**

| London | Egress kbps | Encoder fps | Camera fps | Overruns | Filler, cumulative | Owner, r2-c0 Stream health (verbatim) |
|---|---|---|---|---|---|---|
| 14:45:11 | (warm-up) | 26.66 | 19.43 | 0 | — | |
| 14:45:21–14:47:41 | 4144.4–4182.0 | 29.9–30.1 | 29.9–30.1 | 0 | **0.0 %** | |
| 14:47:51 | 4171.8 | 30.09 | 29.99 | 0 | 0.0 % | |
| 14:48:01 | 4156.2 | 29.9 | 30.0 | 0 | 0.0 % | **"2:48 PM Error — YouTube is not receiving enough video to maintain smooth streaming. As such, viewers will experience buffering."** |
| 14:48:11–14:49:41 | 4149.0–4171.3 | 29.9–30.12 | 30.0–30.1 | 0 | 0.0 % | |
| 14:49:51 | 4073.8 | 30.2 | 29.9 | 0 | 0.0 % | |
| (14:50:00.040 kill) | | | | | | **"2:50 PM Stream is healthy — Stream health is excellent."** The minute granularity cannot place it before or after the kill. |

- Total: 147 keyframes in 290 s (1.97 s); real video 4001 kbps; **filler 0.0 %**.
- Machine 1.107 cores average (p95 1.238, max 1.509).
- The composite snapshot at 14:47:28 showed the overlay over the camera, with the clock in shot. It is not committed.

**Verdict on the filler hypothesis: REFUTED for real camera content.** This is the third clean-path "Poor":
- 14:18 in rerun 1, at 0.3 % filler;
- 14:48 here, at **0.0 %** filler;
- each about 1–3 min into a push, with our egress flat at 4.15–4.18 Mbps.

Health recovered to Excellent by 14:50 with no change on our side.

The morning's filler-heavy runs also went Poor about 1–3 min in. Not concluded: the common factor may be the push's age, or the CF Output relay. **The CF Input B → YouTube leg is unmeasured**: CF exposes no Output egress metrics.

**Candidate next probe (not run):** push the same encode **directly** to `rtmp://a.rtmp.youtube.com/live2` with the r2-c0 key, bypassing the CF Output, for 5 min. That splits our encode from CF's relay.

**Step D. SIGKILL stop, then restart on a new RTMPS connection; owner observations (verbatim where given):**

| Gap | Kill | Restart | Attempts | Push after restart | Owner, r2-c0 |
|---|---|---|---|---|---|
| 5 s | 14:50:00.040 | 14:50:05.050 | 1 | 60 s; egress 3928–4372 kbps; 0 overruns | "2:50 PM … excellent" (see above) |
| 15 s | 14:51:05.424 | 14:51:20.434 | 1 | 60 s; egress 4107–4223 kbps; 0 overruns | **2:51 PM "No data", then "Healthy"** |
| 30 s | 14:52:20.296 | 14:52:50.305 | 1 | 60 s; egress 4137–4177 kbps; 35 encoder-queue overruns in the first (warm-up) 10 s window only | (same-broadcast confirmation for this gap pending) |
| final stop | 14:53:50.165 | — | — | — | **"2:54 PM No data — YouTube is not currently receiving data for this stream…"**; the broadcast **FINISHED by itself at about 14:55** |

- Owner at about 14:54: **"same broadcast in youtube"**. The r2-c0 broadcast survived the 5 s and 15 s gaps without splitting; the 30 s gap is pending owner confirmation.
- After the final stop, YouTube showed No data, then auto-ended the broadcast after roughly **60–90 s without data**. So the auto-stop grace is somewhere above 30 s (if the 30 s gap is confirmed) and around 90 s at most. One sample.
- **Contrast with the morning:** there, every push stop ended the broadcast. Those stops were followed by minutes of no data before the next push, and that morning's channel and binding is now in doubt (see the caveat). Possible differences: scheduled-stream binding, auto-stop setting, stop duration. Recorded, not concluded.

**Changes (proposals; nothing in crates/specs/ADRs is edited):**
- **ADR 0004 (restart policy):** a compositor outage, kill to first packet, must stay **well under about 60 s**, or the organiser's YouTube broadcast splits. A crash restart in about 5–15 s (cold start 7.5 s from a stopped Machine, F-001) is inside the grace in this sample. A fresh-host cold start (25 s) plus reconnect is likely still inside. One sample; the C13 runbook should say "about 1 min", not an exact limit.
- **C04/C09:** `rtmp2sink` connect needs **retry with backoff**. Rerun 1's single-shot D15 restart failed with "Socket I/O timed out" (finding stands).
- **C09 slate/frozen-frame grain:** it no longer carries the YouTube-health argument, because filler is refuted. Keep it only if C09 wants it for its own reasons.
- **C09's acceptance test** should still log non-filler kbps. It is cheap, and it is how this hypothesis was refuted.

### Rerun 1, on a clean channel (2026-10-15, 14:05–14:24 London)

**Clean-channel setup:**
- The fixture's own YouTube output (`YouTubeTest001`, a different channel) was **disabled** in CF by the owner.
- Only Input B → `r2-c0` reached YouTube.
- Session `afe2c9f9-…`, input `4e7a53cb9c112620625695e2da31e6b9`, fresh Machine `8747092f6575d8`.
- The phone stayed **on the laptop clock** (owner decision, so gaps could be timed). The run is therefore a *real camera, low-motion clock scene*, not natural motion.

**Step C**, 14:15:00–14:20:00 London:
- Settings: C09 (720p30, `bitrate=4000 vbv-buf-capacity=1000 nal-hrd=cbr:filler=1`, keyint 60, bframes 0).
- **Owner: health "Poor" at about 14:18 (message text pending).**
- Our side per 10 s window:

| London | Egress kbps | Encoder fps | Camera fps | Overruns | Filler share, cumulative |
|---|---|---|---|---|---|
| 14:15:10 | (warm-up) | 27.14 | 22.09 | 0 | — |
| 14:15:20 | 4208.5 | 30.09 | 29.99 | 0 | 0.0 % |
| 14:15:30 | 3929.9 | 28.2 | 25.0 | 0 | 2.4 % |
| 14:15:40–14:17:50 | 4137.8–4338.1 | 29.8–31.6 | 29.2–30.1 | 0 | 1.35 % → 0.22 % |
| 14:18:00 | 4002.4 | 30.1 | 30.0 | 0 | 0.26 % |
| 14:18:10 | 4198.6 | 30.0 | 30.1 | 0 | 0.33 % |
| 14:18:20–14:19:50 | 4015.3–4323.2 | 29.9–30.1 | 29.9–30.1 | 0 | 0.35 % → 0.31 % |

- Total: 149 keyframes in 290 s (≈1.95 s); real video 3984 kbps; **filler 0.31 %**. Machine 1.145 cores average (p95 1.424). There is no feature on our side at 14:18.
- The composite snapshot at 150 s (14:17:27) showed the overlay over the camera, with the laptop clock in shot. It is not committed.
- Its overlay text (CEF `Date.now()`) against the clock in the camera gives one in-chain camera→compositor reading: **3083 ms**. That is below the 3270–3320 ms decoder-out samples from earlier sessions. With n=1 on a different session, it is not used for the default.
- **Reading:** YouTube went Poor on a single clean stream at 0.3 % filler and a flat 4.0–4.3 Mbps. **This weakens the filler hypothesis**: filler share is not what flips health. It is not forced to a conclusion. Other candidates, all unmeasured:
  - RTMP timestamps or a cadence YouTube dislikes (wall-clock vs media-time stamping on `flvmux`);
  - the CF Output relay itself;
  - the silent `voaacenc` audio track;
  - YouTube's own ingest heuristics on a fresh key.

**Step D:**
- Push SIGKILLs, then a restart each time on a new RTMPS connection. Phone on the clock.
- The owner stopped the stg session at 14:21:33 London (`operator_stopped`), mid-D; the coordinator then ordered D aborted. By then D had already run to completion on the Machine.

| Gap | Kill | Restart | Push result | Camera | Owner (YouTube) |
|---|---|---|---|---|---|
| 5 s | 14:20:00.167 | 14:20:05.173 | ran 60 s; egress 3929–4317 kbps; 0 overruns | live (30 fps) | pending |
| 15 s | 14:21:05.191 | 14:21:20.196 | **FAILED at +7.9 s (14:21:28)**: `rtmp2sink` "Connection error: Socket I/O timed out"; 78 encoder-queue overruns while the sink stalled; chain exited | live until 14:21:33 | pending |
| 30 s | 14:22:20.217 | 14:22:50.223 | ran 60 s; egress 4164–4168 kbps; 0 overruns | **none**: input gone at 13:21:34Z; overlay only, filler 77 % | not a valid gap test |

- CF Input B status history keeps only the last connect: 13:22:51.684Z → `client_disconnect` 13:23:50.548Z.
- The D15 RTMPS timeout to CF Input B is one sample; the D15 restart in the morning run connected fine. **C09/C13 owe a reconnect retry with backoff on `rtmp2sink` connect timeout.** A single failed reconnect must not end the output.
- With the camera gone entirely (D30), the compositor kept 30.0 fps on the CEF branch alone.
- Step B (rerun): the phone input `4e7a53…` stayed connected in CF from 13:05:33Z until the owner's stop at 13:21:34Z. The watcher saw no gap over 300 ms from 14:09:09 to 14:13:00 London (6932 AUs).
- The watcher's FIRST connect STALLED at exactly 5.0 s before any AU arrived. Time to first AU on SRT playback is 2.6–4.4 s here, so a stall timer that starts at connect sits within about 0.6 s of the 5 s `input_stall_after_ms`. **C10 should start the stall timer from the first AU, or give connect its own budget.**

**CAVEAT (owner, 2026-10-15): the MORNING YouTube readings (the table at the top of F-004) are UNRELIABLE, not evidence.** They are kept as history; the Redo above supersedes them.
- The owner reports that two stream keys on the same YouTube channel appear not to work at the same time.
- The fixture's own `B_YoutubeTest` output, carrying the phone feed, and this spike's `r2-c0` key may share a channel.
- So every health flip in the table above, the "each stop ends the broadcast" observation, and the filler-vs-health hypothesis may be contaminated by the other key's stream.
- The Machine-side numbers stand: egress, filler share, keyframe interval, encoder fps, overruns.
- **Owed for the rerun:** each key bound to its own scheduled stream on its own channel (owner to confirm before starting); then the deciding C push (real camera motion, C09 settings, filler logged) and the D gaps (5/15/30 s, same vs new broadcast per gap). Owner-session step C ran on real phone footage at 4000 kbps with filler share 0.21 % (F-002 owner-session section). If health still goes Poor on a clean channel at that filler share, the filler hypothesis is dead.

---

## F-005: Cost, 720p30 vs 1080p30, full chain (2026-10-15)

**Method:**
- `chain.py` runs one process: SRT playback of Input A (camera feed as in F-002) → `tsdemux` → `avdec_h264` → scale, plus `cefsrc` (test page, js-signals off) → `compositor` → `x264enc`. x264 uses the C09 settings: veryfast/zerolatency, keyint 60, no B-frames, `vbv-buf-capacity=1000`, `nal-hrd=cbr:filler=1`. Then `flvmux` → `rtmp2sink` to Input B. Audio is silence through `voaacenc` 128k.
- Pushes to B ran 290 s each.
- `proc_sample.py` measured the process tree and the whole Machine every 1 s from +15 s.
- Dropped frames = leaky-queue `overrun` signals (qcam, qgfx, qenc). Encoder speed = encoder output fps / 30.
- Egress = rtmp2sink `out-bytes-total`. CF sent no RTMP acknowledgements: `out-bytes-acked` stayed 0.

**Raw:**

| | 720p30 @ 4000 kbps | 1080p30 @ 6000 kbps |
|---|---|---|
| **Machine cores avg / p95 / max (of 4)** | **0.832 / 0.902 / 1.224 (20.8 %)** | 1.113 / 1.193 / 1.275 (27.8 %) |
| Chain process tree cores avg | 0.797 | 1.093 |
| …GStreamer host process (decode + compositor + x264 + mux) | 0.695 | 0.987 |
| …CEF renderer + gpu + utility | 0.064 + 0.038 + 0.001 | 0.066 + 0.039 + 0.000 |
| Encoder speed, steady / worst 10 s window | 1.0002 / 0.9963 | 1.0002 / 0.9967 |
| Queue overruns (qcam/qgfx/qenc), total | 0 / 0 / 0 | 0 / 0 / 0 |
| Camera / CEF / compositor fps, steady | 29.997 / 30.0 / 30.0 | 30.003 / 30.0 / 30.0 |
| Tree PSS avg / max MB (RSS sum double-counts) | 494.8 / 500.3 (RSS 935) | 537.2 / 548.4 (RSS 968) |
| Machine memory used, max MB (of 8 GB) | 629 | 683 |
| Egress kbps, steady | 4166 | 6181 |
| Filler share of the video bits | (not measured that run) | 77.7 % |
| First camera frame / first encoded frame | 2.93 s / 0.36 s | 4.19 s / 0.37 s |

Two diagnostic 720p runs:
- 2500 kbps, ball camera: 0.84 machine cores.
- 2500 kbps, **snow** background (the worst-case, all-real-bits encode): **1.266** machine cores avg, p95 1.369, max 1.42.

The snapshot of the composite (overlay over camera) was checked visually. It is not committed.

**Verdict:** **PASS.** 720p30 uses 0.83 of 4 cores against a 3.0 gate, with 0 dropped frames and encoder speed ≥ 0.996. 1080p30 is recorded: 1.11 cores and also clean.

**Changes:**
- **ADR 0005** (720p default, 1080p behind a flag) holds on cost grounds. 1080p costs +0.28 cores.
- performance-4x leaves about 2.7–3.1 cores of headroom. A smaller size is plausible but not measured; that is for C16 to measure if wanted.
- C09's encoder settings sustain real time with margin. The filler question is in F-004.

**Teardown:** done. 0 Machines remain; see the record at the end of this file.

---

## F-007: what the Machines API reports while `restart: on-failure` restarts a crashed Machine (2026-10-15)

**Question (R2003):** the web polls `GET /v1/apps/<app>/machines/<id>`. If it reads `stopped`/`failed` in the gap between a crash and the in-place restart, it declares the runner lost and destroys it mid-restart.

**Method:**
- `spike/fly_restart_probe.py` drives the Machines API directly. The token comes from `fly auth token`, captured in-process, never in argv or logs, and refreshed on 403: these tokens expired after about 4.7 min.
- App `seazn-relay-spike`, region `lhr`, `alpine:3.20`, guest shared-cpu-1x / 256 MB, `auto_destroy: false`.
- Each Machine runs `sh -c 'sleep N; exit C'` under `restart: {policy: "on-failure", max_retries: R}`.
- Polling: every 200 ms (M1, M3) or 1 s (M2). Each poll records `state` and every new `events[]` entry: type, status, timestamp, `request.exit_event`, and `request.restart_count`.
- No 429s in 1,110 polls. Three polls on M1 failed client-side (HTTP 0, 18:13:54–18:14:18).

| Machine | exit code | sleep | max_retries | What it answers |
|---|---|---|---|---|
| M1 `89499ea6e97298` | 1 | 20 s | 2 | gap states, gap ms, exhaustion; then a manual `start` 5 min 23 s after exhaustion |
| M3 `7849236f05e078` | 78 | 10 s | 2 | (a) is 78 restarted? |
| M2 `1859231fd01668` | 1 | 330 s | 1 | does the retry budget reset after a long healthy run? |

Total Machine run time ≈ 13.7 min. All three were destroyed, and `fly machine list` shows "No machines are available".

### Results

**1. States in the gap.** The sequence is: crash → `exit` event (status `stopped`, `exit_event.restarting: true`) → about 110 ms → `restart` event (status `starting`) → about 1.3 s → `start` event (status `started`).
- The polled `state` reads **`stopped` for about 110 ms**, then **`starting` for about 1.2–1.4 s**, then `started`.
- At 200 ms polling, 1 of 4 restarting gaps was caught as `stopped` (M3, 18:12:33.913); the other 3 went straight to `starting`.
- **`failed` was never seen.**
- So **a poller CAN read `stopped` mid-restart**: rarely, at about a 110/200 chance per crash with 200 ms polling, and more often with slower polling that lands in the window.
- The only reliable discriminator is the latest `exit` event's **`request.exit_event.restarting`**: `true` on every restarting exit, `false` on the final one. `restart_count` is also present on the `exit` event's `request`.

**2. (a) Exit code 78 IS restarted.** `on-failure` treats 78 like any non-zero code: two restarts with `restarting: true`, then `restarting: false` once `max_retries` was spent (M3). **R2 cannot rely on exit 78 to mean "don't restart" under `on-failure`.** Exit 0 under `on-failure` was **not** tested.

**3. (b) Gap from exit to the restarted process:**

| Crash | exit → `restart` event | exit → `start: started` event |
|---|---|---|
| M1 #1 (exit 1) | 109 ms | 1407 ms |
| M1 #2 (exit 1) | 109 ms | 1507 ms |
| M3 #1 (exit 78) | 116 ms | 1426 ms |
| M3 #2 (exit 78) | 113 ms | 1463 ms |
| M2 #1 (exit 1) | 108 ms | 1417 ms |
| M2 #2 (exit 1) | 113 ms | 1400 ms |

Median: about **110 ms** to `restart`, about **1.42 s** to `started`. These are Fly event timestamps; the process's own start is a few ms later.

**4. (c) After `max_retries` is exhausted:**
- The final exit carries `restarting: false` and `restart_count: 2`, and the Machine **stays `stopped`**: never `failed`, with no automatic restart over 5 min 23 s.
- A **manual `POST /start`** 5 min 23 s later ran the process again. Its crash 20 s later was **not** restarted (`restarting: false`, `restart_count` still 2), so a manual start does not refill the budget.
- **But the budget refills after a long healthy run.** On M2 (max_retries 1), a crash after 330 s healthy was restarted (`restart_count: 1`). The next crash, after another 330 s healthy run, was **also** restarted, again with `restart_count: 1`. The reset threshold lies somewhere below 5.5 min of healthy run; it is not measured more finely.

### Verdict and changes (proposals; R2003, C13 and ADR 0004 are not edited)

- **R2003's in-place restart survives only if the web's "lost" rule ignores a `stopped` whose latest `exit` event says `restarting: true`.** Proposed rule:
  - lost = `state ∈ {stopped, failed, destroyed}` **and** the latest `exit` event's `request.exit_event.restarting == false`;
  - or `starting` for more than about 10 s (observed about 1.4 s).
  - A plain "`stopped` ⇒ lost" rule is racy (1 in 4 gaps hit at 200 ms).
- **Exit 78 is not a no-restart signal under `on-failure`.** For a refusal that must not restart, the options are:
  - (i) the web destroys a Machine whose latest exit has `exit_code 78`, without waiting for the restart;
  - (ii) the runner exits **0** on refusal; not tested here, so that is owed;
  - (iii) `policy: "no"` plus a web-driven restart.
- **The retry budget is per crash burst, not per lifetime.** A runner that crashes rarely, at least about 5.5 min apart, is restarted indefinitely. A runner that crash-loops gets exactly `max_retries` restarts, then sits in `stopped` with `restarting: false`, and the web must reap it.
- `fly auth token` tokens expire after about 5 min. Any long-lived poller must use a real deploy or org token.

## F-008: Fly stop signal precedence, and the `stop_config` timeout form (2026-10-15)

**Questions (R2004 PR 0, coordinator decision I4):** web creates composed Machines with `stop_config: { signal: "SIGTERM", timeout: <grace> }`, and sends its own stop with an explicit `signal: "SIGINT"`.
1. Does the stop request's explicit `signal` win over `stop_config.signal`? If not, our own stop arrives as SIGTERM, C06 drains at once, and the Final scene never shows.
2. The docs type `stop_config.timeout` as an integer of nanoseconds. Which form reads back as sent?
3. Does a stop that names no signal and no timeout (the shape of a stop Fly starts) wait `stop_config.timeout`, instead of Fly's 5 s default?

**Method:**
- App `seazn-relay-spike`, region `lhr`, image `busybox:1.36` (Docker Hub library), guest shared-cpu-1x / 256 MB, `restart: {policy: "no"}`, `auto_destroy: false`.
- Two entrypoints:
  - **trap:** `sh -c 'trap "exit 3" INT; trap "exit 4" TERM; while :; do sleep 1; done'`. The exit code names the signal that arrived.
  - **ignore:** `sh -c 'trap "" INT; trap "" TERM; while :; do sleep 1; done'`. Only SIGKILL ends it, so the stop's duration shows which timeout Fly used.
- The Machines API is driven directly (`api.machines.dev/v1`):
  - runs 1 and 2: from one-off Node scripts;
  - run 3: from the committed opt-in legs in `seazn.club` #971 (`fly-client.live.test.ts` leg 5, and the R2004 PR 0 leg in `runner-fly.live.test.ts`).
- The token came from the environment only, never argv or logs.
- What each reading is:
  - **Read-back:** `config.stop_config` on the create response, and on a later `GET /machines/{id}`.
  - **Exit code:** the newest `exit` event's `request.exit_event.exit_code`, from `GET /machines/{id}/events`.
  - **Duration:** the client's wall clock, from `POST /stop` until `GET /wait?state=stopped` answers.
- 8 Machines in 3 runs (≈ 20:56Z, 21:00Z and 21:14Z), about 5 Machine-minutes in all. Each Machine was destroyed with `force=true` and re-read as `destroyed`. The app listed 0 Machines after each run, and again at 21:15Z.

### Results

**1. The explicit stop signal wins.** Both Machines had `stop_config.signal` SIGTERM and the trap entrypoint:
- `POST /stop {"signal":"SIGINT","timeout":"20s"}` exited **3** (the INT trap);
- `POST /stop {}` exited **4** (the TERM trap).

Run 1 (a script) and run 3 (the committed leg) agree, and every one of these exits carries `requested_stop: true`. So **our stop arrives as SIGINT, and a stop that names no signal gets `stop_config.signal`**.

**2. Only the string form round-trips.**
- `timeout: 20000000000` (the docs' nanoseconds) is accepted, and both the create response and the GET answer `"20s"`.
- `timeout: "20s"` answers `"20s"`.

The docs' unit is right (20 × 10⁹ ns is `"20s"`), but Fly normalises the value to a duration string. Web sends `"<n>s"` (`flyDuration()` in `seazn.club` `fly-client.ts`), the same spelling its stop request's `timeout` already used.

**3. A signal-less, timeout-less stop waits `stop_config.timeout`.** These runs used the ignore entrypoint and `POST /stop {}`:

| `stop_config` | stop → `stopped` |
|---|---|
| none (the control) | 11.97 s |
| `"20s"` | 31.99 s, 27.92 s |

Fly's default is 5 s (`kill_timeout`), so in both cases `stopped` lands about 7–12 s after the timeout. That is one control sample and two with `stop_config`. This measures stop → `stopped`, not the instant of SIGKILL.

**4. No exit event after a forced kill.** In all 3 ignore runs, the Machine's events carried **no `exit` event** once it read `stopped`. A compositor the host has to SIGKILL therefore reaches web as a stopped Machine with no recorded exit: F-007's "unrecorded" shape (`runner-fly.ts` `observe`).

**5. The adapter's own create.** A Machine created by `seazn.club`'s `FlyRunner.create` (nginx image, `auto_destroy: true`, `restart: on-failure`, max_retries 2) read back `{"timeout":"20s","signal":"SIGTERM"}`. Web's STOP effect (explicit SIGINT, `"20s"`) ended it with exit 0 and `requested_stop: true`.

### Verdict and changes

- **I4 holds as designed.** Our stop (explicit SIGINT) reaches C06 as SIGINT and can hold the Final scene. A stop that names no signal arrives as SIGTERM and drains at once, within the grace instead of 5 s.
- **R2004 PR 0 shipped this way** (`seazn.club` #971, in review): `stop_config: { signal: "SIGTERM", timeout: "20s" }` on every composed create; our stop names SIGINT on every attempt, a retried one too; `RUNNER_STOP_GRACE_SECONDS` = 20 (D15).
- **A web host driver must always name its stop signal.** Leaving it out silently becomes the platform's SIGTERM.
- **C06 and C12 can't rely on an exit record after a SIGKILL.** Web reads that run as unrecorded. The drain has to finish inside the grace: hold 10 s + drain 7 s + 3 s headroom = 20 s.
- **Not measured:**
  - Whether Fly's own platform stops (`fly deploy`, host maintenance, `auto_stop_machines`) use `stop_config` exactly as a signal-less API stop does. The docs say `kill_signal` / `kill_timeout` cover those stops, and `stop_config` is the Machine-config form of the same pair.
  - The teardown overhead beyond these three samples.

### F-008 appendix: raw readings

Times are 2026-10-15, UTC. "—" means the reading wasn't logged.

| Run | Machine | Entrypoint | `stop_config` sent | Read back (create / GET) | Stop request | Stop → `stopped` | Newest `exit` event |
|---|---|---|---|---|---|---|---|
| 1 (≈ 20:56Z) | A `81744df943d018` | trap | `{SIGTERM, 20000000000}` | `{"timeout":"20s","signal":"SIGTERM"}` / same | `{"signal":"SIGINT","timeout":"20s"}` | ≈ 1.0 s | `exit_code 3`, `guest_exit_code 0`, `guest_signal -1`, `signal -1`, `requested_stop true`, `oom_killed false`, `restarting false`, exited 20:56:49.149Z |
| 1 | B `811d23eb5d9218` | trap | `{SIGTERM, "20s"}` | `{"timeout":"20s","signal":"SIGTERM"}` / same | `{}` | ≈ 1.3 s | `exit_code 4`, `guest_exit_code 0`, `guest_signal -1`, `signal -1`, `requested_stop true`, `oom_killed false`, `restarting false`, exited 20:56:50.629Z |
| 1 | C `86d552ae119458` | ignore | `{SIGTERM, "20s"}` | `{"timeout":"20s","signal":"SIGTERM"}` / same | `{}` | 31 990 ms | none |
| 2 (≈ 21:00Z) | D `8d96953b119258` | ignore | none | absent / absent | `{}` | 11 968 ms | none |
| 2 | E `683ddd21ce22e8` | ignore | `{SIGTERM, "20s"}` | `{"timeout":"20s","signal":"SIGTERM"}` / same | `{}` | 27 924 ms | none |
| 3 (≈ 21:14Z), leg 5 | "ns" (id —) | trap | `{SIGTERM, 20000000000}` | — / `{"timeout":"20s","signal":"SIGTERM"}` | `{}` (raw POST, HTTP 200) | — | `exitCode 4`, `oomKilled false`, `requestedStop true` |
| 3, leg 5 | "str" (id —) | trap | `{SIGTERM, "20s"}` | — / `{"timeout":"20s","signal":"SIGTERM"}` | `{"signal":"SIGINT","timeout":"20s"}` (production client) | — | `exitCode 3`, `oomKilled false`, `requestedStop true` |
| 3, `runner-fly` leg | `d8d3741b541008` (nginx `1.27-alpine`, `FlyRunner.create`) | the image's own | `{SIGTERM, "20s"}` (adapter) | — / `{"timeout":"20s","signal":"SIGTERM"}` | `{"signal":"SIGINT","timeout":"20s"}` (`FlyRunner.stop`, from the domain's STOP effect) | — (`auto_destroy`; wait `destroyed` ok) | `exitCode 0`, `oomKilled false`, `requestedStop true` |

Run 3's leg-5 rows are the leg's parsed output (`exitInfoFrom`), not the raw event. Runs 1 and 2 waited 3 s after `started` before the first stop, and run 3 waited 2 s, so the shell's traps were installed first.

## F-009: CEF loopback phase channel (C11 Step 0, 2026-10-16). OUTCOME (a) for a page with no CSP: a public HTTPS page in CEF 139 reaches `http://127.0.0.1:7419` with C12's headers and no flags. The real overlay page's CSP omits the endpoint (R2005 change 8)

**Question (C11 amendment 2026-10-15, Step 0; spec §4.3 D10, §6 C11 row):** can an HTTPS page from a public origin, in CEF `139.0.28+g55ab8a8+chromium-139.0.7258.139`, `fetch("http://127.0.0.1:7419/phase?after=0")`, with and without the PNA preflight headers? R2005's phase hook starts in the channel this picks.

**Method:**
- **Image:** the F-001 build, unchanged: `registry.fly.io/seazn-relay-spike:deployment-01M4ZBW92TN0GPMPC9S0KK5TQ3@sha256:15890e25c22a470bd3e79fb690d9ee1d36844fc0a944d96238fed824adcd73a3` (`spike/t00` @738eb88; `spike/Dockerfile` is identical at the branch head). Its `/opt/gstcefsrc/BUILDINFO`, printed by every run: `gstcefsrc_commit=204b01b02df23d143316b11e51516171ab7fae0a`, `CEF_VERSION "139.0.28+g55ab8a8+chromium-139.0.7258.139"`.
- **Host:** app `seazn-relay-spike`, region lhr, performance-4x / 8 GB (F-001's guest), `restart: no`. **One fresh Machine per run**, so no preflight cache, profile or process survives between runs. The two harness files reached the Machine with `fly machine run --file-local`; the run went over `fly ssh console -C`; the Machine was destroyed straight after. flyctl used its own stored login: no token on argv or in any log.
- **Origin:** public HTTPS, as in production. `harness/pages/` was served from the laptop with `python3 -m http.server 8080 --bind 127.0.0.1` and exposed with `cloudflared tunnel --url http://127.0.0.1:8080` (cloudflared 2026.9.3, darwin-amd64: the binary matched the release's published SHA-256, and it is code-signed, team `68WVV388M8`). The page URL was `https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html`, served as HTTP/2 by `server: cloudflare` from 104.16.230.132 / 104.16.231.132 (public addresses).
- **Inside each Machine (one network namespace):** `Xvfb :99 -screen 0 1920x1080x24`, then `harness/phase-probe-server.py` (bound to 127.0.0.1:7419), then the brief's line, as uid 10001 (`spike`, the non-root user):
  `GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=<page> listen-for-js-signals=false max-video-framerate=30/1 [chrome-extra-flags=<flag>] ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink`.
  Stop: 20 s after the server's first stdout line, or 30 s after the gst start if it never logs (it always logged, 3 858–3 866 ms after the gst start).
- **The page** logs every result on F-001's console channel (`console.info`, read from `cefconsole`): `seazn:probe=ok:<body>` or `seazn:probe=blocked:<name>:<message>`, as the brief writes them, plus `seazn:probe-page=<origin> secure=<isSecureContext>` before the fetch (proof that the channel works) and `seazn:probe-ms=<n>`. Its fetch is the brief's bare `fetch(url)`, with no init.
- Runs at 2026-10-16 06:58–07:05Z.
- **The bytes that ran (sha256; review M1):**
  - `harness/phase-probe-server.py`: `344ec82f55c5fde47c99ea3a395a4ec84689ef65d4e1f6e27c200ce3593c2638`
  - `harness/pages/phase-probe.html`: `a4de5c3ffb1fd6d95a40e41174beedc7d23f39e800adeafc08c4b0e865e7efb8`
  - the scratch runner `run-step0.sh` (uploaded with `--file-local`, never committed; printed verbatim at the end of the appendix): `7a369f253795fb1d2933fdb7caaffa0c6f54705860afc0b758461448bfd7ba87`

  The first two equal their blobs at `ce6ac43`. All three were last written at 06:54–06:57Z, before the first run at 06:58:17Z, and stayed unchanged until that commit. The server has since gained `cors=` logging ("Evidence for the headers sent"), so its current bytes differ.

### Results

| Run | Server | `chrome-extra-flags` | Preflight seen by the server | Then | `seazn:probe=` (verbatim) | `probe-ms` |
|---|---|---|---|---|---|---|
| V1 | `--variant none` | none | `OPTIONS`, `acrpn=true acrm=GET`; answered 204 with **no** CORS header | the GET **still arrived, 1 ms later**, held 2 000 ms, answered 200 | `seazn:probe=blocked:TypeError:Failed to fetch` | 2010 |
| V2 | `--variant pna` | none | `OPTIONS`, `acrpn=true acrm=GET`; 204 with ACAO, ACAM, ACAPN, `Vary` | GET, held 2 000 ms, 200 | `seazn:probe=ok:{"phase":"live","seq":1}` | 2055 |
| V3a | `--variant pna` | `disable-features=LocalNetworkAccessChecks` | same as V2 | same as V2 | `seazn:probe=ok:{"phase":"live","seq":1}` | 2058 |
| V3b | `--variant pna` | `disable-features=PrivateNetworkAccessRespectPreflightResults` | same as V2 | same as V2 | `seazn:probe=ok:{"phase":"live","seq":1}` | 2055 |
| V4 | `--variant pna --hold-ms 9000` | none (V2 passed) | same as V2 | GET **held 9 000 ms**, 200 | `seazn:probe=ok:{"phase":"live","seq":1}` | 9048 |

1. **V2 passes. That is outcome (a).** The fetch succeeds with C12's headers and no CEF flag. V4 shows that a long-poll held 9 s (inside `RELAY_PHASE_LONG_POLL_MAX_MS` = 10 000) comes back intact through CEF: `held_ms=9000`, the reply body unchanged, `probe-ms=9048`.
2. **V1 fails on plain CORS, not on Private Network Access.** Its one non-probe console line, at ERROR:
   `Access to fetch at 'http://127.0.0.1:7419/phase?after=0' from origin 'https://disclosure-raymond-functioning-structured.trycloudflare.com' has been blocked by CORS policy: No 'Access-Control-Allow-Origin' header is present on the requested resource.`

   The rejection came from the GET's answer, after the hold, and not from the preflight (review M2):
   - **The timing:** V1's `probe-ms=2010` is the server's 2 000 ms hold plus about 10 ms. A rejected preflight would have failed the fetch before the GET was ever sent, without waiting on the hold.
   - **The wording:** the message names "the requested resource". A preflight rejection reads "Response to preflight request doesn't pass access control check" instead.
3. **CEF 139 sends the PNA preflight but does not enforce its result.** Every fetch was preceded by an `OPTIONS` carrying `Access-Control-Request-Private-Network: true`, even this simple GET with no custom header. In V1 that preflight came back with no `Access-Control-Allow-Origin` and no `Access-Control-Allow-Private-Network`, and Chromium sent the GET anyway, 1 ms later (`at=1792133902797` then `at=1792133902798`). A respected preflight would have stopped the GET there. So what CEF 139 actually checks is the GET's `Access-Control-Allow-Origin`; the PNA header is not checked yet. That also explains V3a and V3b: disabling either feature changed nothing, because nothing was blocking. Whether the `chrome-extra-flags` switch reached Chromium is **not witnessed**: gstcefsrc logs "Adding switch with value" in category `cefsrc` at INFO, which the brief's `GST_DEBUG=cefconsole:4` leaves off.
4. **No mixed-content, "Private Network" or "Local Network" console line, in any run.** There were 23 `cefconsole` messages across the 6 runs: 4 in V1, 3 each in V2–V4, and 7 in V5. They are the probe's own lines plus V1's CORS line.

   `http://127.0.0.1` from an HTTPS page is not blocked as mixed content, and V2's success is the evidence: `127.0.0.1` is a potentially trustworthy origin. `secure=true` describes the page's own context, not the subrequest (review M3).

   **This holds for the probe page only, which had no CSP.** The real overlay page is expected to log a CSP report line on every poll ("The real overlay page's CSP", below).
5. **The `Origin` header, as the server saw it:** `https://disclosure-raymond-functioning-structured.trycloudflare.com`. That is scheme + host, with no port (443 is the default) and no trailing slash, the same form on the preflight and the GET. C12's exact-origin check (`Env.app_origin`) must serialize APP_URL the same way: it omits a default port and keeps a non-default one.
6. **The request headers** were the same in every run, key by key. The appendix prints every request line in full. The preflight carried `Access-Control-Request-Method: GET`, `Access-Control-Request-Private-Network: true`, `Origin`, `Sec-Fetch-Mode: cors`, `Sec-Fetch-Site: cross-site` and `Sec-Fetch-Dest: empty`, and **no** `Access-Control-Request-Headers`. The GET carried the same `Origin` and `Sec-Fetch-*` values. The User-Agent is `… Chrome/139.0.0.0 …`, and `sec-ch-ua` reads `"Chromium";v="139"`.
7. **Every long-poll pays its own preflight** (V5 below: 4 GETs, 4 `OPTIONS`). Fetch's preflight cache matches on the URL, and `?after=<seq>` changes on every poll, so an `Access-Control-Max-Age` would not save any of them. On loopback the cost is about 1 ms per poll. C12's `PHASE_HTTP_MAX_CONCURRENT` = 4 counts a preflight as a request like any other. The page's polls are sequential, and each preflight was answered before its GET arrived, so one page holds one request open at a time.
8. **Noise, for C12's log budget, is not a defect here.** Each run has 19 `dbus` errors (F-001 saw these) and 1–4 `google_apis/gcm` errors, for example `Registration response error message: PHONE_REGISTRATION_ERROR` and `Error code: 401  Error message: Authentication Failed: wrong_secret`. These are replies, so Chromium's GCM client reached Google's servers from inside the Machine. Whether a switch silences it was not measured.

### V5: the probe server's `--script` mode through CEF (the stand-in the compatibility step uses)

`--variant pna --script waiting:5000,reconnecting:5000,final:5000`, with the page at `?follow=1`: after its first answer it keeps long-polling `?after=<seq>` until `live`. Same origin and image, no flags.
- **The script clock starts at the first GET:** `transition waiting seq=1 at=1792134262972`, in the same millisecond as that GET's own `request` line, about 4.3 s after the server started (the runner starts it 0.5 s before gst-launch, which started at `1792134259213`).
- **Each later transition comes exactly 5 000 ms after the one before:** reconnecting `…267972`, final `…272972`, live `…277972`.
- **Each held poll is answered at the change:** `held_ms=4948/4951/4952`.
- **Each poll was preceded by its own preflight**, and every preflight and GET carried the same headers as V2.
- **The page logged the whole sequence:**

  ```
  seazn:probe=ok:{"phase":"waiting","seq":1}
  seazn:probe-follow={"phase":"reconnecting","seq":2}
  seazn:probe-follow={"phase":"final","seq":3}
  seazn:probe-follow={"phase":"live","seq":4}
  seazn:probe-follow-done=live:4
  ```

This is **not** the compatibility step (next section). It proves only that the stand-in serves the script the way that step needs, through a real CEF, from a public origin.

### The real overlay page's CSP (review I1): outcome (a) does not carry over unchanged

- **The probe page had no CSP.** `python3 -m http.server` sends none. The real overlay page has one, built by
  `seazn.club` `apps/web/src/proxy.ts` (`c64e0a217`):
  - `cspHeader` (`:45`) builds one policy for every page request.
  - Its `connect-src` (`:58`) is `'self' https://api.stripe.com https://*.supabase.co wss://*.supabase.co
    https://*.ingest.sentry.io https://*.sentry.io`, plus PostHog's origin. It has no `http://127.0.0.1:7419`.
  - `/overlay` is outside the `CACHEABLE_PUBLIC` carve-out (`:14`, `/shared|/embed`), so `CSP_MODE=enforce`
    would enforce the policy there.
- **Live headers**, read 2026-10-16 with `curl -D -` on `/overlay/fixtures/00000000-0000-0000-0000-000000000000`.
  The answer is a 404, but the proxy still sets the policy. `https://seazn.club`, `https://stg.seazn.club` and
  `https://seazn-club-stg.fly.dev` all send `content-security-policy-report-only` with that `connect-src`. So the
  policy is report-only today, and `design/v1/DEFERRED.md:36-40` still plans the flip to enforce.
- **Today:** the fetch is not blocked. Each poll is expected to log a `[Report Only] Refused to connect …` console
  error, which reaches `cefconsole` at ERROR. That follows from CSP semantics and was not observed: no run has
  loaded the real page in CEF yet.
- **Once `CSP_MODE=enforce`:** every loopback fetch is blocked in the renderer before it reaches the network. The
  hook falls back to B after 3 failures, sticky for the page's life. Nothing errors; only `phaseChannel: web` on
  the beat shows it.
- **Also unverified (review I1):** under an enforced policy, `upgrade-insecure-requests` (`:65`) might rewrite the
  URL to `https://127.0.0.1:7419`.
- **Coordinator decision (2026-10-16): R2005 owns it** (R2005 brief, "Amendment 2026-10-16", change 8):
  - add `http://127.0.0.1:7419`, derived from `RELAY_PHASE_PORT`, to `connect-src` for the `/overlay` tree only,
    never site-wide;
  - a test that the `/overlay` policy carries it and other routes' policies don't;
  - a mutant each way;
  - an enforce-mode e2e (E8) that also settles the `upgrade-insecure-requests` question.

  The G1 compatibility step on real stg is the first CEF run that can see the policy (Owed 21).

### Evidence for the headers sent (review M1)

- **What the run lines show.** These runs' `reply` lines log status, hold, write result and body, but not the
  headers sent. So two claims rest on the server's code at the hashed bytes above, pinned by its self-test:
  - V1's "answered 204 with **no** CORS header" (`test_none_variant_sends_no_cors_header_at_all`);
  - V2's header set (`test_pna_variant_answers_the_preflight_and_the_get_for_the_request_origin`).
- **CEF's own reaction agrees:** V1's console line names a missing `Access-Control-Allow-Origin`.
- **The server now logs `cors=<the CORS and Vary headers sent, as JSON>` on every `reply` line,** so the
  compatibility step's record carries them. `test_every_reply_logs_the_cors_headers_it_sent` compares that log
  with what the client actually received. A mutant that logs `{}` turns it red.

### Compatibility step (coordinator decision G1): NOT RUN; both legs owed

The brief's pre-condition is "R2005 is live on stg". It isn't, and the fallback (the template leg alone) can't run either:
- `seazn.club` main at `c64e0a217` (2026-10-16) has **no** `seazn:ready` and no `use-programme-phase` anywhere under `apps/web/src`. R2005 is unmerged, so the page sends no readiness signal and has no scene layer.
- `OVERLAY_TEMPLATES` and `?template=` belong to R2007 PR A, which is open and unmerged (`seazn.club` #974).
- `stg.yml` deploys only on a push to main. So stg can't carry either one yet.
- The template leg asserts "the template renders and `seazn:ready` arrives". With no `seazn:ready` and no template id to load, it would assert nothing. Running it now would be a vacuous record, so it was not run.

Owed (`docs/_INDEX.md` Owed 21):
- **The template leg:** C11, after Step 4, once R2005 and #974 are on stg.
- **The scene leg** (`--script waiting:5000,reconnecting:5000,final:5000`): C11, if R2005 is live on stg by then; otherwise C17.

Outcome (a) does not push the scene leg to C17: with loopback primary, the probe server can drive the scenes. That run is also the first to load the real page, with its CSP, in CEF.

Not investigated, but it bears on "on stg": `stg.yml`'s last three runs on main (02:19Z, 02:48Z and 06:41Z, 2026-10-16) concluded `failure`.

### Verdict and changes

**Outcome (a), for a CSP-free page. Loopback is primary, with C12's headers and no CEF flags.** So:
- **R2005:** the phase hook starts in `loopback`, and fallback B (web) stays the fallback: 3 consecutive loopback failures, then sticky (Decision log, 2026-10-15). The `_FINDINGS.md` id to cite at the constant is **F-009**.
  - **R2005 does change (review I1; coordinator decision 2026-10-16).** Outcome (a) was proven for a page with no CSP. The real page's `connect-src` must allow the endpoint on `/overlay` only ("The real overlay page's CSP" above; R2005 change 8).
  - **The hook's fetch must stay CORS-simple (review M5; R2005 change 9):** a GET, no request headers, no `Content-Type`. An `AbortController` signal is fine. A non-safelisted header triggers an enforced CORS preflight, and C12 sends no `Access-Control-Allow-Headers`, so it would be blocked.
- **C11 Step 3:** no `chrome-extra-flags`. The flag assertion in `ready_signal_arrives_with_its_generation` belonged to outcome (b), so it is not added.
- **C12 (`phase_http.rs`):** ship the headers as briefed. ACAO on the GET (the exact app origin) is what CEF 139 checks today. The preflight's `Access-Control-Allow-Private-Network: true` is not checked (result 3). It stays because it is harmless and the brief requires it, **not** as future-proofing: Chromium put PNA preflight enforcement on hold (October 2024) and replaced it with Local Network Access, a permission that no response header satisfies (review I2).
  - Derive `allowed_origin` in the form result 5 observed.
  - It must equal the origin of the `overlayUrl` the job carries. stg serves `/overlay` on two hosts (review M6; C12 brief, amended 2026-10-16).
- **Re-run Step 0 on every CEF bump** (any `docker/GSTCEF_COMMIT` change, C12/C16). This rests on Chromium's public docs and vendor notes (review I2 sources); none of it was measured here:
  - **Local Network Access** gates requests to loopback behind a **permission prompt**, starting with **Chrome 142** (2025-10-28). That is three milestones above this pin (139).
  - gstcefsrc at this pin implements no `CefPermissionHandler`. A windowless CEF has no user to answer the prompt. The reviewer's understanding, unverified, is that an ignored prompt means a denial.
  - **So a bump past 142 is expected to land in outcome (c)**, unless something grants the permission.
- **What to read on the re-run.** V1 tells you nothing: with no CORS headers, it fails on CORS in any release. Under LNA there may be no PNA preflight at all, so result 3's "the GET still arrives after a failed preflight" may have nothing to read. Read instead:
  - **V2's `seazn:probe=` line.**
  - **Whether ANY request (OPTIONS or GET) reached the server.** None means the page was blocked before the network, which is the signature of a permission denial (or of a CSP).
  - **`probe-ms`.** A long value means a hang, not a denial.
- **The per-release detector already exists on paper:** the G1 compatibility step, which runs on every release image and asserts "≥ 3 `GET /phase` lines" (C11 brief :231, the G1 assertions). It goes red if loopback dies after a bump.
- **Candidates to measure at that bump** (none is built now, because CEF 139 enforces nothing, so a grant set today would be a guard no test can kill):
  - a gstcefsrc `CefPermissionHandler` that grants only the LNA permission, and only for `Env.app_origin`;
  - the Chrome policy `LocalNetworkAccessAllowedForUrls` set to the app origin, if CEF reads managed policies (unverified);
  - as a last resort, `disable-features=LocalNetworkAccessChecks` via `GST_CEF_CHROME_EXTRA_FLAGS`. It is global to every `cefsrc`, and Chromium deletes feature flags after launch.
- **The failure is safe on paper.** R2005 aborts a poll with an `AbortController` at `LONG_POLL_MAX_MS + 2 000` and counts it as a failure (R2005 brief, change 6, the hook, :296). So a pending, never-answered prompt still reaches fallback B, and the hook doesn't hang. The beat's `phaseChannel: web` is what shows it.
- **Element fact (C11):** at this gstcefsrc pin, `chrome-extra-flags` is deprecated, and the value is global across every `cefsrc` instance. gstcefsrc.cc:1214–1218 warns: "set GST_CEF_CHROME_EXTRA_FLAGS instead". It splits on `,`, then on `=` (gstcefsrc.cc:676–699). Under outcome (a) the adapter sets no flag. If a later CEF needs one, the warning's env var is the supported route; it is just as global.

**Not measured:**
- The 404 / edge-5xx / net-error paint question (Owed 17). Step 0 never loaded a failing page.
- Whether a permission flow would fire. None was visible (no console line, no failure), and gstcefsrc at this pin implements no `CefPermissionHandler`. CEF 139 predates Local Network Access's prompt (Chrome 142), so this run could not see it (Owed 22).
- The real overlay page, with its CSP (Owed 21, Owed 23).
- A non-loopback private address. Only `127.0.0.1` was probed, which is the only address the compositor serves.

### Teardown (C11 Step 0)

- **Machines:** `8654212a734238` (V1), `e829491b1569e8` (V2), `7849207a95d408` (V3a), `e82dd5e0c69478` (V3b), `784eee0da52678` (V4) and `819653c9755e48` (V5). Each was destroyed with `--force` right after its run.
- **Final check:** `fly machine list -a seazn-relay-spike` reads "No machines are available on this app".
- **Laptop:** the quick tunnel and the laptop's `http.server` were stopped; the tunnel URL above is dead.
- **Kept:** the app, its secrets and the image. Nothing else was created.

### F-009 appendix: raw lines

Every line below is verbatim and complete, in order, from each run's output: first the runner's own lines (`RUN`, `GST start`, `FIRST_SERVER_LINE`, `STOP`), then the probe server's stdout, then the `cefconsole` messages. Left out: the dbus and GCM lines (counted in result 8), the runner's BUILDINFO line, the probe server's stderr, and the gst pipeline lines. The server's `at=` values are epoch ms. These `reply` lines carry no `cors=` field, because the runs predate it (see "Evidence for the headers sent").

**V1**

```
RUN V1 url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html flag=none server_args=--variant none host=Linux 6.12.105-fly x86_64 start=1792133897581
GST start=1792133899090 cmd: GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html listen-for-js-signals=false max-video-framerate=30/1  ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink
FIRST_SERVER_LINE seen=1792133902951 (+3861 ms after gst start)
STOP reason=20s-after-first-server-line at=1792133923050
RUN V1 done
--- server (stdout)
request method=OPTIONS path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792133902797 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=0 status=204 write=ok at=1792133902797 body=-
request method=GET path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792133902798 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
reply method=GET path=/phase?after=0 status=200 held_ms=2000 write=ok at=1792133904799 body={"phase":"live","seq":1}
--- console (cefconsole)
0:00:03.680230790   694 0x55e1f7268be0 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:27 seazn:probe-page=https://disclosure-raymond-functioning-structured.trycloudflare.com secure=true
0:00:05.693694010   694 0x55e1f7268be0 ERROR             cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:0 Access to fetch at 'http://127.0.0.1:7419/phase?after=0' from origin 'https://disclosure-raymond-functioning-structured.trycloudflare.com' has been blocked by CORS policy: No 'Access-Control-Allow-Origin' header is present on the requested resource.
0:00:05.695571950   694 0x55e1f7268be0 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:41 seazn:probe=blocked:TypeError:Failed to fetch
0:00:05.699308540   694 0x55e1f7268be0 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:42 seazn:probe-ms=2010
```

**V2**

```
RUN V2 url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html flag=none server_args=--variant pna host=Linux 6.12.105-fly x86_64 start=1792133984708
GST start=1792133986220 cmd: GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html listen-for-js-signals=false max-video-framerate=30/1  ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink
FIRST_SERVER_LINE seen=1792133990085 (+3865 ms after gst start)
STOP reason=20s-after-first-server-line at=1792134010217
RUN V2 done
--- server (stdout)
request method=OPTIONS path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792133989968 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=0 status=204 write=ok at=1792133989969 body=-
request method=GET path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792133989970 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
reply method=GET path=/phase?after=0 status=200 held_ms=2000 write=ok at=1792133991971 body={"phase":"live","seq":1}
--- console (cefconsole)
0:00:03.714472548   693 0x55e1c608a290 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:27 seazn:probe-page=https://disclosure-raymond-functioning-structured.trycloudflare.com secure=true
0:00:05.771581215   693 0x55e1c608a290 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:45 seazn:probe=ok:{"phase":"live","seq":1}
0:00:05.771699175   693 0x55e1c608a290 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:46 seazn:probe-ms=2055
```

**V3a**

```
RUN V3a url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html flag=disable-features=LocalNetworkAccessChecks server_args=--variant pna host=Linux 6.12.105-fly x86_64 start=1792134067125
GST start=1792134068635 cmd: GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html listen-for-js-signals=false max-video-framerate=30/1 chrome-extra-flags=disable-features=LocalNetworkAccessChecks ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink
FIRST_SERVER_LINE seen=1792134072499 (+3864 ms after gst start)
STOP reason=20s-after-first-server-line at=1792134092635
RUN V3a done
--- server (stdout)
request method=OPTIONS path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792134072382 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=0 status=204 write=ok at=1792134072383 body=-
request method=GET path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792134072385 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
reply method=GET path=/phase?after=0 status=200 held_ms=2000 write=ok at=1792134074385 body={"phase":"live","seq":1}
--- console (cefconsole)
0:00:03.707751849   694 0x56434e766120 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:27 seazn:probe-page=https://disclosure-raymond-functioning-structured.trycloudflare.com secure=true
0:00:05.767429446   694 0x56434e766120 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:45 seazn:probe=ok:{"phase":"live","seq":1}
0:00:05.767548526   694 0x56434e766120 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:46 seazn:probe-ms=2058
```

**V3b**

```
RUN V3b url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html flag=disable-features=PrivateNetworkAccessRespectPreflightResults server_args=--variant pna host=Linux 6.12.105-fly x86_64 start=1792134121661
GST start=1792134123170 cmd: GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html listen-for-js-signals=false max-video-framerate=30/1 chrome-extra-flags=disable-features=PrivateNetworkAccessRespectPreflightResults ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink
FIRST_SERVER_LINE seen=1792134127028 (+3858 ms after gst start)
STOP reason=20s-after-first-server-line at=1792134147124
RUN V3b done
--- server (stdout)
request method=OPTIONS path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792134126855 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=0 status=204 write=ok at=1792134126855 body=-
request method=GET path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792134126856 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
reply method=GET path=/phase?after=0 status=200 held_ms=2000 write=ok at=1792134128857 body={"phase":"live","seq":1}
--- console (cefconsole)
0:00:03.659968320   693 0x55eb395fafe0 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:27 seazn:probe-page=https://disclosure-raymond-functioning-structured.trycloudflare.com secure=true
0:00:05.716433750   693 0x55eb395fafe0 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:45 seazn:probe=ok:{"phase":"live","seq":1}
0:00:05.716526980   693 0x55eb395fafe0 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:46 seazn:probe-ms=2055
```

**V4**

```
RUN V4 url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html flag=none server_args=--variant pna --hold-ms 9000 host=Linux 6.12.105-fly x86_64 start=1792134186449
GST start=1792134187958 cmd: GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html listen-for-js-signals=false max-video-framerate=30/1  ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink
FIRST_SERVER_LINE seen=1792134191817 (+3859 ms after gst start)
STOP reason=20s-after-first-server-line at=1792134211909
RUN V4 done
--- server (stdout)
request method=OPTIONS path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792134191639 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=0 status=204 write=ok at=1792134191640 body=-
request method=GET path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792134191641 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
reply method=GET path=/phase?after=0 status=200 held_ms=9000 write=ok at=1792134200641 body={"phase":"live","seq":1}
--- console (cefconsole)
0:00:03.657955548   694 0x5597ac7c6a80 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:27 seazn:probe-page=https://disclosure-raymond-functioning-structured.trycloudflare.com secure=true
0:00:12.711358542   694 0x5597ac7c6a80 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:45 seazn:probe=ok:{"phase":"live","seq":1}
0:00:12.711459052   694 0x5597ac7c6a80 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html:46 seazn:probe-ms=9048
```

**V5**

```
RUN V5 url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1 flag=none server_args=--variant pna --script waiting:5000,reconnecting:5000,final:5000 host=Linux 6.12.105-fly x86_64 start=1792134257700
GST start=1792134259213 cmd: GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1 listen-for-js-signals=false max-video-framerate=30/1  ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink
FIRST_SERVER_LINE seen=1792134263079 (+3866 ms after gst start)
STOP reason=20s-after-first-server-line at=1792134283208
RUN V5 done
--- server (stdout)
request method=OPTIONS path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792134262970 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=0 status=204 write=ok at=1792134262970 body=-
request method=GET path=/phase?after=0 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792134262972 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
transition waiting seq=1 at=1792134262972
reply method=GET path=/phase?after=0 status=200 held_ms=0 write=ok at=1792134262973 body={"phase":"waiting","seq":1}
request method=OPTIONS path=/phase?after=1 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792134263023 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=1 status=204 write=ok at=1792134263023 body=-
request method=GET path=/phase?after=1 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792134263024 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
transition reconnecting seq=2 at=1792134267972
reply method=GET path=/phase?after=1 status=200 held_ms=4948 write=ok at=1792134267973 body={"phase":"reconnecting","seq":2}
request method=OPTIONS path=/phase?after=2 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792134268019 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=2 status=204 write=ok at=1792134268020 body=-
request method=GET path=/phase?after=2 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792134268021 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
transition final seq=3 at=1792134272972
reply method=GET path=/phase?after=2 status=200 held_ms=4951 write=ok at=1792134272973 body={"phase":"final","seq":3}
request method=OPTIONS path=/phase?after=3 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=true acrm=GET at=1792134273019 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","Access-Control-Request-Method":"GET","Access-Control-Request-Private-Network":"true","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","Sec-Fetch-Mode":"cors","Sec-Fetch-Site":"cross-site","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd","Accept-Language":"en-US,en;q=0.9"}
reply method=OPTIONS path=/phase?after=3 status=204 write=ok at=1792134273019 body=-
request method=GET path=/phase?after=3 origin=https://disclosure-raymond-functioning-structured.trycloudflare.com acrpn=- acrm=- at=1792134273020 headers={"Host":"127.0.0.1:7419","Connection":"keep-alive","sec-ch-ua-platform":"\"Linux\"","User-Agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36","sec-ch-ua":"\"Chromium\";v=\"139\", \"Not;A=Brand\";v=\"99\"","sec-ch-ua-mobile":"?0","Accept":"*/*","Origin":"https://disclosure-raymond-functioning-structured.trycloudflare.com","Accept-Language":"en-US,en;q=0.9","Sec-Fetch-Site":"cross-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","Accept-Encoding":"gzip, deflate, br, zstd"}
transition live seq=4 at=1792134277972
reply method=GET path=/phase?after=3 status=200 held_ms=4952 write=ok at=1792134277972 body={"phase":"live","seq":4}
--- console (cefconsole)
0:00:03.722453646   694 0x56186928c010 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1:27 seazn:probe-page=https://disclosure-raymond-functioning-structured.trycloudflare.com secure=true
0:00:03.778286792   694 0x56186928c010 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1:45 seazn:probe=ok:{"phase":"waiting","seq":1}
0:00:03.778368742   694 0x56186928c010 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1:46 seazn:probe-ms=54
0:00:08.776153663   694 0x56186928c010 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1:56 seazn:probe-follow={"phase":"reconnecting","seq":2}
0:00:13.776091105   694 0x56186928c010 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1:56 seazn:probe-follow={"phase":"final","seq":3}
0:00:18.776250036   694 0x56186928c010 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1:56 seazn:probe-follow={"phase":"live","seq":4}
0:00:18.776336366   694 0x56186928c010 INFO              cefconsole gstcefsrc.cc:442:OnConsoleMessage:<cefsrc0> https://disclosure-raymond-functioning-structured.trycloudflare.com/phase-probe.html?follow=1:63 seazn:probe-follow-done=live:4
```

**The runner, verbatim** (`run-step0.sh`, uploaded into each Machine and never committed; its sha256 is under Method). It needs no secret: the page URL is its only input from outside.

```bash
#!/usr/bin/env bash
# C11 Step 0 runner (scratch; never committed). One run per Machine (container).
# usage: run-step0.sh <run-id> <page-url> <cef-flag|none> <server args...>
# Xvfb :99, the probe server (in CEF's own network namespace), then the brief's gst-launch line.
# Stop: 20 s after the server's first stdout line, or 30 s after gst start if the server never logs.
set -u
RUN=$1; URL=$2; FLAG=$3; shift 3
OUT=/tmp/c11/$RUN
mkdir -p "$OUT" /tmp/c11/home && chmod -R 777 /tmp/c11
export DISPLAY=:99 GST_PLUGIN_PATH=/opt/gstcefsrc GST_DEBUG_NO_COLOR=1
AS=(setpriv --reuid=spike --regid=spike --init-groups --)
echo "RUN $RUN url=$URL flag=$FLAG server_args=$* host=$(uname -srm) start=$(date +%s%3N)"
echo "BUILDINFO $(tr '\n' ' ' < /opt/gstcefsrc/BUILDINFO)"
Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp -ac >/dev/null 2>&1 &
XPID=$!
sleep 1
"${AS[@]}" env HOME=/tmp/c11/home python3 -u /c11/phase-probe-server.py "$@" > "$OUT/server.log" 2> "$OUT/server.err" &
SPID=$!
sleep 0.5
EXTRA=()
[ "$FLAG" != none ] && EXTRA=("chrome-extra-flags=$FLAG")
T0=$(date +%s%3N)
echo "GST start=$T0 cmd: GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=$URL listen-for-js-signals=false max-video-framerate=30/1 ${EXTRA[*]} ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink"
"${AS[@]}" env HOME=/tmp/c11/home GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url="$URL" \
  listen-for-js-signals=false max-video-framerate=30/1 "${EXTRA[@]}" \
  ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink > "$OUT/gst.log" 2>&1 &
GPID=$!
first=""
while :; do
  now=$(date +%s%3N)
  if [ -z "$first" ] && [ -s "$OUT/server.log" ]; then
    first=$now
    echo "FIRST_SERVER_LINE seen=$first (+$((first - T0)) ms after gst start)"
  fi
  if [ -n "$first" ] && [ $((now - first)) -ge 20000 ]; then echo "STOP reason=20s-after-first-server-line at=$now"; break; fi
  if [ -z "$first" ] && [ $((now - T0)) -ge 30000 ]; then echo "STOP reason=30s-and-no-server-line at=$now"; break; fi
  if ! kill -0 "$GPID" 2>/dev/null; then echo "STOP reason=gst-exited at=$now"; break; fi
  sleep 0.2
done
kill -INT "$GPID" 2>/dev/null
sleep 3
kill -KILL "$GPID" 2>/dev/null
kill "$SPID" "$XPID" 2>/dev/null
wait 2>/dev/null
echo "=== server.err"; cat "$OUT/server.err"
echo "=== server.log"; cat "$OUT/server.log"
echo "=== gst.log"; cat "$OUT/gst.log"
echo "RUN $RUN done"
```

## F-010: GStreamer element facts for the programme and the output (C09 Step 1, 2026-10-19)

**Where:** `ubuntu:24.04` (24.04.5 LTS) in Docker, with exactly the CI apt set (`.github/workflows/check.yml`, step "GStreamer"); gstreamer-rs 0.24.5 (`v1_24`), Rust 1.91.0. The C00 spike image is GStreamer 1.24.2 too (F-001).

**Versions:** GStreamer 1.24.2 (`libgstreamer1.0-0` 1.24.2-1ubuntu0.1); plugins-base 1.24.2-1ubuntu0.5; good 1.24.2-1ubuntu1.8; bad 1.24.2-1ubuntu4; ugly 1.24.1-1build1; libav 1.24.1-1build1.

**Element facts (gst-inspect-1.0, then used):**
- **rtmp2sink `stats`:** a `GstStructure` with `in-/out-chunk-size`, `in-/out-window-ack-size`, `in-/out-bytes-total` and `in-/out-bytes-acked`. Progress reads **`out-bytes-total`** (Owed 13). `out-bytes-acked` read 0 through every CF leg below, as F-005 found. `async-connect` defaults to true (connects on READY). Its RTMP `timeout` errored every dead or trickling leg about 5 s after the last write (F-011).
- **x264enc `speed-preset`:** `readable, writable`, not changeable in PLAYING (F-001 holds). `bitrate` and `vbv-buf-capacity` are changeable in PLAYING. Nothing branches on it (amendment 2026-10-18: no `FastPreset`, no deviation owed).
- **compositor:** `latency`, `min-upstream-latency`, `ignore-inactive-pads`, `start-time-selection` (`first` used: the first push's index is wherever the session's tick grid stands). `force-live` can be set only at construction. `background` defaults to `checker`; set `black`.
- **appsrc:** `is-live` defaults to false; `leaky-type` is changeable only in NULL or READY; `max-bytes` defaults to 200 000.
- **flvmux:** `streamable` defaults to false; set true.
- **Present / absent:** `pngdec` (good) and `avdec_h264` / `avdec_aac` (libav) are present, and the tests decode with them. **`textoverlay` is absent** (no pango in the CI set). The slate is textless anyway (amendment 2026-10-15).

**Colour: GStreamer's 8-bit converters truncate.**
- One RGB → I420 → RGB trip through `videoconvert` turns the house night `#150b36` ([21, 11, 54]) into [16, 7, 49].
- A logo-sized AYUV frame with no colorimetry is taken as BT.601, while 720p and 1080p default to BT.709. The slate's logo pad therefore converted on a different matrix from its backdrop.
- Left to negotiation, the compositor mixed in RGB.
- **Built:** `colorimetry=bt709` is pinned on every raw caps: the slate's backdrop AYUV, its I420 output and the logo AYUV; the mix (`AYUV`) and the encoder's input (`I420`). The base layer then never leaves YUV.
- **Measured, decoded from the programme with the exact BT.709 limited-range matrix:** the slate backdrop on air is [22, 11, 54], 1 level off the house colour (x264 codes the flat field). With the mix pin removed it is [18, 7, 51]. The tests' tolerance is 2 (`ON_AIR_TOL`); the earlier 8 could not tell the two apart, and the mix-pin mutant (G24) survived all 44 tests until it was tightened.

**Not live (deviation from Step 6's `is-live=true`).** The programme's appsrcs are not live. Every buffer is stamped by tick index, and nothing in the pipeline waits on, or times out against, the GStreamer clock.
- **Measured** by switching every appsrc to `is-live=true` (mutant L1) against the whole file: **9 of 44 tests red**.
- The live compositor times out against the clock and composites without the late push: 92 frames for 60 ticks, 61 for 60, 36 / 33 / 32 for 30 and 21 for 20. The starved-encoder speed read 0.967 against its bound.
- Output frames then stop being one per tick, which breaks the "never rebuilt, constant-rate by index" contract the domain's tick grid relies on.

**A skipped tick (index gap).** Probe, 150 ticks with every tenth skipped:
- The compositor fills each video gap by repeating its last frame: 149 frames, no PTS jump.
- The audio track keeps a hole of one period per skip: 14 jumps of 55 ms (21 + 33).
- The `DISCONT` flag `push` sets on a gap changes neither: identical with and without it (mutant G33, argued equivalent).
- The audio hole was Owed 40. **Fixed 2026-10-19 (owner: "1 ok"; PR #28 review round 1):** a tick after a gap pushes every missed period's audio first, from the ring and then the filler, up to the ring's 400 ms. The same probe now reads 0 jumps and 4 949 ms of audio against 4 966 (232 AAC frames); a 1 s gap leaves one 600 ms hole (30 periods missed, 12 filled).

## F-011: the output's progress feed, its poll, and the live legs (C09, Owed 13–16, 2026-10-19)

**The poll (Owed 15): option (a).**
- `OUTPUT_PROGRESS_POLL_MS` = 250 (`crates/adapters/src/gst_output.rs`), so the PDF's 5 s holds.
- `stall_is_detected_within_budget_of_the_last_byte` measures 5 122–5 127 ms from the last growing total to the domain's `SwapSink`, against a bound of 5 284 ms: `output_stall_after_ms` 5 000, plus the poll, plus one tick.
- C14 case 5's bound and spec §6 keep 5 s, but see "The last counted byte" below.

**Harnesses (the bytes that ran):**
- **Local legs:** the real adapter. `crates/adapters/examples/live_leg.rs` pushes slate ticks at 720p30 pace and prints every `OutputProgress` total and every event. It ran in the CI-equivalent container with `NET_ADMIN`, against mediamtx v1.9.3 (`mediamtx_v1.9.3_linux_amd64.tar.gz`, sha256 `0b885dbfa4ef9c14cd00191c57d90d804255ff50403a28b85ceee7988c535b60`) on 127.0.0.1, with `tc netem` on `lo`.
- **CF legs:** rtmp2sink alone, from a fresh `performance-2x` Machine in lhr on `seazn-relay-spike`, running the C00 image (GStreamer 1.24.2), one Machine per batch. The script was a scratch Python + gi pipeline: `videotestsrc` 1280x720@30, then x264 veryfast/zerolatency at 3 000 kbps with keyint 60, then `flvmux`, then `rtmp2sink`, plus AAC 128k pink noise. The measured quantity is rtmp2sink's own counter, the same element and version as the adapter's. `tc netem` ran on `eth0`.
- **Push target:** `SPIKE_A_RTMPS_PUSH_URL` (Input A, no Outputs) over RTMPS, read from the Machine's env. The scripts print neither the URL nor the key, and scrub both from every message.
- **Not used:** Input B, which carries the YouTube Output (viewer-visible; not mine to decide).

**Owed 13: the total grows on a healthy push and stops on a dead one. PASS.**
- **CF, healthy:** about 394 KB/s (3.15 Mbps), every 250 ms sample larger than the last, for 15 s.
- **CF, `netem loss 100%` at 15.05 s:** the total grew until 19.06 s, then froze at 7 567 188 to the end (32 s), including 5 s after netem was cleared. rtmp2sink errored "Socket I/O timed out" at 23.97 s.
- **Local, the adapter:** loss at 6 s; last growth at 10.77 s; `OutputError { gen: 1 }` at 15.63 s.

**The last counted byte is not the last delivered byte.**
- `out-bytes-total` counts what rtmp2sink hands the socket. The kernel's send buffer keeps it growing for about 4 s after the link dies: 1.43 MB on CF, at 3 Mbps.
- So the stall rule fires about 4 s + 5 s + poll after the cut, and rtmp2sink's own 5 s timeout errors at about the same moment (CF: 8.9 s after the cut).
- 5 s from the last counted byte holds. Measured from the cut, it is about 9–10 s (Owed 39).

**Owed 16 (a): command bytes. FAILS: the first nonzero total is not media.**
- **CF, 10 healthy connects:** the first nonzero total was 476 B (the `connect` command) at 22–36 ms, and 1 664 B at 69 ms on the first. Media (above 64 KiB) arrived at 132–173 ms.
- **CF, a made-up key on the same host and app:** 52 769 B at 138 ms, 262 334 B at 538 ms, then "connection closed remotely" at 522 ms.
- **mediamtx, a publish-denied path:** the adapter reported 9 growing totals, up to 1 008 779 B, for 2 s. mediamtx then logged "authentication failed" and reset the connection: `OutputError { gen: 1 }` at 2 033 ms.
- **Consequence:** a refused publish is "Connected" by C04's rule, and the back-off resets to attempt 0. It then ends by an error, 0.5 s on CF and 2 s on mediamtx, never by the stall rule. A wrong stream key therefore retries at the base back-off for as long as the session lasts.
- **A byte floor alone does not fix it:** a refusal let 262 KB–1 MB through.
- C04 reopens (Owed 38). Per the brief, C09's progress feed does not merge alone.

**Owed 16 (b): trickle at `netem rate 8kbit`. PASS: the total stops.**
- **CF:** the trickle started at 10.28 s. The total grew to 13.54 s (the socket buffer), then held at 5 424 296 until rtmp2sink's timeout error at about 18.3 s.
- **Local, the adapter:** the trickle started at 4 s. The total grew to 9.03 s, held, and the error came at 14.03 s.
- **Why it holds:** rtmp2sink writes whole RTMP messages, and at 8 kbit a 720p frame (about 12 KB) never completes inside the 5 s timeout.
- **Unmeasured:** a faster trickle, tens of kbit/s, could complete a message every few seconds and keep the total growing. C14 case 12 (`dest-trickle`) stays the witness.

**Owed 14: RTMPS connect → first bytes, n = 10 (Input A, not B, as above).**
- First nonzero total: 22–69 ms, median 31.
- First media (above 64 KiB): 132–173 ms, median 138.5.
- The 5 s `output_connect_timeout_ms` has about 30× headroom; no change proposed.

**A dead sink sends an EOS after its error (found by these legs; fixed in C09).**
- Each output appsrc whose push fails sends an EOS downstream. The watch took that EOS for a drain: `OutputDrained` 34 ms after the error, with no drain asked for, spending the once-only event, so the session's real drain later always waited out `drain_timeout_ms`.
- **Fixed** (`take_drained_at_eos`). Test: `a_failed_output_neither_reports_drained_nor_spends_the_drain`; mutants G38 and G39.

**Teardown (C09):**
- **Fly Machines created:** `874749ef304748` (c09-cf-leg; exited at boot on a bad entrypoint), `6839327a2021d8` (c09-cf-leg2) and `8744410f632118` (c09-cf-leg3). Each was destroyed. `fly machines list -a seazn-relay-spike`: "No machines are available on this app".
- **Cloudflare:** no input or Output was created or changed. Pushes went only to Input A (recording `off` since the C00 teardown; not re-read) and to one made-up key, which CF refused.
- **flyctl** used its stored login; no token went on argv.

**BLOCKED: F-004's 10 min direct-to-YouTube test of the noise filler and of a no-audio-track variant.**
- It needs an owner session: the push is a broadcast on the owner's channel (`SPIKE_YT_DIRECT_URL` is staged), and the verdict is YouTube Studio's health readout. Owed 41.
- **In CI instead:** the filler's encode, through the adapter. 137–140 AAC frames for 3 s; 46.88 frames/s; 128 kbps; zero-byte share 0.106 (fail above 0.5); level between −80 and −45 dBFS. A real tone passes at −23 dBFS; its zero share is 0.618, which is CBR fill on a pure tone, so it is reported and not asserted.

## F-012: the input pipeline's element facts, and the CF Input A leg (C10, 2026-10-19)

**Where:** F-010's container (`ubuntu:24.04` with exactly the CI apt set; gstreamer-rs 0.24.5, Rust 1.91.0). `srtsrc`/`srtsink` come from plugins-bad 1.24.2-1ubuntu4 on libsrt1.5-gnutls 1.5.3. The live leg ran the C00 image (GStreamer 1.24.2).

**`srtsrc`, the adapter's caller:**
- **`auto-reconnect` defaults to TRUE.** With it on, a peer that goes away is retried inside the element: warnings forever and never an error, a retry policy the domain cannot see. Built: `false`.
- With it off, the connect runs on the streaming thread and `set_state` returns at once. With nothing listening, an ERROR "Error on SRT socket: Connection timeout (16)" arrives after 3.0 s (3.01–3.03 s across the tests).
- **A peer that closes gives an ERROR, and then an EOS.** A graceful close (the listening `gst-launch-1.0` stopped by SIGINT) gave the ERROR about 47 ms later; a killed listener gave "Error on SRT socket: Unknown or erroneous (0)" about 5.2 s later. *Corrected by the PR #30 review (probe P2):* `basesrc` then pushes an EOS, which reaches the video appsink 1.4–7 ms after the error was posted. The C10 build first reported whichever reached it first, so a slow log line gave `Eos` (3 of 3 with a 300 ms log). Since round 1 a bus sync handler decides the loss when the error is posted, before that EOS exists: a peer close is `Error`, always (`a_peer_close_is_an_error_even_when_the_log_is_slow`).
- A passphrase under 10 characters fails inside `set_state` (READY to PAUSED), and the element posts it on the bus about 20 ms in: "Could not open resource for reading … failed to set passphrase (reason: Operation not supported: Bad parameters)". The text holds no secret.
- **A hanging resolver** (a container with `--dns 192.0.2.1`) held `set_state(PAUSED)` for 10 s inside `srtsrc`'s start, then "Failed to resolve host". So the adapter starts each pipeline on a starter thread.
- GStreamer percent-decodes the URI's query. `streamid=play%3DMADEUP%26X` reached libsrt as `play=MADEUP&X`: the listener's `caller-connecting` and `srtobject`'s debug line both showed the decoded stream id, byte for byte. A `latency=` in the URI sets the property (read back as 1 000).
- `GST_DEBUG=srtobject:5` prints `passphrase=(string)…` and `streamid=(string)…` raw in its "Opening SRT socket with parameters" line (Owed 47).
- Other defaults read: `latency` 125, `poll-timeout` 1 000, `wait-for-connection` true and `authentication` true. `passphrase` is write-only.

**`srtsink`, the tests' listener only:**
- It sends each buffer as one SRT message. At `mpegtsmux`'s default alignment, every keyframe went out in a buffer over 1 316 bytes and never arrived: the caller decoded 0 buffers in 6 s, against 135 with `alignment=7` (7 × 188 = 1 316, SRT's live payload).
- `authentication` reads as default true, but `caller-connecting` is not emitted until the property is set: 0 calls unset, 1 once set.

**The other elements:**
- `videoconvertscale` `add-borders` defaults to true, so removing that line alone changes nothing. The brief's mutant 1 also removes the fixed caps.
- `queue` `max-size-time` defaults to 1 s. `appsink` `max-buffers` defaults to 0 (unbounded) and `drop` to false, so the adapter sets both.
- `avdec_h264` `output-corrupt` defaults to true (Owed 48). *Since PR #30 round 2 it is set false, by the owner's ruling of 2026-10-19 (hide damaged frames): the slot keeps the last good picture.* `tsdemux` `latency` is 700 ms, left at its default.
- Two warnings on every connect are harmless: `srtsrc` logs "discont detected" at its first packet, and `tsdemux`'s first buffers carry no timestamp ("Not enough information to calculate proper timestamp").
- `appsink` `async=false` is needed for a camera with no audio track. Without it the audio appsink waits to preroll, the pipeline never reaches PLAYING, and nothing reaches the slot (mutant G37: the video-only test went red).
- `audioconvert` dithers its float-to-S16 step per channel (TPDF, its default), so the two copies of a mono source differed by up to 3 LSB.

**Measured in the local tests (`crates/adapters/tests/input.rs`, 25 tests):**
- The first decoded frame (`InputConnected`) came 1.08–1.09 s after `rebuild`, at 720p and at 1080p, with the listener's keyframes 1 s apart. 30 frames followed in the next second.
- `rebuild` returned in about 6–7 ms. `Drop` returned in 0.23 ms with the video decoder hung for 3 s (bound: 500 ms).
- A video decoder hung for 3 s did not stall the audio: the ring stayed full (76 800 bytes) through the hang.
- With the inbound channel full, the camera slot still took every new frame (its PTS advanced at each 200 ms read). 31 events were dropped, with one log line for the run.

**The CF Input A leg (live). PASS for connect, shape, letterbox, audio and scrubbing; and CF never reports a lost source.**
- **Harness:** one fresh performance-4x Machine in lhr (`815406b9946e68`), running the C00 image. Three files went up with `--file-local`:
  - `input_leg`: `crates/adapters/examples/input_leg.rs`, release build, stripped, sha256 `e5e174530b1b3e98f293e818fd538bce1fffe1ee405bec156a515ef75bc9f1f6`;
  - `source.py`: a scratch Python + gi pusher, sha256 `83ec0a83871b4b2d23ff9b0d9dea551d248d625515b66b7f525f100459784a0d`;
  - `run.sh`: the timeline, sha256 `bcf1be96fec248b6156c4f688c062678d5a1d65aae2d5b84e8379292fc226d11`.
- **The source:** `videotestsrc pattern=white` at 30 fps, then x264 veryfast/zerolatency at 3 000 kbps with keyframes 60 frames apart, plus a 440 Hz AAC tone. It pushed over RTMPS to `SPIKE_A_RTMPS_PUSH_URL` (Input A, which has no Outputs).
- **The reader:** the real adapter, reading `SPIKE_A_SRT_PLAY_URL`. Neither URL was printed; the leg printed only their lengths (stream id 36 characters, passphrase 65). It stood in for the domain's two timers (a 5 s stall and a 5 s connect timeout), with no back-off.
- **Timeline:** the source ran at 1280×720 from 0 to 30 s, was off from 30 to 50 s, and ran portrait (720×1280) from 50 to 95 s. The leg ran 105 s.
- **What happened:**
  - Gen 1 was built at 0.06 s, before CF was live. Nothing came; the connect timeout fired at 5.0 s.
  - Gen 2: `InputConnected` 2 915 ms after its build (t = 7.92 s).
  - The source stopped at 30 s. **No event at all: no error and no EOS.** The frames stopped, and the stand-in stall rule rebuilt at 36.0 s.
  - Gens 3–5, while there was no source: each was silent (no error, no frame) until its connect timeout.
  - Gen 6 was built at 51.0 s, 1 s after the portrait source started: `InputConnected` 2 908 ms after its build.
  - The source stopped at 95 s: again no event; the stall rebuild came at 100.9 s.
- **Over the run:**
  - 7 generations, 2 connects, **0 losses**, 1 989 frames.
  - 192 reads of the camera slot, all I420 1280×720, PAR 1/1, BT.709. The 91 landscape reads had luma (edge, centre) = (235, 235). The 101 portrait reads had (16, 235): pillarboxed.
  - The audio read back at 440 Hz, the source's tone.
  - 0 log lines held any form of either credential (raw, percent-encoded in either case, or the URI). 0 samples were dropped at the gate, and 0 frame events.
- **Verdict:**
  - On CF, the input is lost only by the domain's stall rule and connect timeout; the adapter never reports `InputLost` for a source that stops. This matches F-002's silent socket.
  - `LossReason::Eos` never occurred on CF. Locally it could, through the race above; since round 1 it needs a bare EOS with no error before it, and nothing measured produces one (Owed 46).
  - Connects took 2.9 s from the build, inside F-002's 2.22–4.19 s.

**Teardown (C10):**
- **Fly Machines:** 1 created (`815406b9946e68`) and destroyed with `--force`. `fly machine list -a seazn-relay-spike` then read "No machines are available on this app"; `--json` gave a total of 0.
- **Cloudflare:** no input or Output was created or changed. Pushes went only to Input A; its recording setting was not re-read.
- **flyctl** used its stored login; no token went on argv.

## F-013: CEF element facts for the overlay renderer (C11, 2026-10-19)

**Where:** the C11 test image, which is F-001's spike image (`registry.fly.io/seazn-relay-spike`, image id `15890e25c22a`: CEF 139.0.28, gstcefsrc `204b01b`, GStreamer 1.24.2) plus the build tools and Rust 1.91.0. It ran in a colima VM on the dev host, under `Xvfb :99`, as uid 10001 with `HOME=/home/spike`. No Fly Machine was used. Every token below is made up.

**The spare renderer:**
- Without a flag, Chromium keeps a spare renderer beside the active one, and the next browser takes it. Across builds the renderer client ids went 5, 6, 7, 8, with the spare always the highest. So a renderer that appears after build N belongs to build N+1, not to N.
- With `disable-features=SpareRendererForSitePerProcess`, each browser has exactly one renderer, with a new client id per build. Readiness came 540–555 ms after the build either way.
- Memory for one healthy page: the RSS sum of CEF's children was 436 MB with the flag and 505–626 MB without it. PSS was about 173 MB. The RSS sum counts the zygote's shared pages once per process, so the cap reads PSS (`cef_rss_cap_mb`).
- `chrome-extra-flags` is global to the process: CEF reads it once, at its first start, from the first element.
  - It replaces `GST_CEF_CHROME_EXTRA_FLAGS`, so the adapter carries the variable's value over after its own flag.
  - It splits on `,`, then on `=`, so only one `disable-features=` value survives. A later flag that needs its own `disable-features` (an LNA grant, Owed 22) must be merged into the same value.
  - cefsrc logs a WARNING, "…property is deprecated and is global…", on every set.

**Processes dying:**
- **A renderer killed** (SIGKILL): cefsrc logs `Render subprocess terminated, reloading URL!` 12–35 ms later, from its `OnRenderProcessTerminated`, with no bus error. It then reloads the page under a new pid with the same client id, and the reloaded page logs `seazn:ready` again (the crash test sees it).
- **The spare renderer killed:** no warning, and it is not relaunched.
- **The GPU process killed:** recovered silently; 96 frames in the next 4 s. That holds only while its zygote lives, and only up to a limit (the PR #32 confirm pass, below): with that zygote dead, the next GPU-process death aborts the whole process; with it alive, the 6th ~~death in about 10 s~~ GPU-process death of the process's life did: a count, not a rate (amended 2026-10-20, the final C11 review's G2: kills 20 s apart, the 6th aborted too).
- **The zygote killed:** every later renderer launch fails. The warning storms at about 60 a second, for every browser after it, with 0 frames. CEF is dead for the rest of the process (Owed 56).
- **Which zygote** (the PR #32 fix loop's probe, 2026-10-19; corrects the line above, which killed both). CEF runs two zygotes, children of the compositor process.
  - The first parents the GPU process. Killed alone, the page kept painting and the next build signalled ready, which this file first recorded as "CEF recovered". **It did not** (the PR #32 confirm pass's N1, below): the next GPU-process death aborts the process.
  - **The first's death orphans its GPU process** (the final C11 review's FS2, FS3 and G4, 2026-10-20; C11b's image test, 3 runs): the GPU process is reparented to pid 1 and leaves the compositor's tree, so the PSS cap, the OOM grading and the GPU-death count no longer see it, and it lives on. Its death then aborts the process even with graphics stopped (FS3, 2 of 2), while a retry build after the stop reaches ready (FS2). So staying on air without the overlay leaves the abort armed (G3); C11b restarts in place instead, within a poll and a tick of the verdict.
  - The second parents the renderers and a utility process. Killed alone, the current renderer is orphaned: it leaves the compositor's process tree (so the guardrails, the claims and the reaper lose it) and keeps painting and beating, and the next build gets `CefCrashed` at once, with 0 samples.
  - The review's P7: with the zygotes dead, the current build's renderer killed gave no `CefCrashed` within 3 s (2 of 2 runs), and CEF's browser side burned about 1.0–1.3 cores in the compositor process while a build existed.
  - So the adapter's verdict (`health().unrecoverable`, Owed 56) is **any** zygote gone, from the child poll (since the confirm pass; the fix loop decided on the renderers' zygote, or every zygote, only); a zygote missing from one poll's list counts only once its process is gone (or a zombie). Since 2026-10-23 each ready lists the zygotes too, so the watch has them before anyone hears of the ready (F-022).
- **Chromium's abort** (the PR #32 confirm pass, 2026-10-19; reviewer's probes CF, CG, CH, then the implementer's PK and GR, each case in a process of its own). The process logs `FATAL:content/browser/gpu/gpu_data_manager_impl_private.cc:415] GPU process isn't usable. Goodbye.` and dies of SIGTRAP (wait status 133: signal 5, core). In production that process is the compositor: the programme goes off air with it, and the exit reads as a crash to Fly (`max_retries` 2) and to R2003b.
  - The GPU zygote killed, then the GPU process: the abort, 3 of 3 (CG; pinned by `a_gpu_process_death_after_its_zygotes_aborts_the_whole_process`). The control, the GPU process killed with its zygote alive: relaunched under the same zygote, the page kept beating and gen 2 reached ready (CH).
  - Both zygotes killed during CEF's start, before a poll saw a renderer: the abort (CF).
  - **Every CEF child SIGKILLed at once** (PK; C14 case 8's `pkill -KILL -f cef`): the abort, 3 of 3, before the next child poll (no verdict was ever reported). An in-place restart cannot come first.
  - **The GPU process killed again and again, its zygote alive** (GR: a kill, then 2 s for the relaunch): relaunched 5 times, with 21–39 samples in each next 2 s; the 6th kill, about 10 s in, was the abort, 2 of 2. ~~Whether Chromium's count decays with time was not measured (Owed 61).~~ **It does not, over 100 s** (the final C11 review's FD, 2026-10-20): with kills 20 s apart (599–601 samples written in each 20 s) the 6th, 100 s in, aborted too. A count over the process's life. From Chromium's source, by the reviewer's memory and unverified: one crash is forgiven per whole hour between crashes, and `--disable-gpu-process-crash-limit` exists (Owed 70). C11b counts the deaths (`health().gpu_process_deaths`) and restarts in place at 3 (the PR #34 review's I1: 3 leaves the 4th and 5th deaths survivable, for an unread one and one during the exit).
  - **The abort writes a core** (the final C11 review's G1, 2026-10-20): wait status 133 is SIGTRAP with core, and in the rig (`ulimit -c` unlimited, `core_pattern` `core`) a 93–117 MB core of the whole process landed in the cwd, with the page token in it 46 and 61 times. C12 sets RLIMIT_CORE to 0 (Owed 68). G1's grading puts the GPU process (800) ahead of the zygotes (500) in the OOM order, so under memory pressure its deaths come before a zygote's.

**Teardown:**
- `set_state(NULL)` returned in 4–17 ms, and in 7 ms for a page hung in a JS busy loop with js-signals off (the adapter's mode).
- In js-signal mode, with a page that never signals:
  - `set_state(PLAYING)` never returns, and `NULL` blocks behind it.
  - SIGKILL to the renderers does not unblock it, and the reload respawns a renderer with the same client id.
  - So the build's thread stays stuck: the reaper kills the renderers it can see, and the thread leaks (Owed 55).
- During a JS busy loop the appsink receives nothing. The probe read 30 samples a second before the hang, 0 through its 1.4 s, and 30 a second after the revive.

**Debug categories and console lines:**
- `cefsrc` and `cefconsole` are registered in the element's `class_init`, which runs at the first element, not when the plugin loads. A threshold set by name before that adds a pattern that outranks `GST_DEBUG`'s. The adapter's first cut did this and lowered an operator's `cefsrc:7` to WARNING; the token test's child, under `GST_DEBUG=cefsrc:7`, printed 0 cefsrc LOG lines. The adapter now references the class first, then raises a threshold only when it is below the floor (196 paint lines in the same child afterwards).
- A console line arrives as `<source>:<line> <message>`. For an inline script, the source is the page URL, token included.
- `console.info` arrives at INFO and `console.warn` at WARNING.

**Where the page token goes** (URL `…&st=<token>`):
- **stderr:** never, at CEF's log severities default, info, debug and verbose, with GStreamer's handler at its defaults.
- **GStreamer's default handler:** with `GST_DEBUG` ≥ 4, or `cefconsole:4`, it prints every console line, URL and token included. `GST_DEBUG=5` also prints `gst_parse`'s lines, which carry a URL put in a launch string. So the adapter replaces the handler and sets the URL as a property.
- **`$HOME/.config/cef_user_data/chrome_debug.log`:** at CEF's default severity, every console message is written with its source URL: 4 token lines in one short run. Chromium echoes them there because cefsrc's `OnConsoleMessage` returns false.
  - `log-severity=disable` wrote 0 lines, and the signals still arrived.
  - `warning` wrote 0 token lines for info-level console lines.
  - `GST_CEF_LOG_SEVERITY` in the environment overrides the property: `=info` writes them again (Owed 52).
- **The CEF children's argv:** never carries the URL (8 argvs checked).
- **CEF's profile, on disk** (the PR #32 review's I1, probe P1; the fix loop's probes). Over http (the real page's scheme) and past History's 10 s commit, the token was in `.config/cef_user_data/Default/History`, `…/Default/Top Sites` and `.cache/cef_user_data/Default/Cache/Cache_Data/<hash>_0`, in clear. Incognito does not stop it, and `file://` pages write no History, which is why the token test missed it.
  - cefsrc's `cef-cache-location` (or `GST_CEF_CACHE_LOCATION`) sets `CefSettings.cache_path`, and the whole profile moves there: History, Top Sites, the HTTP cache and `chrome_debug.log`. HOME then holds 4 files (GStreamer's registry and `.pki/nssdb`), none with the token. `user-data-dir=` in `chrome-extra-flags` and `XDG_CONFIG_HOME`/`XDG_CACHE_HOME` moved it too.
  - Like `chrome-extra-flags`, it is global, read once at CEF's start from the first element, and logs cefsrc's deprecation WARNING on every set. So a profile per build is impossible: it is one per process.
  - A path CEF cannot create falls back to HOME (no in-memory mode). A `/dev/shm` path worked, about 4 MB for one page.
  - The adapter's profile is `<temp_dir>/seazn-cef-<pid>-<nanos>`, mode 0700; the last `CefGraphics` dropped removes it, and the next start removes any whose process is gone, and any carrying its own pid (an earlier image's: an exec, or a restarted container that gave the compositor the same pid; the confirm pass's N2, where an `exec` as pid 1758 kept `seazn-cef-1758-1`).
  - It fails closed (the confirm pass's N4): if the directory cannot be made, no build is made, so CEF never starts on HOME; the build fails as `CefCrashed` (the programme runs on without the overlay), and the domain's next restart tries again. Measured with a TMPDIR at mode 0500: gens 1 and 2 crashed with no CEF child started and no token under HOME; at 0700, gen 3 swept a dead process's directory and reached ready in the new one. The refusal and the sweep's line go through the adapter's log sink.
  - Removed under a live CEF (a test process, where adapters come and go): History stays open on the unlinked file, so a later test's History never reaches a path, and the disk cache writes its index again later (`Cache_Data/index-dir/the-real-index`: hashes, no URL). In production the drop is the process's exit. C12 puts `TMPDIR` on a tmpfs (Owed 58).

**The environment:**
- CEF writes its profile under `$HOME/.config/cef_user_data`, so the CEF user's HOME must exist and be writable (Owed 54). The probes ran uid 10001 with `HOME=/home/spike`. Since the fix loop the adapter moves the profile to its own directory (above); HOME still takes GStreamer's registry and NSS's database.
- **A long TMPDIR kills CEF at its start** (the confirm pass, 2026-10-19). A test child with a TMPDIR of 85 characters died of SIGTRAP (wait status 133) about 1 s after its first build, before ready, with no FATAL line; the same child at 59 characters ran. Chromium makes Unix sockets under TMPDIR (`.org.chromium.Chromium.*`), and a socket path holds at most 108 bytes, which is the likely cause (not isolated). C12's TMPDIR must stay short (Owed 58).
- **One CEF per profile.** A second process on the same HOME failed its first start: "state change failed and some element failed to post a proper error message with the reason for the failure" (`gst_base_src_start`), alongside CEF's own warning "Please customize CefSettings.root_cache_path … process singleton". So the token test's child gets a HOME of its own.

**Measured in `crates/adapters/tests/graphics.rs` (19 tests, 3 runs in a row):**
- Ready came 552–718 ms after `restart`, and the beats 2.000 s apart from ready.
- `stop()` returned in 0.15–0.33 ms and `Drop` in 0.18–0.44 ms, with the page hung (bound 500 ms).
- The reaper killed the js-signal renderer 3.006–3.039 s after `restart(2)`.
- The hidden page:
  - 180 programme frames before the retry, 45 of them after the hide, with 0 blue;
  - 45 frames after the retry's ready, 40 of them blue (one run).
- A reported crash (the cap lowered to 1 MB): 0 writes after the report, and 44–94 samples dropped over 1.5 s.
- Six CEF children, one of them a renderer, ran 44 threads, all at nice 10 with `oom_score_adj` 1000 after two polls.
- **The PR #32 fix loop's file** (23 tests: 23 of 23 in 3 runs in a row, the file alone, VM load average 0.9–1.6, about 114 s a run; before its last two witnesses, the 22-test file was also green in 3 runs; the ranges below cover all 6):
  - `oom_score_adj` graded after two polls, with the kernel's `oom_score` in the same order: zygotes 500 (score 1006), the GPU and utility processes 800 (1204–1210), the renderer 1000 (1344); the compositor 0.
  - The reaper killed the js-signal renderer 3.005–3.023 s after `restart(2)`; the stuck teardown was counted leaked 6.007–6.021 s after it (bound 3 + 3 s), once.
  - `Drop` returned in 0.37–0.65 ms with the page hung.
  - The write race, exact (a held write): hide then stop left a frame in the slot in every run; stop then hide left none, with 10 samples dropped after both.
  - The disk token: History and the HTTP cache held it, both in the private profile; 0 files under the child's HOME or TMPDIR held it after the drop (one cache index file was written again, no URL).
  - The hidden page: 180 programme frames before the retry, 45 after the hide, 0 blue; 45 after the retry's ready, 45 blue.
  - Two builds' renderers side by side (gen 1's teardown held): 190–193 MB PSS in all, 151–153 MB counted for gen 2, 39–40 MB for gen 1's renderer; a cap halfway between crashed nothing, and a cap under gen 2's share crashed gen 2.
  - The zygotes one at a time, each in a fresh process: the GPU zygote killed, gen 2 ready and no verdict (that verdict was wrong: the confirm pass's N1); the renderers' zygote killed with the GPU zygote alive, `unrecoverable: Some(1)` and gen 2 crashed at once.
- **The PR #32 confirm pass's file** (25 tests: 25 of 25 in 3 runs in a row, the file alone, on the final tree; VM load average 0.8–1.8, host 3.0–11.4, 120–138 s a run):
  - The abort child died with wait status 133 (SIGTRAP, core) every run, after the verdict (`Some(1)`) was read with the GPU zygote dead and before its GPU process was killed.
  - The profile child: gens 1 and 2 refused with no CEF child started; gen 3 ready in a new directory under the TMPDIR; 5 refusal and sweep lines, every one through the sink.
  - The reaper killed the renderer 3.007–3.018 s after `restart(2)` and the leak was counted 6.005–6.017 s after it; `Drop` returned in 0.50–0.83 ms; the hidden page: 180 frames before the retry, 45 after the hide, 0 blue, and 45 after the retry's ready, 44–45 blue.
  - **A false zygote verdict, once:** in 1 of 16 runs of the parent test process, where no zygote dies, the reaper test read `unrecoverable: Some(2)`. The likely mechanism, not caught by a probe (0 in 1763 samples, 2 ms apart, over 12 builds at low load): a zygote's fresh fork reads `--type=zygote` until it is renamed, and its later exit read as a dead zygote. The verdict now counts only zygotes that are children of the process (the real ones, measured), and the reaper test asserts no verdict.
  - **The host's load reds this file.** At host load 60–215 (other sessions' builds; the VM's own load stayed under 5), the hang's 300 ms quiet window, the hidden page's drain and decoded frames, and the token test's `GST_DEBUG` child went red. An interleaved A/B of the pre-confirm head (23 tests) and this one failed the same tests in 2 of 3 runs each, and both passed once the host calmed. These tests' budgets are the domain's thresholds, so a host under about load 25 is the condition for a run to mean anything.
- **C11b's file** (`tests/graphics.rs`, 26 tests: 26 of 26 in 3 runs in a row, the file alone, on the final tree; VM load average 0.4–1.0):
  - **GPU-process deaths counted from real kills** (`gpu_process_deaths_are_counted_from_real_kills`, in a child process): the GPU process killed `cef_gpu_deaths_to_restart` times (4 in the 7 file runs before the PR #34 review's I1, 3 in the one after it), each relaunch (a new pid, under the same zygote) killed next once a poll had listed it; `health().gpu_process_deaths` read 1, 2, 3 (, 4) in turn, each once, with no zygote verdict and one adapter WARN line per death. The process lived (under the abort, the 6th).
  - **G4 in the GPU-zygote case:** after the zygote's kill and the verdict, its GPU process was alive and out of `cef_children()`, and `gpu_process_deaths` stayed 0 (an orphan that lives is no death), one orphan in each of 3 runs; a mutant that counted it went red there.
- **Straight alpha costs one pass over the frame.** In place: about 4 ms optimised and 25 ms in a debug build. A two-buffer loop took 200–400 ms in a debug build, too slow for the tests' 30 fps page: frames were written about 3 a second, and the stop test missed its first write.

## F-014: CEF 139 paints an error page opaque (C11, Owed 17, 2026-10-19)

**Where:** F-013's image. One `cefsrc` per leg (console mode, `max-video-framerate=30/1`, BGRA 1920×1080, appsink), 5 s each. Each URL carried a made-up `st=` token.

| Leg | URL | Server's answer | Frames in 5 s | BGRA at the 4 corners and the centre | `seazn:` lines |
|---|---|---|---|---|---|
| 404 | `http://127.0.0.1:7419/nope?host=compositor&st=…` (`phase-probe-server.py --variant pna`) | `GET` → 404, no body | 4 | (255, 255, 255, 255) at all five | none |
| 502 | `http://127.0.0.1:7419/overlay?host=compositor&st=…` (`--variant pna --status 502`) | `GET` → 502, no body | 4 | (255, 255, 255, 255) at all five | none |
| No such host | `http://c11-nowhere.invalid/overlay?host=compositor&st=…` | — | 3 | (255, 255, 255, 255) at all five | none |

- 0 of the run's 24 stderr lines carried the token.
- `--status N` is new in the probe server for this measurement: every request, any method and any path, gets N with no body.

**Verdict:** an error page is opaque white over the whole frame. If one reached the slot, it would cover the programme in white. It never logs `seazn:ready`, so only the ready gate keeps it off air. `frames_before_ready_never_reach_the_slot` is the gate's witness, and mutant 6 reds it.

**Known residual: a ready page that navigates** (the PR #32 review's m2, probe P5; recorded in the fix loop, 2026-10-19). The gate trusts a build's ready for the build's whole life. A page that logged `seazn:ready` and then set `location.href = "http://127.0.0.1:9/nope"` 1.5 s later put Chromium's error page into the slot through the open gate: (255, 255, 255, 255) at the four corners and the centre, with no ready and no beat after it.
- **Bound:** the domain's hang hide, `cef_hang_after_ms` (10 s) after the last beat. Until then, a white 1920×1080 frame covers the programme.
- **No producer known.** R2005's page has no self-navigation (no `location.*` or `router.*` under `components/overlay`). Next's own hard-reload paths are unverified.
- **Caught otherwise:** a 404, a 502 or a DNS failure on the FIRST load never signals ready, so the gate stays shut and the load timeout restarts the page; cefsrc's reload after a renderer crash is shut out by the crashed set (`crash_of_the_renderer_reports_cefcrashed`).
- **A fix, if a producer appears:** close the gate on any main-frame load start. cefsrc surfaces none today (no signal, no log line), so it would need a cefsrc change.

## F-015: `cefsrc` scene compatibility (G1), 2026-10-19: REFUSED, no fixture in progress on stg

- **The pre-conditions now hold on web.** R2005 (`seazn.club` #982) merged at 2026-10-19 09:10Z, and R2007 PR A (#974) on 2026-10-18. `stg.yml`'s run at 15:35Z (`6a7d360`, success) carries both. Its next run, at 16:49Z (`92201cc`), failed.
- **The brief's page:** a public stg fixture, in progress at run time, whose org has the overlay entitlement. `GET https://stg.seazn.club/api/v1/public/discovery?status=live` returned 0 items at about 17:20Z.
- **Not started:** starting one on the stg pad needs a staff login, which this session does not hold.
- **The brief:** "if it isn't in progress, refuse the run rather than skip the check". Neither leg ran: no template leg and no scene leg. Owed 21 stays open (updated there).

## F-016: CEF takes over SIGINT and SIGTERM in the compositor's process (C12, 2026-10-20)

**Where:** image.yml on PR #36 (GitHub's ubuntu-24.04 runner, Docker), the relay image in live mode
(`scripts/image_probe.py`): a fake control plane on the runner, C11's harness page as the overlay, and
input and target on local ports nothing listens on. Each run waited for `cef ready gen=1`.

| Run (image, entrypoint) | Signal and path | Exit status | Time to exit | The compositor's `exit code=` line | Final beat |
|---|---|---|---|---|---|
| `7da159f`, run 39403770532 (`exec` compositor) | `docker stop`: SIGTERM, tini → compositor | 0 | 0.1 s | absent; the log ends at `cef ready gen=1` | none (1 POST, periodic) |
| `c080c3f`, run 39407377806 (forwarding entrypoint) | `docker exec … kill -TERM <compositor>`, around tini | 0 | 0.2 s | absent | none (1 POST, periodic) |
| `c080c3f` | `docker kill -s INT`: tini → entrypoint → SIGUSR1 | 0 | 10.3 s | present | `signalled`, an earlier POST carrying `final` |
| `c080c3f` | `docker stop`: tini → entrypoint → SIGUSR2 | 0 | at once | present | `signalled`, no POST carrying `final` |

- The compositor's `/proc/<pid>/status` once CEF was up: `SigCgt 0000000100014a03` (SIGHUP, SIGINT,
  SIGUSR1, SIGUSR2, SIGTERM, SIGCHLD and signal 33 caught), `SigIgn 0000000000001000` (SIGPIPE).
- In the binary run alone with no CEF (`tests/fake_run.rs`, check.yml), SIGTERM and SIGINT reach the
  domain: the drain in 12 ms and the hold in 10.04 s, each with its final beat.

**Verdict:** once CEF starts in the process, a SIGINT or SIGTERM delivered to the compositor ends it
with status 0 at once, before the domain hears of it: no Final scene, no drain, no final beat, no exit
line. The caught HUP/INT/TERM set matches Chromium's shutdown handlers; the handler's owner was not
traced further. So the image's entrypoint does not `exec` the compositor: it runs it as a child and
forwards SIGINT as SIGUSR1 and SIGTERM as SIGUSR2, which the compositor reads as the same stops
(`wiring::STOP_SIGNALS`), and tini (no `-g`) signals the entrypoint alone. Since the PR #36 review
(I2, m1) that forwarding lives in `docker/supervise.sh`, sourced first, which also holds a stop that
arrives before the compositor's handlers exist.

**On a Fly Machine (MEASURED 2026-10-20, Machine `48e4533c564ed8`, image at `50c6643`, destroyed;
the coordinator's Owed 73 probe):** PID 1 is `/fly/init`, which signals tini's pid only, in 12 of 12
stops. Each stop drained and posted its final `signalled` beat (12 of 12); SIGINT held the Final
scene for 10.018–10.032 s; SIGTERM reached the exit line in 7–22 ms. No stop had this finding's shape.
**`fly machine stop` with no flag sends the Machine's `stop_config` signal (SIGTERM), not SIGINT** as
flyctl's help says; `--signal SIGINT` sends SIGINT. Web names SIGINT itself, so its stop is unaffected;
a person stopping a Machine by hand gets the drain, not the Final hold.

## F-017: the process aborted AFTER a clean exit: libc's exit handlers raced CEF's threads (C12, 2026-10-20)

**Where:** the relay image in live mode, CEF up, after `app::run::run` returned and the compositor
logged `exit code=0` (or `exit code=75`'s path, unmeasured).

| Where | Exits | Aborted after the exit line | glibc's text |
|---|---|---|---|
| image.yml, runs at `7da159f`–`50c6643` (the PR #36 review) | 6 live exits | 1 (run 39410132377 at `ff0fdf4`, a `docker stop`): container status 134, bash `Aborted (core dumped)` | `double free or corruption (!prev)` |
| A Fly Machine at `50c6643` (the coordinator's Owed 73 probe) | 12 stops | 6: SIGTERM 5 of 8, SIGINT 1 of 4; exit 134 | `corrupted size vs. prev_size in fastbins`, `corrupted double-linked list`, `double free or corruption (!prev)` |
| The same Machine | 2 deadline exits | 1, after its `deadline` beat: Fly's `on-failure` restarted the ended session's Machine in place (`requested_stop: false, restarting: true`), which booted past its deadline | `corrupted size vs. prev_size in fastbins` |

- Every abort came after the drain, the final beat and the `exit code=0` line. A requested stop was
  never restarted, even at 134; web's reader would still see 134.
- **Cause (by mechanism, not traced to a frame):** `main` ended with `std::process::exit`, libc's
  `exit(3)`, which runs every `atexit` handler and C++ static destructor (libcef's, GLib's) while the
  teardowns that the adapters' drops started (each returns at once and joins nothing, by design:
  C08 and C11 amendments of 2026-10-19) still run on CEF's and GStreamer's threads.
- **Fix:** `main` ends through `hard_exit::now`, `_exit(2)` after flushing stderr: the kernel ends every
  thread and runs no handler. The final beat is already sent (`run` returns after
  `flush_final_beats`). `crates/hard-exit` is the workspace's one sanctioned `unsafe` (no safe binding
  exists); its test shows `now` runs no `atexit` handler where `std::process::exit` does.
- **Proof owed to CI:** image.yml's probe repeats 32 live exits with CEF up (24 stops, 4 deadlines,
  4 dead zygotes, at 720p and 1080p) and requires every status exact with no abort text. The
  coordinator re-runs the Fly probe after the fix.

## F-018: RLIMIT_CORE 0 stops a core only for a FILE `core_pattern` (C12, 2026-10-20)

- **The CI runner (GitHub ubuntu-24.04):** its `core_pattern` is a pipe to a crash helper. The kernel
  ignores RLIMIT_CORE for a pipe (`fs/coredump.c` sets the limit to infinity, except the value 1, at
  which it refuses the core: "RLIMIT_CORE is set to 1, aborting core"). F-017's abort there printed
  `Aborted (core dumped)` from a compositor whose `/proc/<pid>/limits` read 0 0: a core holding the
  job token, the page token and the stream key reached the helper (the PR #36 review's I1).
- **A Fly Machine (MEASURED 2026-10-20, Machine `48e4533c564ed8`):** `core_pattern` is `core`, a file
  in the cwd (`core_uses_pid 0`, `fs.suid_dumpable 0`, kernel 6.12.113-fly). The limit of 0 stopped
  every core there: 6 compositor aborts printed `Aborted` with no `(core dumped)`; a control in the
  same Machine dumped a 557 KB `core` with an unlimited limit and nothing with 0. Fly's own default is
  soft 0, hard unlimited, on init, tini, bash and Xvfb. The rootfs is reset at every start, an
  `on-failure` restart included, so no core or profile file outlives a boot there. (Corrected
  2026-10-24, the PR #54 review's m1: of that sentence only the starts after a requested stop were
  measured, 10 of them, a file left in `/tmp` gone each time; the `on-failure` restart is cited from
  Fly's docs, "Restarting wipes the ephemeral file system", with `persist_rootfs` defaulting to
  `never`, which web's Machine create does not set. F-025.)
- **So, as built:** the compositor is non-dumpable (`PR_SET_DUMPABLE 0`: no core of it under any
  pattern), and its RLIMIT_CORE is 1 byte, soft and hard, which CEF's children inherit (the dumpable
  flag resets at `execve`): refused for a pipe, and below the ELF minimum for a file. The entrypoint
  sets 1:1 on itself and tini. The probe aborts a CEF child (the kernel's refusal line, on the runner's
  pipe) and the compositor (134, no `(core dumped)`). ~~Limit 1 under Fly's file pattern is the
  coordinator's post-fix probe to confirm (Owed 73).~~ **MEASURED 2026-10-23** (the coordinator's Owed 76
  probe at `341ae7a`, Machine `8ed9e19b166048`, kernel 6.12.114-fly, destroyed): the limit reads 1 1 on
  tini, both bashes, Xvfb, the compositor and all 6 CEF children; the compositor's `/proc/<pid>/status`
  is root's; no core after a forced compositor abort or a forced CEF-child abort (none in the cwd,
  `/tmp`, `/` or `/dev/shm`); a dumpable control at limit 1 left no file, and at unlimited a 442 368-byte
  `core`.


## Platform notes (Fly-specific; nothing inside the image depends on them)

- **CPU:** performance-4x reports AMD EPYC with 4 vCPU.
- **Remote builds:** builds ran on Fly's remote builder (`fly deploy --remote-only`), ≈5 min for this image.
- **Secrets:** app secrets reach Machines created with `fly machine run` as well as `fly deploy` Machines. The `fly ssh console` environment contains them, so never dump the environment there.
- **`fly ssh sftp put` refuses to overwrite** an existing file. Upload under a new name, then `cp`.
- **`pkill -f <pattern>` self-match:** run inside `fly ssh console -C "bash -c '…'"`, the pattern matches the ssh-launched shell's own command line and kills the session. Keep killed names out of the command line.
- **Restart:** the main Machine's `uptime -s` is 08:52:20Z, after its creation, so it restarted once during setup. The cause was not investigated.
- **Cold start:** 25.07 s on a fresh host (14.38 s of it is the image pull), 7.48 s on a stopped-then-started Machine (F-001).

## Cloudflare state left behind (all scratch)

- **Input A recording:** while diagnosing F-002, Input A's recording was switched to `automatic` twice, and reverted to `off` both times; it is `off` now.
- **Recorded videos:** this left 6 videos on Input A: five `ERR_LIVE_VIDEO_TRANSITION_FAILED` from the audio-only feed (09:10–09:22Z), and one `ready` recording of 1513.66 s (from 09:23:21Z, the control feed and then the fixed feed). They auto-delete after 30 days (`deleteRecordingAfterDays: 30`); the owner may delete them sooner.
- **Kept:** the app, the staged secrets, both inputs and the YouTube Output.

## Teardown record

2026-10-15 ≈10:19Z.

- `fly scale count 0 -a seazn-relay-spike --yes` destroyed `48edd39ead9098` (group app).
- `fly machine destroy --force` removed the source Machine `800eeda6e99948`. The cold-start Machine `80e9d00a1267e8` was destroyed at 09:06Z.
- `fly machine list -a seazn-relay-spike`: "No machines are available on this app"; `--json`: total 0, running 0.
- Kept:
  - the app;
  - secrets `SPIKE_A_RTMPS_PUSH_URL`, `SPIKE_A_SRT_PLAY_URL`, `SPIKE_A_SRT_PUSH_URL`, `SPIKE_B_RTMPS_URL`;
  - Input A (disconnected 10:19:48Z, recording off);
  - Input B (disconnected, recording off, 1 Output: YouTube).
- The YouTube Output was **not** removed (dispatch ruling overrides brief Step 10).

## Teardown record (owner session, partial teardown per coordinator)

2026-10-15 ≈11:11Z.

- Owner-session Machines: `28655e54c4e358` was destroyed at ≈11:04Z, because `fly machine restart` does not pick up a re-staged secret. Its replacement `8ed9e13b302d98` was destroyed at ≈11:11Z.
- `fly machine list -a seazn-relay-spike`: "No machines are available on this app".
- **Kept, for the C/D rerun:**
  - the app;
  - all staged secrets (including `SPIKE_FIX_SRT_PLAY_URL`);
  - CF Inputs A and B;
  - B's YouTube Output (`8448ee…`).
- Nothing touched the fixture's own session, nor its YouTube target.

## Teardown record (owner rerun and redo)

2026-10-15 ≈13:57Z.

- Machines `8747092f6575d8` (rerun 1) and `7811141db04698` (redo) were destroyed.
- `fly machine list -a seazn-relay-spike`: "No machines are available on this app".
- Kept: the app, all staged secrets, CF Inputs A and B, and B's YouTube Output (`8448ee…`).
- The fixture sessions and their YouTube targets were not touched by this agent. The owner disabled and enabled outputs in CF.

## Teardown record (A/B probe)

2026-10-15 ≈15:14Z.

- Machine `891e574f434d38` was destroyed.
- `fly machine list -a seazn-relay-spike`: "No machines are available on this app".
- Kept: the app, all staged secrets (including `SPIKE_YT_DIRECT_URL`), CF Inputs A and B, and B's YouTube Output.

## Teardown record (audio A/B)

2026-10-15 ≈16:41Z.

- Machine `784ee345b637d8` was destroyed.
- `fly machine list -a seazn-relay-spike`: "No machines are available on this app".
- Kept: the app, all staged secrets, CF Inputs A and B, and B's YouTube Output.
### F-004 addendum (2026-10-15, owner-confirmed after the redo)
The **30 s gap (14:52:20–14:52:50 London) did NOT keep the same broadcast**: YouTube started a new one.
Combined with the 5 s and 15 s gaps (same broadcast), YouTube's split threshold for this setup
(scheduled stream, Normal latency) lies **between 15 s and 30 s**. One sample per gap.
**Changes:** every recovery path the organiser's broadcast must survive has to finish its output gap
**under 15 s**. A new-attempt cold start (~25 s Machine boot plus warm-up) will likely split it.
Re-check ADR 0004 and the C14 chaos budgets against 15 s, and add it to the C17 runbook.

### F-006 (2026-10-15, from R2002 PR #966): relay contract facts C07 must honour
- `fixtureId` is **nullable** in the session GET (the session outlives its fixture). C07's
  `fixture_id` must be `Option<String>`.
- Both internal replies (session GET and heartbeat) are wrapped as `{ ok, data }`. C07 unwraps `.data`.
- The vendored contract is `docs/contracts/relay-job.v1.json` in seazn.club, **generated** from the zod
  schemas by `scripts/gen-relay-contracts.ts` (sha256 at #966: c0d87a54…375d). C07 vendors that file;
  never hand-edit it.
- `RELAY_OVERLAY_DELAY_DEFAULT_MS = 3100` (provisional, F-002) lives in `server/relay/config.ts`.
  R2004 imports it.
- **Risk for R2003:** web's heartbeat handler already fires `callback_stopped` on a `state: stopped`
  beat for a composed session. The final beat on a `fatal` exit is also `stopped`, so R2003 must
  decide explicitly whether `exitReason: fatal` ends the session or books `r<n+1>`. Pending the
  R2002 review's read of the handler.

## Appendix F-007: raw polled state and events

Generated from the probe's per-poll JSONL. Every poll is counted in a run; consecutive identical states are merged.

#### M1 exit 1, max_retries 2, sleep 20

Polls: 532 (interval 200 ms for M1/M3, 1 s for M2). HTTP non-200: 3.

**Events (`events[]`, Fly timestamps, UTC):**

| UTC | type | status | exit_code | restarting | oom_killed | restart_count |
|---|---|---|---|---|---|---|
| 18:12:18.290 | launch | pending |  |  |  |  |
| 18:12:18.434 | launch | created |  |  |  |  |
| 18:12:21.347 | start | started |  |  |  |  |
| 18:12:41.655 | exit | stopped | 1 | True | False | 1 |
| 18:12:41.764 | restart | starting |  |  |  |  |
| 18:12:43.062 | start | started |  |  |  |  |
| 18:13:03.480 | exit | stopped | 1 | True | False | 2 |
| 18:13:03.589 | restart | starting |  |  |  |  |
| 18:13:04.987 | start | started |  |  |  |  |
| 18:13:26.084 | exit | stopped | 1 | False | False | 2 |
| 18:18:48.988 | start | starting |  |  |  |  |
| 18:18:50.180 | start | started |  |  |  |  |
| 18:19:10.591 | exit | stopped | 1 | False | False | 2 |

**Polled `state` runs (consecutive identical states merged):**

| from (UTC) | to (UTC) | state | polls | HTTP |
|---|---|---|---|---|
| 18:12:31.908 | 18:12:41.465 | started | 45 | 200 |
| 18:12:41.667 | 18:12:42.678 | starting | 6 | 200 |
| 18:12:42.975 | 18:13:03.312 | started | 81 | 200 |
| 18:13:03.519 | 18:13:04.743 | starting | 7 | 200 |
| 18:13:04.945 | 18:13:25.975 | started | 98 | 200 |
| 18:13:26.179 | 18:13:48.729 | stopped | 101 | 200 |
| 18:13:54.014 | 18:13:54.014 | None | 1 | 0 |
| 18:14:03.450 | 18:14:03.450 | None | 1 | 0 |
| 18:14:18.681 | 18:14:18.681 | None | 1 | 0 |
| 18:14:26.321 | 18:14:26.321 | stopped | 1 | 200 |
| 18:18:44.537 | 18:18:48.888 | stopped | 10 | 200 |
| 18:18:49.096 | 18:18:49.917 | starting | 5 | 200 |
| 18:18:50.126 | 18:19:10.491 | started | 87 | 200 |
| 18:19:10.693 | 18:19:49.993 | stopped | 88 | 200 |

**Crash gaps:**

| exit (UTC) | exit_code | restarting | exit→restart event ms | exit→started event ms | states polled in the gap |
|---|---|---|---|---|---|
| 18:12:41.655 | 1 | True | 109 | 1407 | starting → started |
| 18:13:03.480 | 1 | True | 109 | 1507 | starting → started |
| 18:13:26.084 | 1 | False | — | — | started → stopped |
| 18:19:10.591 | 1 | False | — | — | started → stopped |

#### M3 exit 78, max_retries 2, sleep 10

Polls: 294 (interval 200 ms for M1/M3, 1 s for M2). HTTP non-200: 0.

**Events (`events[]`, Fly timestamps, UTC):**

| UTC | type | status | exit_code | restarting | oom_killed | restart_count |
|---|---|---|---|---|---|---|
| 18:12:20.553 | launch | pending |  |  |  |  |
| 18:12:20.698 | launch | created |  |  |  |  |
| 18:12:23.672 | start | started |  |  |  |  |
| 18:12:34.003 | exit | stopped | 78 | True | False | 1 |
| 18:12:34.119 | restart | starting |  |  |  |  |
| 18:12:35.429 | start | started |  |  |  |  |
| 18:12:46.087 | exit | stopped | 78 | True | False | 2 |
| 18:12:46.200 | restart | starting |  |  |  |  |
| 18:12:47.550 | start | started |  |  |  |  |
| 18:12:57.927 | exit | stopped | 78 | False | False | 2 |

**Polled `state` runs (consecutive identical states merged):**

| from (UTC) | to (UTC) | state | polls | HTTP |
|---|---|---|---|---|
| 18:12:31.870 | 18:12:33.705 | started | 10 | 200 |
| 18:12:33.913 | 18:12:33.913 | stopped | 1 | 200 |
| 18:12:34.113 | 18:12:34.999 | starting | 5 | 200 |
| 18:12:35.200 | 18:12:45.562 | started | 50 | 200 |
| 18:12:45.767 | 18:12:45.767 | stopped | 1 | 200 |
| 18:12:46.098 | 18:12:47.316 | starting | 6 | 200 |
| 18:12:47.522 | 18:12:57.447 | started | 33 | 200 |
| 18:12:57.686 | 18:13:38.353 | stopped | 188 | 200 |

**Crash gaps:**

| exit (UTC) | exit_code | restarting | exit→restart event ms | exit→started event ms | states polled in the gap |
|---|---|---|---|---|---|
| 18:12:34.003 | 78 | True | 116 | 1426 | stopped → starting → started |
| 18:12:46.087 | 78 | True | 113 | 1463 | starting → started |
| 18:12:57.927 | 78 | False | — | — | stopped |

#### M2 exit 1, max_retries 1, sleep 330

Polls: 284 (interval 200 ms for M1/M3, 1 s for M2). HTTP non-200: 0.

**Events (`events[]`, Fly timestamps, UTC):**

| UTC | type | status | exit_code | restarting | oom_killed | restart_count |
|---|---|---|---|---|---|---|
| 18:12:23.335 | launch | created |  |  |  |  |
| 18:12:26.237 | start | started |  |  |  |  |
| 18:17:56.960 | exit | stopped | 1 | True | False | 1 |
| 18:17:57.068 | restart | starting |  |  |  |  |
| 18:17:58.377 | start | started |  |  |  |  |
| 18:23:29.415 | exit | stopped | 1 | True | False | 1 |
| 18:23:29.528 | restart | starting |  |  |  |  |
| 18:23:30.815 | start | started |  |  |  |  |
| 18:24:05.880 | destroy | destroying |  |  |  |  |
| 18:24:12.276 | destroy | destroyed |  |  |  |  |

**Polled `state` runs (consecutive identical states merged):**

| from (UTC) | to (UTC) | state | polls | HTTP |
|---|---|---|---|---|
| 18:19:13.208 | 18:23:28.604 | started | 237 | 200 |
| 18:23:29.604 | 18:23:30.610 | starting | 2 | 200 |
| 18:23:31.615 | 18:24:05.755 | started | 35 | 200 |
| 18:24:06.756 | 18:24:11.953 | destroying | 6 | 200 |
| 18:24:12.957 | 18:24:15.966 | destroyed | 4 | 200 |

**Crash gaps:**

| exit (UTC) | exit_code | restarting | exit→restart event ms | exit→started event ms | states polled in the gap |
|---|---|---|---|---|---|
| 18:17:56.960 | 1 | True | 108 | 1417 |  |
| 18:23:29.415 | 1 | True | 113 | 1400 | starting |

## F-019: a stop's trap can cost bash the child's exit status (C12, 2026-10-23)

**Where:** `docker/supervise.sh` as built in PR #36's review round 1 (`6b077fb`): the shell that traps
INT and TERM also `wait`ed on the compositor, and re-waited after each trap.

- **Measured:** check.yml run 39420277627, bash 5.2.21 on ubuntu-24.04. 150 races, where the stub child
  exits 75 by itself at 0.30 s and an INT lands at 0.30 ± 0.015 s, gave `{75: 148, 130: 1, 255: 1}`.
  130 is `wait`'s own status when a trap interrupts it (128 + SIGINT); it was kept because the re-wait
  answered 127, so bash had reaped the child and dropped its status. The 255 is a status the child
  never had. The same 150 races passed on macOS bash 3.2.57 and 5.2.21, so the host is no witness.
- **What it costs:** a stop that lands as the compositor ends on its own (a deadline or C11b's 75
  within milliseconds of a SIGTERM) makes the entrypoint exit 130 or 255. Fly reads a non-zero exit as
  a failure and restarts an ended session (F-017's consequence), or a 75 reads as a crash.
- **Fix:** the compositor runs under a waiter, a subshell that has no traps (a subshell resets them)
  and that tini never signals. The waiter writes the compositor's pid, then its status, each to a file
  in a private `/tmp` directory. The trapping shell reads a pipe that carries no data until the
  waiter ends, then reads the status file. Data cannot go through the pipe: on a macOS host, bash
  3.2.57's `read` dropped the byte it was reading when a trap ran (a `status=75.` line lost its `.` in
  1 of 200 races).
- **Guard:** `scripts/supervise_test.py`'s race, with each race timed from the child's own start.
  check.yml runs 500 races. A new case pins that the child's stdout reaches the wrapper's, never the
  waiter's pipe. ~~at round 1's measured rate, 500 clean races happen by luck about once in 800 runs~~
  (wrong, corrected 2026-10-23 by the PR #36 final review's m1): mutant 37, round 1's design exactly,
  lost 1 of 500 at the same ±15 ms draw (PR #45, check run 39931935374), so with round 1's 2 of 150 it
  is 3 of 650, and 500 clean races came by luck in about 1 run in 3 to 1 in 10. Mutant 35 (c080c3f's
  loop) lost 5 of 500. **Every printed loss had its INT at 0.301 s** (4 of 4), the 1 ms after the
  child's own exit, where the ±15 ms draw puts about 1 race in 30. So 400 of the 500 races now draw the
  INT from +0 to +3 ms after the exit, and 100 keep ±15 ms for timing drift; the tally prints each
  share. Measured on the round-3 mutant PR #46 (Owed 75): 37 lost 16, 15 and 14 races of 500 (3 of 3
  runs red), 35 lost 32 of 500.

## F-020: a CEF child's `/proc/<pid>/environ` shows its process title, not its environment (C12, 2026-10-23)

- **Measured on the runner:** image run 39931930126 (PR #44, mutant 29: `remove_var` deleted, so the
  compositor's live environment kept `JOB_TOKEN`) reported all 6 CEF children "no JOB_TOKEN in its
  environment". Their files read 2 to 509 "lines" of NUL padding, against the compositor's 15 (the
  PR #36 final review's I-1). The probe's "readable, more than 0 lines" guard counted the padding.
- **Measured on Fly** (the coordinator's Owed 76 probe at `341ae7a`): the two zygotes' and the
  NetworkService child's files are 754 or 785 bytes of NUL only; the other three hold only title text
  (one non-empty line each, none starting with `/opt/gstcef`). The old guard counted NUL padding as lines, so on
  Fly it would have passed all 6 vacuously, exactly as on the runner.
- **Cause (by mechanism, not traced to a frame):** a Chromium child sets its process title from its
  command line, which reuses the argv and environ area; `/proc/<pid>/environ` reads that area.
- **So:** no read of a CEF child's `/proc/<pid>/environ` witnesses its environment. The probe now
  witnesses it at exec, in one dedicated container: `scripts/cef_env_witness.sh` (test only, bind-mounted,
  never in the image), named by gstcefsrc's `GST_CEF_SUBPROCESS_PATH` (gstcefsrc.cc at the F-001 pin,
  lines 852-856, which then puts the real gstcefsubprocess in `CEF_SUBPROCESS_PATH`; the C00 spike's
  `cef-nice.sh` used the same knob), records for each child CEF execs whether `JOB_TOKEN` and three
  controls (`DISPLAY`, `SESSION_ID`, `CEF_SUBPROCESS_PATH`) are set, then execs the real subprocess. A
  zygote's forks (the renderer, the GPU process) keep the environment the zygote was exec'd with, and the
  probe requires every live CEF process to descend from a recorded exec. A file nothing rewrites (Xvfb's,
  a bash's, and the compositor's, whose in-process CEF browser sets no title: 24 lines on Fly) is its
  exec-time environment, read as before, now with a control.

## F-021: MediaMTX, the harness's fake Cloudflare and destination: the pin and its API (C13, 2026-10-23)

- **The pin:** `bluenviron/mediamtx:1.21.2@sha256:c7540465f868a02652118bea5a0daab12a29ac0f6cb8485abd535939ecddf886`
  (`harness/compose.yml`). Tag `v1.21.2`, released 2026-10-08T21:42:39Z, the latest release on
  2026-10-23 (GitHub's release API); the digest is Docker Hub's for the tag (its tag API, read
  2026-10-23, pushed 2026-10-08T21:50:03Z).
- **Read from the tag's own files** (2026-10-23): its `mediamtx.yml` and its OpenAPI document.
  **Not run by C13's implementer:** no local Docker (owner ruling 2026-10-23). The image first ran
  in harness.yml on PR #48 (harness run 39979756278, green); the facts measured there are below.
- **The API paths a case uses** (the brief's `/v3/srtconns/...` do not exist at this tag):
  - `GET /v3/srt/conns/list`, then `POST /v3/srt/conns/kick/{id}` for the reader (C14 case 3,
    `reader-kicked`); `GET /v3/srt/conns/get/{id}`; the RTMP twins are `/v3/rtmp/conns/list`,
    `/v3/rtmp/conns/get/{id}` and `/v3/rtmp/conns/kick/{id}`; also `GET /v3/paths/list` and
    `GET /v3/recordings/list`.
  - `GET /v3/config/global/get` (C14 case 18, the PR #76 confirm pass's n-1; read from the tag's
    source 2026-10-25, `internal/api/api_config_global.go`): the running global config, redacted by
    MediaMTX (`conf.Redact`). Durations are `conf.Duration` strings (`internal/conf/duration.go`: a
    day count `<n>d` when there is one, then Go's `time.Duration` String(), e.g. `1m0s`); its
    `readTimeout` defaults to 10 s (`internal/conf/conf.go`'s `setDefaults`).
  - An `SRTConn` carries `id`, `created`, `remoteAddr`, `state`, `path`, `query`, `user` and the
    SRT statistics (`msRTT`, `packetsReceivedLoss`, `packetsRetrans`, `mbpsReceiveRate`, …). The
    compositor's reader is the connection whose `remoteAddr` is 172.28.0.10 and whose `path` is
    `cam`.
- **Config facts that bind the harness** (`harness/mediamtx.yml`):
  - `api` is off by default, and the default users allow the API from localhost only, so the
    harness's single `any` user carries `action: api` (the API is reachable only inside the compose
    network: no port is published).
  - The SRT passphrases are per path: `srtReadPassphrase` and `srtPublishPassphrase`, set from the
    environment (`MTX_PATHS_CAM_SRTREADPASSPHRASE`, `MTX_PATHS_CAM_SRTPUBLISHPASSPHRASE`), so the
    run's random value is in no file and no argv.
  - Recording: `record`, `recordPath` (variables `%path %Y %m %d %H %M %S %f`, `%f` in
    microseconds, plus `%z` and `%s`), `recordFormat` (`fmp4` or `mpegts`), `recordPartDuration`
    (default 1 s, the loss on a crash), `recordSegmentDuration` (a minimum: a segment ends at a
    keyframe after it), `recordDeleteAfter` (default 1 day; `0s` disables).
  - `useAbsoluteTimestamp: false` by default: a segment's name is the wall clock when it started,
    in the container's time zone (the harness sets `TZ=UTC`; the checker reads names as UTC).
    **Measured (harness run 39979756278, ffprobe on the uploaded recordings):** "when it started"
    is when the segment's FIRST SAMPLE ARRIVED. Each segment's PTS restarts near 0 (video first PTS
    0.000 to 0.021 s, audio 0.000). The first segment after the compositor connected held 352 frames
    over 11.70 s of PTS, but was named only 9.71 s before the second: its first frames arrived as a
    backlog. So name plus PTS runs a connect's first segment past the next segment's name, and the
    checker re-dates such a segment from the next one (`_INDEX.md`, decision row "C13 CI round 2"
    (2); Owed 80).
  - The protocol switches at this tag: `rtsp`, `rtmp`, `hls`, `webrtc`, `srt` and `moq` (MoQ is
    on by default; the harness turns every one off but `rtmp` and `srt`). An unknown key fails the
    start.
  - The image is `scratch`-based: no shell and no `tc`. `netem-in` is a sidecar from the harness
    image in MediaMTX's network namespace, which is how the input leg is impaired at its sender.
- **Changes:** the C13 brief's kick paths are a false premise (`_INDEX.md`); C14 uses the paths
  above.

## F-022: the zygote watch's first listing came after the ready (image run 40015873298, 2026-10-23)

- **Measured** (the coordinator's diagnosis of image run 40015873298 and the image.yml runs since
  2026-10-20: 11 completed the probe's 32-exit sweep, 44 zygote kills): the child poll starts at gen 1's
  restart, before CEF forks a zygote, so its first list holds none; it lists every `CHILD_POLL_MS`
  (1 s), so the zygotes were first listed by the 2nd poll, at about restart + 1.01 s, while
  `cef ready gen=1` came at about restart + 0.72 s. 43 of 43 passing kills got the verdict at
  restart + 2.017 to 2.036 s (the 3rd poll). The one that failed (exit 6 of 32) got none: 15
  `cef alive gen=1` beats over the next 30 s, no "a CEF zygote is gone", no `cef health` line, no exit
  line. Its run's probe was the fastest measured (its ready-to-stop gaps were the 4 shortest of 128,
  0.100 to 0.157 s; the median 0.188 s), so its kill most likely landed before the 2nd poll.
- **`docker inspect -f {{.State.ExitCode}}` of a running container prints 0**: the probe read
  "exit status 0 (30.2 s)" for a container that never exited.
- **So:** a watch that learns its baseline from a periodic poll is blind from the moment the process
  exists to the first poll that lists it. The adapter now lists at every ready too (the decision log's
  "Zygote watch at ready"); what is left is a zygote that dies between CEF's start and the first
  ready (Owed 70 (e)).

## F-023: a sink that connects after the fault clears delivers its backlog in a burst, and MediaMTX's recorder discards it (chaos run 40045826841, 2026-10-24)

- **Measured** (C14 fix round 1, from the run's `dest-disconnect`, `dest-trickle` and `dest-stall`
  artifacts; the PR #52 review's I9):
  - **dest-disconnect (case 4):** `netem out loss 100%` from 00:41:54.188 to the clear at
    00:42:24.421. The output's last sink before the clear swapped at 00:42:20.409 (gen 5), about 4 s
    before it. Its connect blocked under the fault; at the clear it delivered the frames queued since
    its swap: the first recorded frame after the clear is camera counter 1502, the counter on air at
    the gen 5 swap. About 4.1 s of backlog: the 2 s `ENCODED_RING_SPAN` and the 512 KiB
    `OUTPUT_QUEUE_BYTES` (about 1–2 s at the programme's rate).
  - In the same second MediaMTX v1.21.2's recorder logged `WAR [recorder] sample of track 1 received
    too late, discarding` and `ERR [recorder] detected drift between recording duration and absolute
    time, resetting`. It closed a 34-frame segment (00:42:24.541) and opened the next at
    00:42:26.525: camera counters 1505 to 1743 (238, about 7.9 s of content) reached MediaMTX and
    were not recorded. The checker reads that as outage 2, `lost_ms` 7 082 against
    BUDGET_OUTPUT_LOST_MAX_MS 2 000.
  - Outage 3 (00:42:36.525 to 00:42:38.407, counter gap 0, `lost_ms` −1 882) is the checker's
    dating, not a loss: MediaMTX names a segment by its first sample's arrival (F-021), 11.88 s after
    a 300-frame (10 s) segment (Owed 80).
  - **dest-trickle (case 12):** the same shape. Last swap 00:45:18.958, clear 00:45:22.473, the
    recorder's "too late" and drift reset at 00:45:23; three outages, `lost_ms` [−4 109, 30 366,
    −1 884] (the 30 366 is outage 2 dated from a burst-named segment that overlaps outage 1).
  - **dest-stall (case 5):** its last swap (00:42:23.926) came 0.86 s after the clear
    (00:42:23.064), so no sink sat connecting under the fault. The recorder logged only "too late",
    no reset, and the run had one outage (`lost_ms` −911).
- **So:** when a sink's connect outlives the fault, the product sends the stale backlog it queued
  since the swap at the connect, faster than real time, and this recorder refuses it and resets. A
  real ingest's answer to the burst (Cloudflare, YouTube) is unmeasured. The verdicts are not
  loosened: cases 4 and 12 stay red on `output_lost` and `no_outage_outside_fault` until the owner
  rules Owed 84. The negative `lost_ms` values also show that the checker's `counter_gap − wall_span`
  is not "programme lost" when a connect replays its backlog (Owed 84 (c)).

## F-024: four facts from C14's second CI round (chaos runs 40051995233 and 40051902711, mutants 40051985045, 2026-10-24)

- **A restarted container's CEF mostly fails to start** (Owed 85). Read from the 18 container
  restarts in the two runs: cases 8, 9 and 15's threshold variant, 3 runs each, twice.
  - The restarted compositor's first build logged `init_cef: Failed to initialize CEF`
    (gstcefsrc.cc:903) in 13 of them, 144–175 ms after the container's `start` event in the 4 timed. Its next two
    builds failed at once ("the pipeline did not start"), and graphics ended `failed`.
  - By exit kind: 75 (C11b's restart) 7 of 7; 133 (Chromium's abort) 4 of 5; 137 (the compositor
    SIGKILLed) 2 of 6. The 2 recoveries timed logged `cef ready gen=1` about 0.83 s after the start.
  - A restarted container's compositor starts 11–15 ms after the `start` event. A fresh container's
    starts 115–124 ms after it, which is the entrypoint's wait for Xvfb's socket. Compose restarts
    the container rather than recreating it, and `/tmp` is on its writable layer, so the previous
    Xvfb's `/tmp/.X11-unix/X99` passes the wait at once. Whether that is the cause is not proven
    here: CEF's own log is off (`log-severity=disable`, F-013). F-025 measured the cause: the
    restarted container's Xvfb refuses its predecessor's lock.
- **Case 5's swap comes from the sink's own socket timeout, not the stall rule.** Under
  `netem out rate 1kbit`, rtmp2sink failed with `Connection error: Socket I/O timed out` 5.6 s after
  the cut in both runs, and the sink was swapped 0.5 s later. With `output_stall_after_ms` doubled
  (mutant 9), the swap still came 1 332 ms after the last growth beat. Under 8 kbit (case 12) no
  run logged that error, so the stall rule swapped.
- **Under CPU starvation the ladder never moved** (Owed 86). 16 busy loops in the compositor's
  cgroup for 40 s:
  - The window carried 11.5 fps (dispatch) and 24.7 fps (PR), with encoderSpeed 0.37–0.67 and
    0.63–1.03.
  - Every beat read fps 29–31, droppedFrames 0 and rung `full`.
  - After the burn the encoder caught up at encoderSpeed 2.97 and 15 Mbps (dispatch).
- **A fast GPU-death loop outruns the 1 s child poll** (Owed 70 (a)). In 6 of 6 runs Chromium
  relaunched the GPU process in about 80 ms. The loop's 5 kills of 5 distinct pids took 312–337 ms,
  and `cef health` counted `gpu_process_deaths=1`. No restart followed.

## F-025: Owed 85's cause: a restarted container's Xvfb refuses its predecessor's lock (image run 40057826178, 2026-10-24)

The follow-up to F-024's first fact (Owed 85; F-024 is on `feat/c14-chaos`, unmerged at the time).

- **Measured, the real Xvfb** (image run 40057826178, head `f190df4`, the entrypoint unchanged; the
  in-place restart probe, `scripts/image_probe.py --restarts 6`): containers under Docker's
  `--restart on-failure:2` (ADR 0007 item 5's Docker equivalent), which restarts the same container,
  so its writable layer and `/tmp` persist as under compose. `scripts/xvfb_witness.sh`, bind-mounted
  ahead of `/usr/bin/Xvfb`, logged each Xvfb start's pid, the pid in `/tmp/.X99-lock` and Xvfb's own
  stderr (the entrypoint sends that to `/dev/null`, which is where the reason had gone).
  - 2 of the 4 restarts made (after 137 and after 133, in two containers): the new Xvfb started as
    pid 10, found `lock=10 socket=yes`, and said `Fatal server error: (EE) Server is already active
    for display 99`. No Xvfb ran in the new life, `/proc/net/unix` held no X99 socket, the
    compositor's first build failed `init_cef: Failed to initialize CEF` (the GStreamer line's pid
    18) and graphics went `failed` 3.5 s later. Those two containers' second restarts could not be
    made (no CEF in the life before), so 4 of the 6 owed were checked, and the probe was red.
  - The other 2 (after 75 then 133, one container): the new Xvfb was pid 11 against the lock's 10,
    then pid 10 against 11; Xvfb found the lock's pid gone, took the lock as stale, and CEF was ready
    in 2.2 s and 1.4 s, with one Xvfb and the lock naming it. Both met the previous life's socket
    (`socket=yes`), and it did not stop them: Xvfb's transport unlinks the path before it binds
    (xtrans 1.5.0's `SocketUNIXCreateListener`).
- **Xvfb's rule** (xorg-server 21.1.12, Ubuntu 24.04's): `LockServer` (os/utils.c, called from
  `OsInit`, os/osinit.c:313) reads the pid from an existing lock and calls `kill(pid, 0)`. Only
  `ESRCH` makes the lock stale; success or `EPERM` is `FatalError("Server is already active for
  display …")`, and the pid it finds live can be its own. The refusal comes before
  `CreateWellKnownSockets` (dix/main.c), which unlinks and binds the socket path, so the old
  `/tmp/.X11-unix/X99` stays, and the entrypoint's `[ -e /tmp/.X11-unix/X99 ]` wait passed on it at
  once (F-024's 11-15 ms against 115-124 ms).
- **Why the pid repeats:** a fresh container's Xvfb is pid 10 (image run 40027305462's table: tini 1,
  the entrypoint 7, Xvfb 10, the waiter 20, the compositor 21), after the entrypoint's two `prlimit`
  forks, which never vary. A container start is a new pid namespace, so a restart's forks repeat:
  both failed restarts above, and 12 of round 2b's 13 below (the 13th: 19), logged the compositor as
  pid 18 in the `init_cef` line, three forks fewer than a fresh start's 21: the three `sleep 0.1` the
  wait loop skipped. The
  namespace's first pids are not fixed, though: 1 life in 6 here started one pid later (Xvfb 11),
  and such a life recovers when the lock's pid is gone at the check: C14's 5 recoveries in 18,
  twice.
- **C14's runs, the same signature:** round 2b (chaos runs 40056474269 and 40056388445, head
  `66db5f4`) failed `init_cef` after 13 of 18 restarts again: 75 6 of 7, 133 3 of 5, 137 4 of 6.
  Its `container-diff.txt` (names, `docker diff`) shows what a restart inherits: `/tmp/.X11-unix/X99`
  and `/tmp/.X99-lock`, beside HOME's dconf, GStreamer registry and NSS database.
- **Ruled out:** Chromium's singleton files (hypothesis 2) and `/dev/shm` leftovers (3). The CEF
  profile, CEF's root cache path, is under `TMPDIR=/dev/shm/seazn`, and Docker mounts a fresh
  `/dev/shm` at each start: in every restarted life the probe listed, `/dev/shm/seazn` held only that
  life's own `seazn-cef-<pid>-…` (and its `.org.chromium.Chromium.*`), and the profile sweep removed
  nothing; `container-diff.txt` lists no singleton file.
- **Fly** (corrected 2026-10-24, the PR #54 review's m1: this said F-018 MEASURED the reset across an
  `on-failure` restart; no file was ever checked across one): cited, not measured. Fly's docs say
  "Restarting wipes the ephemeral file system" (docs.fly.io/apps/restart) and `persist_rootfs`
  defaults to `never`; web's Machine create sets no `persist_rootfs` (seazn.club
  `apps/web/src/server/relay/runner-fly.ts`). What was measured is narrower: a Fly probe for Owed 73
  (2026-10-20, Machine `48e4533c564ed8`; its record is outside this repo) found a file it had left
  in `/tmp` gone at each of 10 starts after a requested stop. So Owed 85 is a restart that keeps the
  writable layer (compose, Docker's `--restart`, ADR 0007 item 5's Docker equivalent). The fix holds
  on both, and on a Machine that ever sets `persist_rootfs` too: no X lock is written for a later
  boot to meet.
- **So** (the fix, `docker/xvfb.sh`, sourced by the entrypoint; corrected 2026-10-24 by the PR #54
  review's C1 and I1, below): `start_xvfb` starts Xvfb with `-displayfd`, which removes both halves
  of the cause. It sets Xvfb's `nolock` (xorg-server 21.1.12, os/utils.c's `ProcessCommandLine`;
  `LockServer` then returns at once), so Xvfb neither reads nor writes `/tmp/.X99-lock` and no life
  leaves one for the next; and ready is Xvfb's own display line on that fd (`NotifyParentProcess`,
  os/connection.c, called from dix/main.c once its sockets listen and its screen and input are up),
  read from a file made new at each start, instead of a socket file that may be an earlier life's,
  so an Xvfb that exits first fails the start at once (70, "it exited before it was ready"). The
  socket an earlier life left needs nothing: Xvfb replaces it (above). The check is image.yml's
  "In-place restart probe" step: 6 restarts, each kind twice, every one made whatever an earlier
  check found; after each, CEF ready, one Xvfb with `-displayfd`, no lock, and that Xvfb listening
  on `/tmp/.X11-unix/X99`; per container, the witness shows no start met a lock and each restart met
  the previous life's socket.
- **Corrected 2026-10-24 (the PR #54 review's C1 and I1).** The fix as first built (`e7ab150`)
  also removed the display's lock and socket before Xvfb started, and this finding called that
  removal what made a restart safe, and had the probe require a lock naming the live Xvfb. Both were
  wrong: under `-displayfd` there is no lock to remove or to name, and the socket is replaced by
  Xvfb itself. Head `34632fd`'s image run 40058946304 shows it on the real Xvfb: CEF ready after each
  of the 3 restarts made (75, 137, 133), every life's lock `''` (none), and the probe red on its own
  lock check, whose failure then cancelled each container's second restart (the review's I2). Either
  half alone fixed the restart, so neither was tested. **Decided** (coordinator-medium, 2026-10-24,
  decision log): `-displayfd` stays and the removal goes; beside `-displayfd` it changed nothing
  observable (the stub test's hand mutant that puts it back changes no outcome, only what each case's
  Xvfb met), and alone it would keep the socket-file wait that a leftover socket fools.

## F-027: where Owed 84's backlog queued, and how rtmp2sink 1.24.2 connects (source read; a fake destination checked against ffmpeg, 2026-10-24)

The follow-up to F-023 (on `feat/c14-chaos`, unmerged at the time). Source: GStreamer 1.24.2,
`subprojects/gst-plugins-bad/gst/rtmp2/` (`gstrtmp2sink.c`, `rtmp/rtmpclient.c`,
`rtmp/rtmpconnection.c`), read at the tag.

- **Where the frames queued** (`gst_output.rs` before the fix): each output generation's feeder
  started at its build and took the encoded ring from its oldest entry (up to `ENCODED_RING_SPAN`,
  2 s by PTS). rtmp2sink's `render` waits `while (is_running && !self->connection)`, so whatever the
  feeder pushed during a pending connect waited in the output appsrcs (`OUTPUT_QUEUE_BYTES`, 512 KiB
  each), in flvmux's queue and in `render`, and went out at the connect as fast as the socket took
  it. The swap's force-key event did make a fresh keyframe, but the feeder had started at the ring's
  oldest keyframe, before it.
- **When rtmp2sink can send:** `async-connect` defaults to TRUE, so `start()` begins the connect
  task (with FALSE the connect waits for the first buffer). `self->connection` is set only in
  `connect_task_done`, after the client's chain (`connect` `_result`, `createStream` `_result`,
  then `publish`'s `onStatus` `NetStream.Publish.Start`) has finished. `stats` returns that
  connection's counters. Without a connection it returns the counters saved when the last one
  ended (`self->stats`, saved at `gstrtmp2sink.c` 990 and read at 1215ff), and a structure of zeros
  before any. The saved counters are cleared when the connect task starts (977), and each build
  makes a new rtmp2sink. So this sink's `out-bytes-total` reads 0 until its publish is accepted, and
  above 0 from then on (the commands it has sent; F-011's 476 B). The middle branch was added in
  PR #57's review round 2 (m-2).
- **What rtmp2sink gives up at its 5 s `timeout`:** the TCP connect (`GSocketClient`'s timeout) and
  the connection's writes (`gst_rtmp_connection_write_buffer_done` emits the error). Not a held
  reply: the connection's read source retries a read that hit the socket's timeout
  (`gst_rtmp_connection_input_ready`: `G_IO_ERROR_TIMED_OUT` reads "continuing"). So a destination
  that takes the TCP connection and holds its `publish` answer holds rtmp2sink's connect until the
  domain's `output_connect_timeout_ms` swaps it. The handshake's own reads were not checked.
- **A connection error during the connect fails it at once:** the client's `connection_error`
  returns the task's error, and `connect_task_done` posts `Failed to connect` on the bus.
- **The fake destination** (`crates/adapters/tests/support/rtmp.rs`, the gst legs' destination):
  checked on the host against ffmpeg's RTMP publish (`-re`, H.264 and AAC, the publish held 1.5 s,
  then released): 1 connection accepted, 1 publish, 312 media messages (120 video, 189 audio), the
  first video message a keyframe at timestamp 0, no timestamp going back, and the FLV it writes
  decodes in ffprobe (120 H.264 frames, 189 AAC frames). rtmp2sink against it runs in CI only (no
  GStreamer on the host).
- **The gate's cost** (PR #57 review m-5): while the sink connects, the feeder reads the gate at
  each ring push (about 77 a second at 30 fps with AAC) and at each 20 ms `WAKE`. That is about 80 to
  130 `stats` reads a second, each a lock and a copy of a small structure. It stops when the gate
  opens.
- **flvmux forwards no EOS before its first buffer** (gst-plugins-good 1.24.2 `gstflvmux.c`,
  `gst_flv_mux_aggregate`). In its header state, with no buffer on any pad, it asks for more data
  (2057-2059) before it reaches the all-pads-EOS check (2128). These are the PR #57 review's line
  numbers (its I-1). So an output whose feeder sent nothing never posts EOS, and a drain that waited
  for it waited out `drain_timeout_ms`. Mutant run 40084647054's G3 leg (no keyframe request)
  witnessed it: a file sink that got nothing never drained. The feeder now ends that drain itself
  (`release_unsent`).
- **The version:** these facts are 1.24.2's. `the_gate_reads_gstreamer_1_24`
  (`crates/adapters/tests/programme_output.rs`) fails when the runner's core, rtmp2sink or flvmux
  leaves 1.24.x. It runs in check.yml only, so it covers the image only while both install Ubuntu
  24.04's apt GStreamer.

## F-028: Owed 80's 2 s overlap is MediaMTX's start, not a compositor backlog (harness runs 40081524300 and 40084630169, 2026-10-24)

Measured from the two harness smoke runs on PR #57 (heads `bc159b3` and `3fc0fc5`, both with Owed
84's gate), from their artifacts: `report.json`'s `segment_overlaps`, `compositor.log`,
`services.log`, and ffprobe over the recordings (`-show_entries packet=pts_time,flags`; `K` is the
MP4 sync-sample flag). The PR #57 review measured the first run the same way and also read its NAL
types.

| | 40081524300 (`bc159b3`) | 40084630169 (`3fc0fc5`) |
|---|---|---|
| Gate open (`the sink can send`) | 09:26:30.258, 32 ms after the build, 1 video frame dropped | 10:10:46.977, 17 ms after the build, 0 dropped |
| MediaMTX `is publishing to path 'live/dest'` | 09:26:32 (the log has whole seconds) | 10:10:48 |
| Recorder warning | `sample of track 1 received too late, discarding` | none |
| Segment 1 | `09-26-32-234771`: 359 video packets, the first 59 not keyframes, first keyframe at PTS 1.991 s, then every 2.000 s | `10-10-48-917301`: 300 video packets, a keyframe first at PTS 0, then every 2.000 s |
| Segment 2 named after segment 1 | 9.974 s | 7.986 s |
| `overlap_ms`, `content_gap_ms` | 1 992, 33 | 2 014, 0 |

- **No compositor backlog:** in both runs, at most one frame was queued when the gate opened, and
  that frame was dropped; the compositor starts at a keyframe made after the gate (Owed 84's gst
  legs, not measured here). The 2 s overlap is unchanged from Owed 80's measurement on main.
- **The destination's start:** in both runs MediaMTX logged `is publishing` about 2 s after the
  gate, and segment 1 holds about 2 s more content than its name's span to segment 2. The likely
  reading, which is not verified, is that MediaMTX reads about 2 s of the stream before it publishes
  (its RTMP track probe) and records it under a name taken when it starts.
- **The lost keyframe, in one run of two:** in the first run the recorder discarded a sample, and
  segment 1 opens with 59 frames that are not keyframes. The keyframe asked for at the connect was
  lost, and x264's next one (`key-int-max` = 2 × fps) came 2.0 s later. If a destination does that at
  each reconnect, each reconnect's recording starts up to 2 s late, which counts against C14's
  `output_reconnect` (Owed 84) and not against `output_lost`.
- Not checked: MediaMTX's source, or what it receives on the wire, and Cloudflare's start.

## F-026: a GPU crash loop between two child polls, leg by leg (C14 case 15, chaos runs 40051995233, 40056474269 and 40056388445, 2026-10-24)

C14's F-024 (PR #52, branch `feat/c14-chaos`) records the fast loop's headline. This finding is its
9 `fast` legs (3 per run) read one by one from `marks.jsonl` and `compositor.log`, for Owed 70 (a).

- **Measured:**
  - Every leg killed 5 distinct GPU pids, 69 to 88 ms apart; the 5 kills took 288 to 337 ms.
  - The `cef health` lines counted `gpu_process_deaths=1` in 7 legs and 2 in 2 (run 40056474269's
    r2 and r3). No leg restarted.
  - 40 to 97 pids were born between two kills.
  - From the deciding `cef health` line to `exit code=75`: 4.5 to 14.1 ms, median 7.5 ms, in 21 legs
    of the same 3 runs (amended 2026-10-24, the PR #55 review's m6; it first read one leg, 7 ms). The
    21 are every `cef-gpu-deaths` `threshold` leg (r1 to r3) and every `cef-gpu-zygote-killed` and
    `cef-renderer-zygote-killed` leg, in each run. The exception is run 40051995233's 6 zygote legs,
    which logged no zygote verdict, so no restart.
  - From the deciding line to the container's `die`: 127 to 177 ms (one `threshold` leg and 3
    `cef-gpu-zygote-killed` legs); the rest is the entrypoint's teardown.
- **How the loop kills** (`harness/in-container.sh`, `cmd_gpu_loop`, read, not run): each pass walks
  `/proc` and forks one subshell per pid to read its `--type`, then kills the first GPU pid of the
  walk. A pass that finds no new GPU pid sleeps 50 ms. So a killed GPU process was born before the
  walk that listed it began, and died after that walk ended: it lived at least one whole walk.
- **Estimate (not measured):** 69 to 88 ms between kills fits one pass that found no relaunch, the
  50 ms sleep, and one pass that found it, so a walk took about (69 − 50) / 2 to (88 − 50) / 2 ms,
  10 to 19 ms. The walk was not timed.
- **The estimate bounds existence, not visibility** (amended 2026-10-24, the PR #55 review's m5). A
  zygote's fresh fork reads `--type=zygote` until it is renamed `--type=gpu-process`. The watch
  lists it as a GPU process only from the rename, and the harness's walk reads the newest pids near
  its end. So the window in which the watch can see a relaunch may be much shorter than one walk.
  The margin is measured only by a kill-to-count latency per death: the image test prints one (PR
  #55), and C14's re-run owes one per leg.
- **So:** the 1 s child poll sees at most one relaunch of such a loop, and a pid-gap or start-time
  inference cannot recover the rest (most of the gap is likely the walk's own forks, one per pid it
  reads, and start times tick at 10 ms). The adapter now runs a GPU watch between polls (decision
  log "Owed 70 (a): the GPU watch"). It misses a GPU process visible for less than its listing pace:
  ~~one 5 ms step while a listing costs under 250 us, and 20 times a listing's cost above that (review
  round 1)~~ in a wait, 5 to 8 ms: 20 times a listing's CPU, held to `GPU_LISTING_GAP_MAX_MS`, which is
  this estimate's 10 ms less a 2 ms margin (amended 2026-10-24, review round 2).
- **A listing's cost in the image** (amended 2026-10-24, the PR #55 confirm pass's I3; image runs
  40080601547 and 40085739077): with up to 6 CEF children, a listing took 1 439 us on the clock over 21
  processes in a debug build. Over 11 processes it took 370 us of the thread's CPU on the release profile,
  the compositor's (431 us on the clock), and 632 us on the debug one. ~~At 5% of a core the release cost
  paces a wait's listings 7.4 ms apart.~~ (amended 2026-10-24, the PR #55 confirm pass 2's r2-m1 and
  r2-m3; image run 40087528576.) The next run's release listings averaged 483 and 516 us of CPU, over
  the 400 us at which the 8 ms ceiling binds, so 9 of its 11 release waits held a gap there, and its held
  death waits listed at 5.2 to 7.5% of a core. The coordinator accepted 7.5% of one of 4 cores for at
  most 2 s per death. The worst case: a listing whose CPU reaches the ceiling is followed as it returns,
  so a wait lists back to back, one full core for each death's 2 s wait.
- **The exit's margin** (m6): the fast variant's `gpu_death_split` and `last_kill_not_before_die`
  checks go red when the browser outlives the deciding line long enough for further relaunches to be
  killed. That is one kill interval (69 to 88 ms) less the count and the driver's tick (about 5 to
  40 ms, the review's figure): about 30 to 80 ms from the decision to `exit code=75`. The 21 legs
  measured 14.1 ms at the most.

## F-029: Owed 81's judder starts at the fake camera's mux, and the compositor passes it on 1:1; the real path's exposure is unmeasured (harness runs 40089108946, 40089107461 and 40090232700, 2026-10-24)

Owed 81 (C13 CI round 2): on a clean smoke run about a quarter of the programme's frames repeat the
camera frame before and about as many camera frames are never shown, while every beat reads
`fps 30.0` and `droppedFrames 0`. Branch `inv/owed81-frames` (PR #59) adds probes to the harness
only (no product crate changed) and reads one run stage by stage with `scripts/cadence_stages.py`.
Revised 2026-10-24 by the PR #59 review (needs fixes, I-1 to I-5 and m-1 to m-8): the first version
named one cause, claimed a one-line fix and read as settled.

- **Verdict: PLAUSIBLE HARNESS ARTEFACT.** Where it starts is close to proven: the fake camera's
  mux, measured stage by stage, then passed on 1:1. The mechanism is two holds in that mux: the
  audio-wait hold, proven frame by frame at alignment 1 (5 409 of 5 410), and the alignment-7 hold,
  consistent with the rate but not predicted frame by frame. No run has removed both holds, so the
  fix is unproven, and what the product shows on a real Cloudflare input is not measured: that is
  the open half of Owed 81. The counter (`camera.cadence`) is right, and so are the beats for what
  they count. This is not "viewers see no judder".
- **The probes** (harness only, outside the production image): `fake-camera --wire` stamps every
  PES that starts in a buffer the camera's mux hands `srtsink` (a pad probe on the sink, read by
  `harness::wire`; the PTS maps to the counter from each build's first push); `input-tap`, a second
  reader of MediaMTX's camera path built as `adapters::gst_input` builds the compositor's input
  (`srtsrc` latency 1000, `tsdemux`, leaky 1 s queue, `h264parse`, `avdec_h264`, I420 1280x720, an
  appsink that never syncs, two samples, drop), stamps every decoded frame with its strip (compose's
  `probe` profile: the smoke and owed81 cases only); `fake-camera --audio off` and
  `--ts-alignment 1..7` are the controls (cases `owed81-noaudio`, `owed81-align1`).
- **Stage by stage, one run each** (every number below is printed by `cadence_stages.py OUT_DIR`;
  "bunched": arrived under 10 ms after the frame before; "sampled repeats": a 30 fps latest-frame
  tick swept over 66 phases, min / median / max; "held": whole periods from push to sent; "fit":
  programme frames that equal the latest frame of a series at a regular 30 fps tick, one offset per
  10 s segment):

  | Stage | Smoke with the probes (run 40089108946, PR #59 at `ec34aad`, 17 of 17 checks) | Audio off (dispatch 40089107461, `owed81-noaudio`, 5 of 5) | Alignment 1 (dispatch 40090232700, `owed81-align1`, red: never live) |
  |---|---|---|---|
  | push (`emit.jsonl`) | 2 426 frames, 0 missing, gaps 33/34 ms, 0 bunched; sampled repeats median 0 (a knife-edge phase on the whole-ms stamps) | 2 440, 0 missing, 0 bunched; median 0 | 5 411, 0 missing, 0 bunched; median 0 |
  | sent (`wire.jsonl`) | 2 425 video PES + 3 790 audio, 0 bad buffers; **641 of 2 424 gaps bunched (26.4 %)**; sampled repeats 608 / 625 / 823 of 2 364 | 2 437 video PES, 0 audio; **1 547 of 2 436 (63.5 %)**; 1 508 / 1 508 / 1 515 of 2 376 | 5 410 video PES + 8 454 audio; **2 029 of 5 409 (37.5 %)**; 83 / 2 005 / 2 005 of 5 347 |
  | held, push to sent (periods: frames) | 0: 421, 1: 1 701, 2: 303; residues 2, 4, 6, 9, 11, 13 (mod 16) held 100 %, the others 65 to 81 % | 0: 661, 1: 842, 2: 715, 3: 219; every residue 66 to 78 % (no 16-frame structure) | 0: 3 381, 1: 2 029; residues 2, 4, 6, 9, 11, 13 held 100 %, the others 0 %; the audio-wait rule agrees on **5 409 of 5 410** |
  | input (`tap.jsonl`) | 2 351 decoded, 0 missing, 587 bunched; 567 / 573 / 1 021 of 2 288 | 2 368, 0 missing, 1 501 bunched; 1 188 / 1 460 / 1 481 of 2 305 | **0 frames** (the tool: FAIL) |
  | input(c) − sent(c + 2) | median 1 211 ms; 2 092 of 2 351 within 20 ms (89.0 %) | 1 211 ms; 2 063 of 2 368 (87.1 %) | none |
  | shown (recordings) | 498 repeats of 1 994 pairs (25.0 %), 499 skipped counters in 445 skips; PTS steps 33/34 ms only; the checker's `camera.cadence` 499 / 499 / 445 | 1 263 of 1 990 (63.5 %), 1 258 skipped in 689; 33/34 ms only; the checker 1 266 / 1 265 | 0 pairs, slates only (the tool: FAIL) |
  | fit against input | **2 001 of 2 001 (100 %)** | **1 997 of 1 997 (100 %)** | none |
  | fit's contrasts: push / ideal 30 fps / sent / sent(c + 2) | 70.3 / 70.3 / 47.1 / 83.8 % | 37.4 / 38.8 / 35.9 / 71.0 % | none |

  The contrasts are what make the 100 % a finding: the programme is not what a smooth camera would
  give, nor what the mux sent, but exactly what the input received. The review's own controls add
  that an input jittered by ±12 ms still fits 99.7 to 99.9 %, so the fit cannot see a jitter under
  about 12 ms. The five earlier smoke runs, read from their recordings with an independent ffmpeg
  decode that agrees with the checker within 2 (harness runs 40065135397, 40078150465,
  40084630169, 40085739088, 40087096979): 496 to 797 repeats of about 1 990 pairs (25 to 40 %),
  inside the input stage's own phase sweep (567 to 1 021 of 2 288, 25 to 45 %): the run-to-run
  spread is the tick's phase against the arrivals.
- **Where it starts: between push and sent, in the camera's mux.** The camera pushes on time and the
  encoder's output is per frame (x264 `ultrafast`, `zerolatency`, no B-frames); what reaches
  `srtsink` is held back by whole frame periods (the held row). The mux holds a frame for two
  reasons, and each control shows one alone:
  - **The wait for the audio** (alignment 1 shows it alone). GstBaseTsMux gives `GstAggregator` no
    `get_next_time` (gstbasetsmux.c, GStreamer 1.24.2), so the aggregator waits until every pad
    holds a buffer, then muxes the earliest. A video frame n (counted from the build's first)
    therefore leaves only once an AAC frame stamped at or after it is encoded: frame
    k = ceil(1 600 n / 1 024), which needs 1 024 (k + 1) samples, when the camera has pushed
    1 600 (n + 1). That holds n exactly when 9 n mod 16 is 1 to 6: n = 2, 4, 6, 9, 11 and 13 of
    every 16 (lcm(1 600, 1 024) = 25 600 samples, 16 frames), 6 of 16 = 37.5 %, the alignment-1
    run's share, and the rule agrees with 5 409 of its 5 410 frames. The smoke holds the same six
    residues every time (100 %).
  - **The alignment** (the audio-off control shows it alone: one pad, so no wait for another).
    `mpegtsmux alignment=7` (media.rs, C10's 1 316-byte SRT payload): `gst_base_ts_mux_push_packets`
    returns without pushing while fewer than `alignment` packets are pending and forces the rest
    only at EOS. The ball picture's P-frames are about two TS packets (CI's wire test: 266 packets
    for 90 video-only frames, two a P-frame after the IDRs), so a frame starts in a buffer of seven
    shared with the frame before about 1 − p/7 of the time (64 to 71 % for p of 2 to 2.5 packets),
    against the 63.5 % measured. The probe logs PES starts, not packets per
    buffer, so this hold is not predicted frame by frame.
  - **Both, on the smoke.** With audio there are about 6 packets a frame period (546 in 90 frames
    at alignment 1), so a buffer of seven fills in about 1.2 periods, and most frames are held
    exactly one (1 701 of 2 425): a uniform hold bunches nothing, which is why the smoke bunches
    less (26.4 %) than either control. Its residues off the audio rule are held 65 to 81 % by the
    alignment; the combined hold has no frame-level model.
  - **Pattern-dependent.** Only `ball` at 720p was measured. Snow and noise frames carry hundreds
    of packets, so the alignment hold should vanish there and the audio wait alone remain: about
    37.5 % bunched at sent, more than ball's 26.4 % (a prediction; C15 runs snow and noise).
- **After the mux: a constant delay and no net judder.** MediaMTX 1.21.2 re-muxes per reader and
  flushes after every access unit (internal/protocols/mpegts/from_stream.go, `bw.Flush()` after
  each write; the writer is a `bufio.Writer` of the SRT payload size, internal/servers/srt/conn.go),
  so it holds no partial buffer. Its reader and the compositor's `tsdemux` each complete a video PES
  only at the next payload start (the PES is unbounded), so frames reach the input about two frames
  late: input(c) − sent(c + 2) within 20 ms of a constant 1 211 ms (the two SRT latencies, 200 and
  1 000, and that wait) for 89.0 % of the smoke's frames, not all of them. The programme fits
  sent(c + 2) for 83.8 % (smoke) and 71.0 % (audio off) against 100 % for input, so 16 to 29 % of
  the programme's choices are set after the mux, in the SRT legs and MediaMTX; in aggregate the path
  removes repeats (sampled medians: sent 625, input 573; shown 498). The mixer and the output add
  nothing: each tick shows the camera slot's latest frame, as designed. The recorder adds nothing
  (PTS steps 33/34 ms), and the reader is right (two decodes agree).
- **The alignment-1 run lost the video, and that is a harness fault, not a compositor finding.**
  Push was clean (5 411 frames, 0 bunched) and the mux sent every frame, but `input-tap` decoded 0
  frames and the compositor rebuilt its input 14 times, each generation abandoned by the domain
  after about 5 s with only audio flushed, so it never went live. The compositor never received an
  alignment-1 TS: MediaMTX re-muxes per reader (above). An alignment-1 TS from this camera decodes
  (CI's `the_wire_probe_sees_every_frame_the_camera_sends_with_and_without_audio` reads all 90
  strips from one). And MediaMTX writes no H264 to a reader until it has seen a random-access
  access unit, and logs nothing while it waits (from_stream.go, the `idrPresent` check before the
  first `WriteH264`). So MediaMTX never got a whole IDR from the camera's 188-byte SRT messages (an
  IDR is 30 to 80 of them), in its ingest or on the SRT leg; why was not read. The discriminator is
  the publisher's `packetsReceivedLoss` and `packetsReceivedDrop` in MediaMTX's
  `/v3/srt/conns/list` on an alignment-1 run. Alignment 1 is not a usable default; compose keeps 7.
- **The beats are blind, not wrong.** `droppedFrames` counts the ladder's overruns before the encoder
  and `fps` is the encoder's output rate; both are true here. Neither sees the camera's cadence: the
  camera slot is latest-frame only (`adapters::gst_input`'s appsink, max 2, drop), and the driver
  ticks at `origin + ceil(i * 1000 / 30)` ms whatever the slot holds, so a camera frame replaced
  before a tick, or a tick that finds the same frame, is counted nowhere.
- **The real path (phone → Cloudflare → compositor): judder of unknown size.** This exact mechanism
  is the fake camera's: the compositor reads Cloudflare's re-mux, not the phone's mux (F-002's
  RTMPS-in, SRT-out run shows Cloudflare re-muxes). The amplifier is the product's, though: no input
  de-jitter, a latest-frame tick, and each video PES complete only at the next one's start, so any
  arrival jitter at the input becomes repeats and skips 1:1. How much there is has not been
  measured. F-002's Cloudflare arrival gaps (p50 33.28, p90 40.44, p99 64.36, max 69.55 ms) came
  from a 30 fps x264 feed over RTMPS from a datacentre machine, not a phone over SRT, and report no
  low tail: against their mean (30.12 fps), those percentiles allow anywhere from about 0 to about
  18 % of frames bunched near 0 ms (the review's bound), and the p99 alone puts at least about 1 % of
  frames a period late, each a repeat plus a skip. Its upper tail is smoother than this harness's
  input (the smoke's input stage: p90 69.58, p99 99.11 ms). The real path adds clock drift too: the phone's clock against
  the Fly Machine's sweeps the tick's phase, which one host clock cannot show and the tool's phase
  sweep can. A measurement owes the raw arrivals kept, so `sample()` can be run on them.
- **The fix to the harness: not known yet, and not one line.** Each hold needs its own:
  - **The audio wait:** the audio must lead its video by at least 384 samples (8 ms) beyond the
    frame's own 1 600, for instance frame n + 1's samples, with their true PTS, pushed one push
    early. The rule above holds 6 of 16 frames at a lead of 0, one (n = 9) up to a lead of 383
    samples, and none from 384. Pushing a frame's audio before its own video leaves the lead at 0
    (the audio is already queued when the video arrives; AAC encodes faster than x264), so it
    removes no hold: this entry's first example fix was exactly that, and is withdrawn.
  - **The alignment:** still unaddressed. A candidate (the review's): merge each push's packets
    into one buffer before the sink, since `srtsink` 1.24.2 splits a buffer into messages of at most
    the payload size (gstsrtobject.c's send loop). It meets F-012's measurement that a buffer over
    1 316 bytes at the mux's default alignment never arrived, so it is unmeasured and doubtful.
  Then the same probe run again; a cadence gate for C14 and C15 waits for it.
- **For the owner (product, not done here):** (i) a beat counter of camera frames replaced unshown
  and ticks that repeat a frame (small: a sequence number on the camera slot, read at each tick),
  so a judder on a real input is visible in the beats at all; (ii) a PTS-paced input buffer of one
  or two frames (medium; costs 33 to 66 ms of latency), only if a measurement of the programme off a
  real Cloudflare input (the fake camera, once its mux holds nothing, into a CF Live Input, read
  by `input-tap`, `cadence_stages.py`'s input-stage sweep) shows judder worth that.

## F-030: real overlay cost, first measurement: legs A and C on performance-4x (C15 cost run 40153136892, 2026-10-25), PARTIAL

**Run:** https://github.com/onryde/seazn.club.compositor/actions/runs/40153136892. Four cost legs on
performance-4x, each a 600 s drive: A (720p, `bar` on cricket) and C (1080p, `bar` on cricket), each
once with the snow camera and once with noise. Method: `harness/soak/README.md`, "The cost legs"
(a `/proc` sample a second from +15 s, the real web page and templates, a boundary every 6 s, one
renderer kill at the midpoint). The table is `cost/table.sh`, the `cost-table` job's script, over the
four legs' `cost.json` artifacts, in F-005's shape:

| Leg | **A** 720p bar on cricket, noise, performance-4x | **A** 720p bar on cricket, snow, performance-4x | **C** 1080p bar on cricket, noise, performance-4x | **C** 1080p bar on cricket, snow, performance-4x |
|---|---|---|---|---|
| **Machine cores avg / p95 / max (of N)** | **1.348 / 1.451 / 1.694 (of 4; p95 36.275 %)** | **1.422 / 1.557 / 1.913 (of 4; p95 38.925 %)** | **1.596 / 1.752 / 2.009 (of 4; p95 43.8 %)** | **1.595 / 1.704 / 1.932 (of 4; p95 42.6 %)** |
| Process tree cores avg | 1.252 | 1.33 | 1.531 | 1.511 |
| …compositor (GStreamer host, CEF browser) | 1.197 | 1.268 | 1.469 | 1.462 |
| …CEF renderer + gpu + utility + zygote | 0.031 + 0.022 + 0.001 + 0 | 0.035 + 0.026 + 0.001 + 0 | 0.035 + 0.025 + 0.002 + 0 | 0.028 + 0.02 + 0.001 + 0 |
| …Xvfb | 0 | 0 | 0 | 0 |
| The sampler itself / the rest | 0.049 / 0.003 | 0.053 / 0.003 | 0.05 / 0.003 | 0.083 / 0.003 |
| Busiest compositor thread, p95 / max | pool-compositor 0.14 / 0.18 | queue0:src 0.14 / 0.2 | queue0:src 0.18 / 0.21 | queue0:src 0.2 / 0.21 |
| Encoder speed, median / worst beat | 1 / 1 | 1 / 0.967 | 1 / 0.967 | 1 / 0.967 |
| Output fps, median beat | 30 | 30 | 30 | 30 |
| Final droppedFrames | 0 | 0 | 0 | 0 |
| Rungs (beats) | full 126 | full 126 | full 126 | full 125 |
| Moments (by the page's feed), a minute; longest gap, s | 100, 9.983/min; 6.892 | 100, 9.983/min; 6.885 | 100, 9.983/min; 6.872 | 100, 10/min; 6.004 |
| Renderer kill, s into the drive / cef crashed, then cef ready N+1, s after it (compositor log) | 299.92 / 0.023, 5.197 (1 crash) | 300.401 / 0.027, 5.233 (1 crash) | 299.984 / 0.009, 4.221 (1 crash) | 300.345 / 0.016, 4.252 (1 crash) |
| Live renderers before / after the kill (max) | 1 / 1 | 1 / 1 | 1 / 1 | 1 / 1 |
| Stills written / asked | 8 / 8 | 8 / 8 | 8 / 8 | 8 / 8 |
| Machine memory used, max MB | 735 | 740 | 781 | 792 |
| Samples / expected | 586 / 586 | 586 / 586 | 586 / 586 | 585 / 585 |
| Owed 93: p95 + two held pages (2 × renderer) vs the size rule | 1.513 vs 2 (within) | 1.626 vs 2 (within) | 1.821 vs 2 (within) | 1.76 vs 2 (within) |
| **Size rule verdict** | **FAIL**: reload | **PASS** | **FAIL**: reload | **PASS** |

**CPU, rung, drops: within the size rule on all four.** The Machine's p95 is 1.451 to 1.752 cores of
4 (36 % to 44 %, against 50 %). Every beat is `full` and the final `droppedFrames` is 0. The busiest
compositor thread peaks at 0.21 cores, so no single thread is near a core. The Machine figure
over-reports a little: it includes the sampler itself, its ssh session, Fly's init and hallpass (the
"sampler itself / the rest" row, 0.052 to 0.086 cores together).

**The two reload reds are the harness's timing, not the product.** A:noise read `the kill came
-0.08 s from the midpoint` and C:noise `-0.016 s`. In both, the compositor logged exactly one
`cef crashed`, 0.023 s and 0.009 s after the kill, and the new page's `cef ready` 5.197 s and
4.221 s after it, as on the two passing legs (5.233 s and 4.252 s). The kill landed early because
`leg.sh`'s midpoint wait ended up to 999 ms early, by an integer division, on the runner's clock.
PR #81 moves the decision to the compositor Machine's clock (`harness/soak/README.md` finding 18).

**Snow against noise: the heavier camera differs by profile.** At 720p snow is heavier (p95 1.557
against 1.451), so leg B runs on snow; at 1080p noise is heavier (1.752 against 1.704), so leg D
runs on noise. Both gaps are about 0.1 core or less.

**Owed 93** (two held pages at the live renderer's cost) is within the size rule on all four: at
most 1.821 against 2.0, on C:noise.

**Against F-005:** F-005's chain read a p95 of 0.902 (720p) and 1.193 (1080p) on the ball camera
with the test page. The camera, the page and the process differ, so the gap (0.55 to 0.66 cores at
720p, 0.51 to 0.56 at 1080p) is not the overlay's cost alone.

**Still owed:**
- Legs B and D (`bug` on badminton, a rally every 3 s), after PR #81: B on snow, D on noise.
- The size verdict for performance-4x needs all six runs passing: A and C on both cameras, then B on
  snow and D on noise. A:noise and C:noise passed every check but the kill's timing, so they want a
  re-run after PR #81. If B or D measures above A or C, a football leg is owed (the README's rule).
- The stills: 8 of 8 written per leg, not yet looked at for this entry.

**Teardown:** done. Each leg's "Destroy every Machine" step succeeded, and `flyctl machine list -a
seazn-relay-soak` was empty on 2026-10-25. The `cost-table` job was green.

---

## F-031: each input rebuild left 1.2 to 2.4 MB in glibc's thread arenas; reusing the input's threads, one set per streaming role, takes it away (CI runs 40176756299 to 40194261349, 2026-10-25)

**Question.** Soak arm 1 (run 40166664556; schedule `dropouts`, page `test`, 75 min) grew the
compositor's anon memory by +118 MB in shrinking steps, against +18.7 MB with no dropouts (arm 3,
run 40172443534) and +23.7 MB with dropouts and `MALLOC_ARENA_MAX=2` (arm 2, run 40172425373): about
2.8 MB a dropout with glibc's default, 0.14 MB with the arenas capped. CEF did not grow. A dropout is
one input rebuild (`gst_input.rs` `abandon`, then `rebuild`). Leak or retention, and where?

**Method.** A diagnostic on throwaway branches (`loop/input-heap-diag-1` to `-3`, deleted); its gate
form is `crates/adapters/tests/input_heap.rs`. A listener child sends fake-camera's picture (1280×720
ball, 30 fps, AAC, `mpegtsmux alignment=7`) over SRT; a measuring child holds the adapter alone
(`GstInput`, P720, `Thresholds::default()`, latency 1 000 ms). Each cycle is one dropout: a rebuild
held past the handshake (Owed 97), `InputConnected`, N s of frames (about 30 a second), `abandon`,
the build before it torn down, then one sample: `RssAnon` and `Threads` (`/proc/self/status`), and
`/proc/self/smaps`' anon pages split into the main `[heap]`, glibc's thread-arena heaps (64
MiB-aligned mappings, `HEAP_MAX_SIZE`) and the rest. ubuntu-24.04, 4 vCPU, glibc 2.39, GStreamer
1.24.2.

**The diagnosis** (60 cycles of 2 s decode unless noted; least-squares slope over cycles 11 to 60):

| Variant | RssAnon KB a rebuild | Anon at the end (MB) | CPU ms a cycle | Shape |
|---|---|---|---|---|
| default (rounds 1, 2, 3) | **1 503 / 1 187 / 1 193** | 144 / 140 / 159 | — / 178 / 155 | a staircase, all in thread arenas (17, flat); `[heap]` and the rest flat; threads flat |
| default, 10 s decode (30 cycles) | 2 190 | 125 | — | the same |
| `MALLOC_ARENA_MAX=1` / `=2` | 20 / 23 and −17 | 35 / 42 and 40 | — / 190 | flat |
| `MALLOC_ARENA_MAX=2`, 10 s decode (30 cycles) | −9 | 41 | 541 | flat |
| `MALLOC_ARENA_MAX=4` / `=8` | 180 / 349 | 58 / 89 | 185 / 191 | |
| `MALLOC_MMAP_THRESHOLD_=131072` | 113 / 87 | 40 | — / 209 | a slow creep |
| a listener that handshakes and sends nothing | 8 | 30, flat | — | |
| nothing listening (a refused connect, 30 cycles) | 3 | 18 | — | |
| one long-lived build thread | 195 | 57 | 104 | flat 44 to 47 to cycle 52, then +10 MB |
| `avdec_h264 max-threads=1` | 80 | about 65 | 130 | oscillating |
| a task pool of reused threads | 180 | 83 | 176 | a plateau of 77 to 83 from cycle 25 |
| the same, 10 s decode (30 cycles) | 29 | about 75 | 441 | a plateau of 69 to 75 (the default: 2 190) |
| task pool + one build thread | 97 | 48 | 124 | a 40/46 oscillation to cycle 51, then 47 to 48 |
| task pool + one build thread + `max-threads=1` | 29 | 46 | 124 | a 39/45 oscillation |
| `GST_TRACERS=leaks`, `log-live-objects` at cycles 10 and 40 | 752 | — | — | 164 live objects at both, every type +0 |

**The gate and its mutants** (the gate's own form: 8 rebuilds to warm up, 1 s decode, the median of
the pairwise slopes of `RssAnon` over the measured rebuilds, in KiB a rebuild; rounds 1 and 2
measured 24 rebuilds, rounds 3 to 5 measured 32):

| Build | Runs (KiB a rebuild) | Anon after 40 rebuilds (MiB) |
|---|---|---|
| **the fix** (reused build threads; a task pool and a set of threads per role) | round 4 (run 40191976912), 7 runs: 7.4, 8.6, 26.8, 29.4, 45.5, 78.9, 118.5; round 5 (run 40194261349), 5 runs: 6.4, 8.8, 10.2, 11.3, 35.1 | 46 to 52, level from about rebuild 10 |
| one shared task pool for every role (the fix as first built) | rounds 1 to 4 (runs 40182839995, 40185312932, 40189274326, 40191976912), 24 runs: 2.0 to 619.9, median 293 | 65 to 83 (rounds 3 and 4), still climbing |
| **both halves removed** (a build thread per build, GStreamer's default pool) | 6 runs to round 4: 1 592, 1 765, 1 952, 2 028, 2 060, 2 447; round 5, 1 run: 2 284 | 126 to 149 |
| the fix + `MALLOC_ARENA_MAX=2` | 1 run: 78.6 | 39.5 |
| the fix + `avdec_h264 max-threads=1` | 2 runs: 34.1, 59.2 | 50 to 52 |
| one shared pool + `max-threads=1` | 2 runs: 125, 354 | — |
| one shared pool, the build threads per build | 1 run: 10 | — |
| reused build threads only (GStreamer's default pool) | 1 run: 925 | — |
| no reuse in `Workers` (a new thread per job) | 1 run: 5 398 (threads 46 → 138) | — |

The fix's series alternates between two levels about 6 MiB apart (44 and 50, say): the gate's
rebuild tears the last build down while it starts the next, so each role runs on two threads in
turn, and each of their two arenas holds a frame pool. The gate bounds the median slope at 512 KiB
(`tests/input_heap.rs`, `BOUND_KIB`): the power of two nearest the geometric middle of the fix's
highest reading and the cause's lowest (434 KiB), so each clears it by about the same factor (4.3
and 3.1).

Round 5 ran each guard's mutant on the final code against the test that must kill it, and each
was red on its message: both halves removed (the gate: "the heap grows", 2 284 KiB); one pool for
every role (the reuse test: "each build's source task ran on the source threads"); the pools made
without their roles (the same); `vqueue` renamed in the role table ("each build's video task ran
on the video threads"); a build thread per build ("the three builds ran on the adapter's
threads"); no task pool ("every build's streaming tasks ran on the adapter's pool"); and no reuse
in `Workers` ("no build thread was started"). The input suites on the final code: the lib's 4, the
preload's 1, the heap's 2 and the input's 40, all passed.

**Findings.**
1. **Not a leak.** With one or two arenas the same 50 rebuilds hold flat (−17 to 23 KB a rebuild),
   so every byte the default run grew was freed and could be reused; and GStreamer's live objects
   are the same 164, type by type, 30 rebuilds apart.
2. **It needs decoded video.** A build that connects and receives nothing costs 8 KB; a refused
   connect 3 KB. Longer decoding costs more (2.2 MB a rebuild at 10 s, 1.2 to 1.5 at 2 s).
3. **Mechanism (inferred, consistent with every row).** A 720p I420 frame is 1 382 400 B, above
   glibc's first mmap threshold (128 KiB); glibc raises the threshold to the size of the first mmapped
   block freed, so later frames come from the arena of the thread that allocates them. Each build
   allocated its frames on threads the last build had not used: a build thread of its own, and the
   streaming tasks of `srtsrc` and the two queues on GStreamer's default pool. A thread keeps the
   arena it first lands on, and an arena keeps its high-water mark, so one build's freed frames stay
   resident in one arena while the next build fills another. A fixed mmap threshold removes 93 % of
   it, and capping the arenas removes all of it.
4. **Reusing the threads removes it in code.** The streaming tasks carry most of it (the task pool
   alone reads 10 KiB a rebuild in the gate's form, the build threads alone 925). The fix keeps
   both.
5. **One pool for every task was not enough.** With one shared set of task threads, the source,
   video and audio tasks took whichever thread was idle, so the video task (the parser, the decoder
   and the converter run on it) moved between threads from build to build, and every arena it
   visited kept a frame pool: 2 to 620 KiB a rebuild in 24 runs, in steps of 6 to 10 MiB, and 65 to
   83 MiB after 40 rebuilds, still climbing. One pool and one set of threads per role (by the name
   of the element that owns the task: `src`, `vqueue`, `aqueue`, any other) keeps each kind of
   buffer on the same two threads: 6 to 119 KiB in 12 runs, 46 to 52 MiB after 40 rebuilds, and
   level. The arena cap on top of that reads 39.5 MiB, the floor for this harness.
6. **The decoder's slice threads are not what is left** (`max-threads=1` with the fix: 34 and 59
   KiB; 50 to 52 MiB, inside the fix's own spread). They are new with each build, but taking them
   away moves nothing measurable here. Not traced: libsrt's own threads, which are new with each
   connection.
7. **The arena cap is the other remedy, and costs nothing measurable in CPU here** (178 to 191 ms a
   cycle across 1, 2, 4 and 8 arenas). It changes the allocator of the whole process tree, since the
   compositor's environment reaches the CEF children. It is not used: the fix removes the cause, and
   the cap stays the measured fallback.
8. **Two tools failed here.** heaptrack 1.5 aborted the Rust test binary 0.1 s in ("fatal runtime
   error: stack overflow") under its preload. The leaks tracer's `get-live-objects` action in 1.24.2
   hands its list over without a ref on each object: the objects were disposed while PLAYING, and the
   process died on SIGSEGV. The `log-live-objects` action, with `GST_DEBUG=GST_TRACER:7`, works.

**Not measured here.** A dropout soak on the fix; the production Machine's core count; the output's
swaps (about 3 MB each in soak run 40139563554's diagnosis).

---
