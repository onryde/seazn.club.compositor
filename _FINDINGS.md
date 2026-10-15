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

