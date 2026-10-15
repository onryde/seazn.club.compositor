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

**Owed (owner session, phone):**
- the camera→compositor delay median (n=10, clock in shot), which sets the ADR 0006 default;
- the time to first frame after an iOS background/return.

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

## F-004: RTMPS to Input B → YouTube. OWED: owner session (partial, owner-observed)

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

**Cause:**
- **Hypothesis SUPPORTED by owner observation through LIVE 2:20 of the snow run, not pinned.** The hypothesis: YouTube judges health on *real* video bits, so filler-padded CBR on low-entropy content reads as "not receiving enough video".
- Across bitrates 2.5/4/6 Mbps and latency Low/Normal, every filler-heavy run (56–78 % filler) went Poor about 1 min in. Only the 0 %-filler run stayed Excellent.
- **Open confound:** the snow run also had an IDR every 0.2 s against 2.0 s in all the others.
- **Deciding experiment owed (owner session):** real phone camera footage, with realistic motion, 2 s keyframes and scenecut as C09 will ship, while logging filler share. No further B pushes were spent disambiguating, per the coordinator.

**Changes (proposals; C09 is not edited):**
- **C09 slate and frozen frame.** A static slate encodes to almost pure filler, which is exactly the case YouTube flags Poor. Proposal: composite a full-frame, low-amplitude, animated noise/grain layer, or a continuously moving element, under every slate and frozen-frame output, so real bits approach the target. The exact amplitude is unmeasured.
- **C09 acceptance test:** measure the **non-filler** bitrate (bytes outside NAL type 12) on a slate-only run. The current test, "bitrate within ±50 % (CBR filler keeps it near the target)", passes on the wire while YouTube flags Poor.
- **R2 restart policy (ADR 0004):** a compositor restart, or any output stop, ends the organiser's YouTube broadcast. Whether YouTube resumes the *same* broadcast after a short reconnect is unmeasured.

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
