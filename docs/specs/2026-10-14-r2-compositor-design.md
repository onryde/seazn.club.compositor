# Relay R2 — Compositor: design

- **Date:** 2026-10-14
- **Status:** design approved by owner in session "r2" (2026-10-14); awaiting written-spec review
- **Programme:** Seazn streaming ("Relay"). R1 (control plane) is merged in `seazn.club`; R3 is the
  native capture apps (`seazn.club.capture`); R4 is the remainder of the compositor programme (§12).
- **Source documents:**
  - Owner's production spec: *Live Sports Streaming Compositor — Production Design & Specification*
    (2026-10-03, §4 amended 2026-10-05). Referred to below as **the PDF**. Option A is adopted.
  - Owner's proof of concept `compositor-rs-poc` (Rust, single pipeline, native cairo templates).
    Ideas are reused; the code is not carried over wholesale (§3.4).
  - R1 control plane: `seazn.club/apps/web/src/server/relay/` and the internal relay routes.
- **Decisions:** ADRs 0001–0006 in `docs/decisions/`.

---

## 1. Intent

Turn a single phone camera into a scored broadcast on YouTube (and our own player) with **no OBS and
no operator laptop**. The organiser picks *composed* mode for a fixture, and the camera feed comes out
the other side with Seazn's live scoreboard on it.

Success, in the PDF's ranked order (a higher rank wins a conflict):

1. **The output never stops.** Constant-rate frames for the whole event. When the camera drops, a
   slate appears within 3 s with the score still on screen. No single process failure ends the
   broadcast.
2. **Bounded resources.** Every buffer has a limit, and memory stays flat over a 3 h soak (~~±10 %~~
   ±40 %, and its growth projected to the 5 h session cap stays within half the production guest's memory:
   owner rulings 2026-10-25, `docs/_INDEX.md`).
3. **Latency.** The compositor adds ≤500 ms. A score change appears on screen within one frame of its
   scheduled time.
4. **Sync.** A/V within ±40 ms. A score is applied at event time plus the calibrated delay.
5. **Operability.** One compositor per match, created and destroyed by the backend. Its status is
   visible, and a fault alerts within 30 s.
6. **Extensibility.** A new design is a change to the overlay page only. The compositor never changes
   for a theme.

**Out of scope for R2:** multi-camera, replays, commentary mixing, viewer interactivity, 4K. Also out:
everything listed for R4 (§12).

## 2. Rulings this design rests on

| # | Ruling (owner, 2026-10-14) | ADR |
|---|---|---|
| 1 | PDF Option A: Rust + GStreamer, **two pipelines**, graphics by `cefsrc` rendering the existing `/overlay/fixtures/[id]` page. This **supersedes** the Sept B2 design (ffmpeg x11grab + headed Chromium + `#ff00ff` colorkey + TS supervisor). The PDF lists Node+ffmpeg+Chromium as a rejected alternative. | 0001 |
| 2 | The compositor lives in its own repo, `onryde/seazn.club.compositor`. **All** R2 specs, plans, decisions and task briefs live in this repo's `docs/`. | 0002 |
| 3 | R2 = PDF phases 0 + 1 plus the R1 wiring, opening with a spike gate. PDF phases 2–4 become **R4** (R3 is already taken by the capture apps). | 0003 |
| 4 | Supervisor is in-process Rust (no Node in the image). Fly restart policy is `on-failure` with `max_retries: 2`, and `auto_destroy` is kept. | 0004 |
| 5 | Output is 720p30 by default, with 1080p30 behind a config flag. CEF always renders the page at 1920×1080. | 0005 |
| 6 | Score delay defaults to the spike-measured camera→compositor delay (not 0), with a per-fixture override. The spike also tests whether an SEI wall-clock stamp survives Cloudflare. | 0006 |

Sept rulings that still stand: `lhr` region with no picker; `performance-4x / 8 GB` guest; multi-cam is
N inputs under one session, so the compositor reads **slot 0** explicitly; broadcasts default to
public; a migration version is never pinned in a doc; the S1–S10 test rules.

## 3. Architecture

### 3.1 Topology

```
phone (seazn-capture) ──RTMPS/SRT──▶ CF Live Input A (camera)
                                          │ SRT playback (one reader)
                                          ▼
┌──────────── Fly Machine  relay-<sid>-r<n>  (perf-4x/8GB, lhr) ────────────┐
│ compositor (one Rust binary)                                              │
│  ├─ input pipeline     srtsrc→demux→decode→appsink   (torn down/rebuilt)  │
│  ├─ programme pipeline appsrc(clocked)→compositor[slate|camera|cef]       │
│  │                     →x264 CBR→flvmux→rtmp2sink    (never rebuilt)      │
│  ├─ graphics           cefsrc → overlayUrl (1920×1080, transparent)       │
│  └─ supervisor         pure state machine; drives the three above         │
└──────┬─────────────────────────────────────────▲──────────────────────────┘
       │ RTMPS (leg 3)                           │ outbound HTTPS only
       ▼                                         │ GET session, POST heartbeat
CF Live Input B (programme) ──CF Outputs──▶ YouTube / own player   seazn.club (R1 control plane)
```

### 3.2 Design rules (PDF, binding)

1. **Two pipelines, not one.** The programme pipeline runs on its own clock and always has a frame
   to encode: live video, the frozen last frame, or the slate. The input pipeline can stall,
   reconnect or be rebuilt without touching the output. The POC's single pipeline let any error,
   such as a YouTube drop, tear down everything for 2 s. This is the fix.
2. **Per-match isolation.** One Machine, one process tree and one set of credentials per session.
3. **Outbound-only.** No public port. Configuration and commands are pulled, health is pushed.
4. **Separate credentials per leg.** The camera uses A's ingest. The compositor uses A's *playback*
   and B's *ingest*. Exactly one reader per SRT playback stream.
5. **Protocol per leg.** Leg 1 (camera): SRT where the app supports it, RTMPS otherwise. Leg 2
   (pull): SRT playback. Leg 3 (programme): RTMPS. SRT push from Fly is not used; memory trap C14
   says it never starts a broadcast.
6. **Timestamps travel end to end.** Audio and video keep source timestamps through to the encoder,
   with a single offset established per input (re)connect.
7. **Everything bounded.** Every queue has a byte and time limit with a drop policy. Every external
   call has a timeout and a retry budget.

### 3.3 Frame scheduling

On every programme tick (1/fps), the domain picks a `FrameChoice`:

- `Emit`: the newest decoded camera frame
- `Repeat`: the last camera frame, for as long as the programme is `Frozen` (from 0.5 s with no
  frame until the 3 s slate threshold). The PDF's scheduler paragraph says "up to 1 s", but its
  state machine and timing budget put the slate at 3 s. The state machine wins (owner ruling: the
  PDF tables are adopted as written).
- `Slate(kind)`: waiting or reconnecting

The CEF layer is composited on top of whichever one is picked, so the score bug is on screen in every
state.

**How the isolation is realised (plan refinement).** No failing element may take down an element that
must not fail. So the "programme pipeline" of §3.1 runs as four GStreamer pipelines inside one process:

- **input**: `srtsrc → decode → appsink`. Rebuilt on loss.
- **graphics**: `cefsrc → appsink`. Restarted on a CEF crash, with the last overlay frame held.
- **programme**: `appsrc(camera|slate) + appsrc(overlay) + appsrc(audio) → compositor → x264 → appsink`.
  Never rebuilt.
- **output**: `appsrc(encoded) → flvmux → rtmp2sink`. Rebuilt on a sink failure, fed from a bounded
  2 s encoded ring buffer, and resumed at a forced keyframe.

The per-tick `FrameChoice` replaces the `SetCameraLayer` / `ShowSlate` effects named in §4.3. Audio follows the same choice (live audio, or silence generated on the programme clock).

### 3.4 What is reused from the POC

| POC piece | R2 fate |
|---|---|
| `fallbacksrc` + single `parse::launch` pipeline (`pipeline.rs`) | **Replaced** by the two-pipeline design (§3.2.1). |
| `cairooverlay` + JSON templates (`render.rs`, `scene.rs`, `templates/`) | **Dropped.** Graphics come from the overlay page via `cefsrc` (ADR 0001). Moments and slots move into the page in R4. |
| Score polling (`score.rs`, `feeds.rs`) | **Dropped** from the compositor. The page polls the score itself (PDF §4). |
| Broadcast API / proof-of-play (`feeds.rs`, mock backend) | **Deferred to R4.** |
| Health endpoint (`health.rs`) | **Replaced** by the pushed heartbeat (§5.3). There is no inbound port. |
| x264 CBR settings, `redact()`, env config shape | **Reused as ideas** inside the new adapters and `config.rs`. |

## 4. Domain model

The pattern is hexagonal (ports and adapters) with in-repo fakes, the same as `seazn.club`'s
`server/relay/`. There is one pure aggregate. It is the only place where state changes.

### 4.1 Workspace

```
crates/
  domain/     pure: no I/O, no clock, no env, no gstreamer / cef / http dependency
  app/        application layer: event loop; decide → apply effects through ports
  adapters/   gst-input, gst-programme, cef-graphics, http-control, sys-clock, metrics
  testkit/    a fake for every port + a scripted scenario builder
  compositor/ bin: wiring only; drivers chosen once per process (COMPOSITOR_DRIVERS=fake|live)
```

`COMPOSITOR_DRIVERS` unset means `fake` in dev and test, and a hard refusal to start in a release
build. This mirrors `RELAY_DRIVERS`.

### 4.2 Aggregate and sub-machines

`Supervisor::decide(&self, event: Event, now: Instant) -> Decision { next, effects, health }`

Each sub-machine is table-driven (a transition table, not ad-hoc `match` chains), so that the table
can be enumerated in tests.

| Sub-machine | States | Key transitions (defaults from `config.rs`) |
|---|---|---|
| `Programme` | `Waiting → Live → Frozen → Reconnecting` | Waiting→Live after 1 s of continuous frames. Live→Frozen after 0.5 s with no frame. Frozen→Reconnecting after 3 s. Back to Live after 1 s continuous (hysteresis). |
| `InputLink` | `Connecting → Running → Backoff(n)` | Backoff 1 s → 10 s cap, retried forever. A reader kick or EOS counts as one kick. More than 3 kicks in 5 min raises an alert flag. |
| `OutputLink` | `Connected → Reconnecting → Stalled` | A sink error leads to `SwapSink`; a destination that stops taking bytes fails the sink's own socket wait after 5 s (rtmp2sink `timeout`, set explicitly). A reconnect is expected to complete within 5 s. **Amended 2026-10-24** (owner ruling "A", `docs/_INDEX.md` Owed 87): no progress or ack rule; a sink that sends nothing and never errors is not swapped (Owed 94). **Amended 2026-10-25** (owner ruling "B", `docs/_INDEX.md` Owed 94): one backstop, not a stall rule. A sink whose egress stays flat for `output_egress_flat_ms` while both encoded tracks keep growing is swapped (`SwapReason::EgressStalled`, the log word `egress_stalled`), and the beat reads `stalled` until the new sink earns Connected. The threshold is one derived constant (`config.rs` `output_egress_flat_ms`: the 15 s outage target less one connect's worst case, the back-off, one progress poll and two widest ticks, floored to the poll: 9 000 ms; owner, 2026-10-25, in chat, relayed by the coordinator, "Apply rec": 9 s, keeps the worst outage under 15 s). It sits above the sink timeout plus the back-off, so a socket wait that outlasts the timeout still reaches the sink's own error first; a multi-wake trickle, whose every wait is under the timeout, never errors, and there the backstop is the only detector (`docs/_INDEX.md` false premises, 2026-10-25, P-3). A starved encoder restarts its clock. |
| `Graphics` | `Loading → Ready → Recycling \| Failed` | A CEF crash or hang leads to restart with backoff, holding the last frame. Three failures in 5 min lead to `Failed`: the overlay is reported as failed, and video continues **without waiting on the browser**. Scheduled recycle every 2 h. |
| `Lifecycle` | `Booting → Ready → Streaming → Draining → Exited(code)` | Driven by session bootstrap, heartbeat `desiredState`, `RELAY_DEADLINE_AT` and signals. |

### 4.3 Events, effects, value objects

- **Events in** (from ports): `Tick`, `InputFrame`, `InputStalled`, `InputEos`, `OutputError`,
  ~~`OutputAckStalled`~~ `OutputProgress`, `EncoderOverrun`, `CefReady`, `CefCrashed`,
  `HeartbeatReply(desired)`, `Deadline`, `Signal(kind)`. **Amended 2026-10-24** (a pre-existing
  error, corrected beside the Owed 87 amendment above; the PR #73 review's I-1): `OutputAckStalled`
  was never built; F-005 made it `OutputProgress { gen, total_bytes }`, the sink's bytes-sent total,
  which proves Connected (Owed 38) and feeds the bitrate but detects no stall.
- **Effects out:** `RebuildInput`, `SwapSink`, `SetCameraLayer(on)`, `ShowSlate(kind)`, `RecycleCef`,
  `StepDown(rung)`, `SendHeartbeat(report)`, `Exit(code)`.
- **Value objects:**
  - `FrameChoice`
  - `Backoff`
  - `DegradeLadder`: overlay fps 30→15 first, then x264 preset `veryfast`→`superfast`; never stall
    the clock. **Amended 2026-10-18** (owner ruling, `docs/_INDEX.md` Owed 20): there is no preset
    rung. F-001 found it cannot change live, and its fallback was to drop the overlay, so the ladder is
    overlay fps 30→15 only, and the bottom holds. A step down is a sizing alarm (C17 runbook)
  - `ExitReason` → exit code
  - `Thresholds`
  - `HealthReport`: every optional field is an `Option`, so an absent value is never sent as zero,
    matching the heartbeat schema

### 4.4 Ports (traits)

`InputSource`, `ProgrammeOutput`, `GraphicsRenderer`, `ControlPlane`, `Clock`, `Metrics`.
No GStreamer, CEF or HTTP type crosses a port. Each port's fake lands **before** its live adapter.

### 4.5 Enforced, not documented

- A test asserts the `domain` crate's dependency list contains no `gstreamer*`, `cef*`, `ureq` or
  `tokio` entry.
- A purity test fails on `SystemTime::now`, `Instant::now`, `std::env`, `std::fs`, `std::net` or
  `std::thread` under `crates/domain/src`.
- A port-boundary test fails if `app` imports from `adapters`.
- `config.rs` is the one authority for every number. Each value carries a doc line naming its source
  (PDF §, spike F-id, or the R1 contract).

## 5. Control-plane contract

### 5.1 Contract file

`seazn.club/docs/contracts/relay-job.v1.json`, with fixtures, is authored in the web repo **first**.
This repo vendors it by hand into `contracts/`, with `VENDORED.json` recording the web commit and a
sha256 per file, and a pin test checks it. Neither repo's CI fetches the other (the capture
convention).

### 5.2 Machine environment (from R1, unchanged)

`SESSION_ID`, `JOB_TOKEN` (HS256, `{sid, scope:"relay-job"}`), `APP_URL`, `RELAY_DEADLINE_AT` (ISO).
The compositor reads no other env and no platform-specific variable (ADR 0007: any OCI host that meets
the host contract runs it unchanged). There is no inbound port.

### 5.3 Endpoints

- **`GET /api/internal/relay/sessions/[sid]`**: the bootstrap. Secrets arrive here over HTTPS, never
  in Machine metadata. For composed mode it adds to today's shape:
  - `input`: CF Input A **slot 0** SRT playback (URL, stream id, passphrase)
  - `target`: CF **Input B** RTMPS ingest (no longer the destination)
  - `output`: `{width, height, fps, videoKbps}` (720p30 by default; 1080p30 when flagged)
  - `heartbeatIntervalMs`: 5000
  - `overlayUrl`: built server-side,
    `${APP_URL}/overlay/fixtures/<id>?st=<pageToken>&delay=<ms>&host=compositor`
  - `overlayDelayMs`: the per-fixture override, or the measured default (ADR 0006)

  Every refusal is a bare 401. A terminal session returns 410 `SESSION_ENDED`. An unknown session
  returns 404.
- **`POST …/heartbeat`** every `heartbeatIntervalMs`. The existing strict body keeps `state`
  (`starting|playing|stalled|stopped`; `playing` = output connected, slate or picture, per owner ruling 2026-10-14; the camera's state is in `programmePhase`/`input`), `fps`, `bitrateKbps`,
  `egressBytes`, `measuredLatencyMs`, `droppedFrames`, `encoderSpeed`, `cpuPct` and `memMb`. It adds
  these optional fields: `programmePhase`, `input`, `output`, `graphics`, `inputKicks`,
  `pipelineRebuilds` and `scoreStale`. Anything not measured is omitted, never zero. The reply is
  `{desiredState: "live"|"ending"}`.

### 5.4 Exit contract

- On `ending`, `RELAY_DEADLINE_AT` or SIGTERM: flush, send output EOS, exit 0 within 10 s. The Machine
  stops and `auto_destroy` removes it.
- On crash: exit non-zero. Fly `on-failure` restarts it in place, at most twice.
- On the third crash Fly gives up. Heartbeats stop, web marks the session `lost` after 3 missed beats
  and starts `r<n+1>`.

## 6. Robustness

The PDF's failure table, timing budgets and resource bounds are adopted as written, except for the
adjustments below.

| Failure | R2 behaviour | Recovery target |
|---|---|---|
| Camera loss | Frozen after 0.5 s, slate after 3 s, reconnect forever. iOS backgrounding makes this **normal operation**: it gets soaked, not just edge-tested. | Live 1.2 s after frames return |
| CF closes SRT playback (second reader, maintenance) | Treated as camera loss. The peer's close reason is logged. More than 3 in 5 min raises an alert flag. | as above |
| Output disconnect / stall | Sink swapped behind a 2 s leaky queue. Input and graphics untouched. | ≤5 s, ≤2 s of programme lost |
| Encoder overrun | Drop frames before the encoder (`droppedFrames`), then step down the `DegradeLadder` (one rung, overlay at half rate; amended 2026-10-18, Owed 20). The score is never removed. In production a step down is a sizing alarm: raise the Machine size. | Real time within 10 s |
| CEF crash / hang | Hold the last overlay frame, restart with backoff. After 3 in 5 min, `graphics: failed`, and the camera goes out without the overlay. A hang is detected by the page's 2 s `seazn:alive` beat stopping for 10 s. A hang **hides the overlay at once** (no stale score), then restarts it. The overlay returns at the next page's ready (amended 2026-10-18, Owed 18; C05b; owner-approved design (2026-10-18); "at once" is the coordinator's reading of it). A crash below the count still holds the last frame, for at most 10 s, then hides until a reload is ready (amended 2026-10-18, C05b PR #22 review; coordinator decision derived from the owner-approved 10 s rule). Generalised the same day (the review's second round, coordinator decision): no overlay frame stays on air more than 10 s after the last ready or beat of the page that drew it, whether that page hung, crashed or was recycled and its reload has not come ready; the hide names which. | n/a |
| Score feed stale | The page keeps the last state and signals `scoreStale` after 10 s. The compositor relays it in the heartbeat. | First good poll |
| Process crash | Fly `on-failure` restarts it (≤2). | Output back ≤15 s |
| Machine loss | 3 missed beats (15 s), web marks `lost`, cold-start of `r<n+1>`. There is **no warm pool in R2**. | **~60 s** (the PDF's 30 s needs R4's warm pool) |
| Clock skew | NTP on the host. The page clamps events more than 2 s past now + delay. | Immediate |
| Memory growth | Alert at the 1 GB soft limit. The OOM order kills CEF before the encoder at the 2 GB hard limit. | n/a |

**Bounds at 720p30:** decoded video queue 1 s, encoded output queue 2 s, audio 400 ms. All are leaky
and drop the oldest. CEF is capped at about 600 MB. CPU ≤70 % sustained across 4 vCPU.

**Budgets:** input frame→encoder ≤150 ms; slate after camera loss ≤3.1 s; return to live ≤1.2 s;
output reconnect ≤5 s; score event on screen by the next frame after its scheduled time.

Where the spike measures a different number, the measured number wins. The change is recorded in
`config.rs` (with its F-id) and in `docs/_INDEX.md`.

## 7. Score delay (ADR 0006)

Through Cloudflare, video reaches the compositor about 2–4 s after the event, while score events
arrive in real time. With a delay of 0, the bug would show the point before viewers see it.

- **Default:** the camera→compositor delay measured in the spike with a clock in shot. It lives in
  `seazn.club/apps/web/src/server/relay/config.ts` (the web repo's single authority for relay
  numbers), citing its F-id. The session GET serves it as `overlayDelayMs` when the fixture has no
  override.
- **Override:** a new **nullable** `fixture_stream_settings.overlay_delay_ms`, edited from the organiser
  UI (§8 item 7). `null` means "use the default"; `0` is a real override, never read as "unset". At
  session create, the effective value (override, or else the default) is copied into the existing
  `fixture_stream_sessions.overlay_delay_ms`. That column exists today but is never written, so every
  session currently reads 0 (a false premise, corrected 2026-10-14).
- The capture app's `deliveredLagS` cannot be used for calibration. It is **relative** lag growth
  (`lagMs − minLag` on the HLS manifest, `DeliveryWatch.kt`), not absolute delay, and it is measured
  on a different path.
- **SEI probe (spike T00.3):** if an H.264 SEI wall-clock stamp survives CF Input A → SRT playback,
  an R3 capture change can stamp NTP time into keyframes. The compositor could then measure the delay
  per frame automatically, which would remove R4's calibration UX. If CF strips it, this section
  stands as the permanent design.

## 8. Main-repo changes (`seazn.club` PRs)

Briefs live in this repo's `docs/tasks/`. Code and PRs land in `seazn.club`, in this order:

1. **Hazard fix (first, standalone).** Today `mode: "composed"` reaches `runner-fly.ts`, which throws
   because `RELAY_IMAGE` is unset. The API must refuse composed mode with a typed refusal while the
   image is unconfigured.
2. **Contract.** `docs/contracts/relay-job.v1.json` plus fixtures. The session GET and heartbeat zod
   schemas are extended to match §5.3.
3. **Runner.** Restart `on-failure` with `max_retries: 2`, and the guest taken from config. The sweep
   must count Fly in-place restarts as the **same attempt**, and create `r<n+1>` only after the
   Machine is lost or destroyed. Without that, a crash double-spawns and double-bills.
4. **Composed provisioning.** Create CF Input B per composed session. Attach the destination Outputs
   to B instead of A. Tear B down with the session. Passthrough mode is unchanged.
5. **Overlay page.** Accept `st` (scope `relay-page`) in place of the overlay key. Render a transparent
   body when `host=compositor`. Emit a readiness signal the CEF adapter can observe (the mechanism is
   pinned in spike T00.1). Surface `scoreStale` the same way.
6. **Sweep thresholds** for 5 s beats: `lost` after 3 missed beats (15 s). This replaces today's
   `STALE_HEARTBEAT_SECONDS = 90` for composed sessions. A `degraded` state after 2 missed beats is
   **not** built in R2: nothing in R2 would read it, so it would be an inert seam. It arrives with
   R4's console on-air view.
7. **Organiser UI.** A composed-mode option in `components/v2/fixture-stream-panel.tsx`, behind the
   existing relay entitlement, plus the per-fixture delay override. This carries the full width bar
   (320 / 768 / 1280, seven-width e2e) and new strings in all 4 locales.

**Owner actions** (not code): Fly apps `seazn-relay-stg` and `seazn-relay-prod`, a `FLY_API_TOKEN`
per app, and the Cloudflare secrets. Then set `RELAY_IMAGE` per env.

**Docs in `seazn.club`:** a pointer row in
`docs/superpowers/specs/2026-09-05-stream-overlay-prompts/_INDEX.md` ("R2 moved to
`seazn.club.compositor/docs/_INDEX.md`; B2/B3 superseded 2026-10-14"), and a superseded header on
`R2-compositor.md`.

## 9. Testing and gates

### 9.1 T00 spike gate (2–3 days, code throwaway, findings kept)

The plan **halts and returns to the owner** if item 1 or 2 fails.

1. `gstcefsrc` builds into the image and, on a Fly perf-4x, renders `/overlay/fixtures/<id>`
   transparent at 30 fps using ≤1 core. The readiness mechanism is pinned.
2. CF Input A SRT playback: is it single-reader (does a second reader kick the first)? Are there
   bursts? Measure camera→compositor delay with a clock in shot (this becomes the ADR 0006 default).
3. Does an H.264 SEI wall-clock stamp survive CF?
4. RTMPS from Fly → Input B → CF Output actually starts a YouTube broadcast.
5. 1080p30 CPU on perf-4x (one measurement), and the image cold-start time.

Findings are recorded as F-ids in `_FINDINGS.md`, and `config.rs` numbers cite them.

### 9.2 Unit and property tests (domain, app)

- Pure, sub-second, table-driven. Every sub-machine's transition table is enumerated.
- **Expected values come from the PDF thresholds via `config.rs` declarations, never from the code
  under test.**
- **Sequence cases** (house rule, "the four questions"): a second kick, a flap every 5 s for 2 min,
  `ending` mid-reconnect, a crash after `ending`, an empty event stream, and a restart after a void.
- A `proptest` property: for any event sequence, every `Tick` yields exactly one `FrameChoice`, and
  no `Programme` transition happens faster than the hysteresis allows. The property **reports how
  many sequences it checked, and zero checked is a failure.**
- Every guard is mutated once, and a test must go red.

### 9.3 Harness (in repo, Docker; runs locally and in CI)

- **Fake camera:** GStreamer source with a frame counter and millisecond clock burned in as a
  machine-readable strip, plus an AAC tone, pushed over SRT.
- **Fake CF:** MediaMTX with a control API to kick a reader, close, deliver in bursts. `tc netem`
  applies 100–800 ms latency, 1–5 % loss and jitter.
- **Fake destination:** an RTMP receiver that records, and refuses, stalls or disconnects on command.
- **Fake control plane:** session GET plus a heartbeat recorder that serves the vendored contract
  fixtures, and a test overlay page drawing a known score timeline.
- **Reference checker:** decodes the recording and asserts that the frame counter never stalls or
  repeats beyond policy, audio is continuous, and the drawn score matches the timeline frame by frame.

### 9.4 Chaos suite (each case automated pass/fail)

| Case | Pass |
|---|---|
| Camera stops 10 s, resumes | Slate ≤3.1 s, live ≤1.2 s after resume, no output gap |
| Camera flaps every 5 s for 2 min | Output continuous; no state change faster than 1 s |
| Relay kicks the reader every 20 s | Reconnect each time; alert flag after 3 kicks |
| Destination disconnects 30 s | Reconnect ≤5 s after availability; ≤2 s lost; input untouched |
| Destination accepts but stalls | Detected ≤5 s; sink replaced |
| CPU starved (another process at 100 %) | Frames dropped before encoder; clock holds; ladder steps down |
| Score feed down 60 s | Last score stays; `scoreStale` reported; recovers on first poll |
| Camera uplink impaired (400 ms ± 100, 2 % loss, 60 s) | Programme clock holds; no output gap |
| CEF killed | Last overlay frame held; overlay back; video never paused |
| Process killed | Fly restarts it; output back ≤15 s |

The CEF-killed case replaces the PDF's operator kill-switch case, which moves to R4 with slots.

Score correction (void) and scorer clock skew are **page** rules, so they're tested where the page
lives (`seazn.club` R2005: unit + e2e) and proven live in R2 acceptance, not re-implemented in a
harness page (owner ruling 2026-10-14). The fake CF is the off-the-shelf MediaMTX image at a pinned
tag. It runs only on our CI and Machines, and its behaviour is set from the spike's measurement of
real Cloudflare (F-002).

### 9.5 Soak

3 h at 720p30 (owner ruling 2026-10-14), with the fake camera, a scripted match every 20 min, a
dropout every 5 min and one destination disconnect per hour. One GitHub Actions job drives it. The
media runs on Fly Machines (the real image on the production guest, plus a harness Machine), because
private-repo runners have 2 vCPU. It uploads a report. It can't run through real stg web: R1 caps a
session at `MAX_DURATION_MINUTES = 300`, and admission needs a paired, beating phone. Pass: memory within ~~±10 %~~ ±40 % of the first hour
and its fitted growth projected to 300 min within half the production guest's memory (owner rulings 2026-10-25),
zero output gaps, zero programme-pipeline rebuilds caused by input events, dropped frames <0.1 %.

### 9.6 Main-repo PRs

Each PR owes unit + e2e + smoke + regression per `seazn.club/docs/superpowers/RULES.md` and
`TEST-STRATEGY.md`. The UI change gets e2e at all seven widths. **Seams are proven through the real
producer and consumer**: the real compositor image against stg web, not a fixture on both ends.

### 9.7 R2 acceptance

1. Chaos suite green on **3 consecutive runs**.
2. Soak passed.
3. **One real end-to-end on stg**: capture app → composed mode → YouTube, with a deliberate phone
   dropout, watched by the owner, and per-screen verdicts recorded.
4. A runbook entry for every alert the heartbeat can raise.

## 10. Repository layout, CI, deploy

```
seazn.club.compositor/
  CLAUDE.md              @AGENTS.md
  AGENTS.md              rulebook in capture's shape: how to work, scope lock, enforced layering,
                         domain rules, testing, tooling, style, orchestration, verification traps
  _FINDINGS.md           F-ids from spike / chaos / soak; cites shared register IDs (C14, P1…)
  Cargo.toml             workspace; rust-toolchain.toml pinned
  crates/{domain,app,adapters,testkit,compositor}/
  contracts/             relay-job.v1.json + fixtures, VENDORED.json, pin test
  harness/               docker-compose: fake-camera, fake-cf, fake-dest, fake-control-plane;
                         checker/, chaos/, soak/
  docker/Dockerfile      multi-stage: build → runtime (GStreamer + CEF + xvfb), non-root, tini
  fly/stg.toml, fly/prod.toml
  docs/_INDEX.md, docs/_RULES.md, docs/specs/, docs/plans/, docs/decisions/, docs/tasks/
  .claude/agents/        scout / implementer / reviewer (from capture, retargeted)
  .github/workflows/
    check.yml            fmt, clippy -D warnings, domain + app tests, purity / boundary / pin tests
    image.yml            path-filtered: build image, 60 s harness smoke
    chaos.yml            chaos suite (PR to main + workflow_dispatch)
    soak.yml             dispatch-only: launches the soak Machine, collects the report
    deploy-stg.yml       on main: push seazn-relay:<sha> to the Fly registry
    deploy-prod.yml      on v*.*.* tag
```

- **Image handoff:** deploy only pushes the image. Moving `RELAY_IMAGE` in the web env is an explicit
  step, so a compositor push never silently changes what prod runs. (C16, amended 2026-10-25: that
  step is `scripts/promote.sh`, run by the owner on their own flyctl login, never a workflow.)
  (C16, amended 2026-10-26, owner: "let's have beta tag when merge and stable tag in when git tag".
  deploy-stg moves `seazn-relay-stg:beta`, and deploy-prod `seazn-relay-prod:stable`, to each build once
  its build record verifies, and only when it is the newest build: main's tip, or the highest release
  tag (PR #94 fix round 1). RELAY_IMAGE is set once per env, by the owner. While it names the moving
  tag, a merge (stg) or a release tag (prod) changes what NEW sessions run, with no further step. A pin
  to a digest, set with `scripts/promote.sh`, is the explicit step that freezes an env or rolls it back.)
- **Missing secret:** the job prints `::warning::` with the exact `gh secret set …` fix and does
  nothing, rather than failing (the `seazn.club.workflow` convention).

## 11. Risks

| Risk | Mitigation |
|---|---|
| `gstcefsrc` will not build or perform on Fly | T00.1 halts the plan. Fallback for owner decision: `wpesrc` (WPE WebKit, packaged in gst-plugins-bad), at the cost of OBS parity. |
| CF SRT playback behaves differently from the harness (single reader, bursts) | T00.2 measures it before any media code. RTMPS playback is the fallback input path. |
| Fly in-place restarts confuse R1's attempt accounting | §8 item 3 makes "same attempt" explicit, with a sequence test. |
| GStreamer expertise is a single point of failure | The pipeline is documented in-repo. Every element choice is recorded in an ADR or F-id. |
| Cold start (~60 s) on machine loss | Accepted for R2; the warm pool is in R4. |
| CPU headroom on perf-4x with CEF | 720p default (ADR 0005); the ladder steps down; the chaos CPU-starve case. |

## 12. R4 (recorded so it is not lost)

PDF phases 2–4, all built on R2's contracts:

- moments and slots in the overlay page (priority bands, queue ≤20, fades)
- sponsor rules and proof-of-play (`GET broadcast`, `commands?since`, `POST plays`), plus the
  kill switch
- operator console additions (on-air state, template picker, latency calibration helper, programme
  preview)
- warm pool and hot standby / backup ingest
- the 3-match pilot with an operator

The sponsor-moments scenario already scheduled "after R2" lands here.
