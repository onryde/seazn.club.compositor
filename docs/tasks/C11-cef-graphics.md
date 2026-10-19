# C11: Graphics pipeline (CEF renders the overlay page)

**Repo:** `seazn.club.compositor`. **Depends on:** C09, and **C00 PASS**. Read F-001 first: it pins
the gstcefsrc commit, the CEF version, the alpha mode, and **the exact readiness mechanism**.
**Amended 2026-10-15:** a Step 0 (the loopback probe) runs as soon as C00 has passed, ahead of the
rest of this task, because R2005 waits on its finding. See "Amendment 2026-10-15 (overlay templates
spec)" at the end.

## Context

```
cefsrc url=<overlayUrl> <readiness property from F-001> → video/x-raw,format=BGRA,width=1920,height=1080,framerate=fps/1
       → [unpremultiply if F-001 says premultiplied] → appsink(max-buffers=1 drop=true sync=false)
```

- The page always renders at 1920×1080 (ADR 0005). The programme compositor scales it.
- `cefsrc` needs an X display. The Machine runs `Xvfb :99` (C12), and the adapter uses `DISPLAY=:99`.
  The adapter never starts Xvfb itself.
- `restart(gen, url)`: tear down the old graphics pipeline on the reaper thread. The **last overlay
  frame stays** in `FrameStore` (it's held), and a new pipeline is built for `gen`.
- (Amended 2026-10-18, C05b PR #22 review I1) `stop()`: the same reaper teardown and kill fallback as
  `restart()`, with no new pipeline. It returns without waiting, and before it returns it closes the
  stopped generation's slot gate. See "stop() and the slot gate" below.

**Events:**

| Trigger | Event |
|---|---|
| The page's readiness signal (F-001 mechanism) | `CefReady{gen}` |
| Each appsink sample | `FrameStore::set_overlay(sample)` |
| Bus error, renderer process exit, or the CEF child disappearing (poll the pids F-001 identified, every 1 s) | `CefCrashed{gen}` |
| (Amended 2026-10-19, R2005 as built) Page logs `seazn:score-stale=true` or `seazn:score-stale=false` on the same channel as ready: edge-triggered, the first value after ready always sent, then only on a change | `ScoreStale(true)` / `ScoreStale(false)` |
| (Amended 2026-10-15) Page logs `seazn:phase-channel=loopback` or `=web` on the same channel | `PhaseChannel(Loopback/Web)` (C06) |

**There is no load timeout in the adapter.** The domain owns it (C05 `cef_load_timeout_ms`). One
source of truth.

**Guardrails** (spec §6):
- CEF runs under `nice 10`. Pass it through `chrome-extra-flags` or the equivalent F-001 property;
  if neither is possible, `setpriority` on the child pids.
- Memory is capped by watching CEF RSS. Above `cef_rss_cap_mb`, report `CefCrashed{gen}`. The domain
  then restarts it. **Add `cef_rss_cap_mb: 600` to `config.rs`** (spec §6), with a doc line. That's
  the only allowed domain edit in this task.
- OOM order: write `1000` to each CEF child's `/proc/<pid>/oom_score_adj`. The encoder process writes
  `-900` in C12.

## Files

- Create: `crates/adapters/src/cef_graphics.rs`
- Modify: `crates/domain/src/config.rs` (one field)
- Create: `harness/pages/overlay-test.html`. Transparent body, a frame-counter strip, an opaque test
  square at top-left 100×100 (`#0000ff`), and the readiness signal from F-001 sent after 500 ms. Add
  query switches:
  - `?noready=1` (never signal)
  - `?crash=1` (`while(true){}` after ready, to simulate a hang)
  - `?stale=1` (after ready, log `seazn:score-stale=true` once; amended 2026-10-19, R2005's line form)
  - (amended 2026-10-15) `?channel=loopback|web|bogus` (after ready, log `seazn:phase-channel=<value>` once)
- (Amendment 2026-10-15) Create: `harness/pages/phase-probe.html` and `harness/phase-probe-server.py`
  (Step 0).
- Test: `crates/adapters/tests/graphics.rs`. These tests need `cefsrc`, so they run **in the C12 image
  only**. Gate them with `#[cfg_attr(not(feature = "cef"), ignore)]` and an `adapters` feature `cef`.
  CI's `image.yml` (C12) runs `cargo test --features cef`.

## Interfaces

- **Consumes:** `app::ports::{GraphicsRenderer, Inbound}`, `FrameStore`, `domain::supervisor::Event`.
- **Produces:** `pub struct CefGraphics; impl CefGraphics { pub fn new(frames: FrameStore, tx: Sender<Inbound>, fps: u32, rss_cap_mb: u64) -> Self; }`
  and `impl GraphicsRenderer for CefGraphics`.

## Steps

- [ ] **Step 1: Write the failing tests** (`--features cef`, single-threaded):

  | Test | What it asserts |
  |---|---|
  | `ready_signal_arrives_with_its_generation` | `restart(1, file://…/overlay-test.html)` → `CefReady{gen:1}` within 5 s; the overlay sample is 1920×1080 BGRA with an opaque blue pixel at (50,50) and alpha 0 at (960,540) |
  | `page_without_ready_signal_times_out` | review focus 2: with `?noready=1`, frames arrive but **no** `CefReady` within 8 s; the domain timeout is C05's, so here assert only that the adapter never fakes ready |
  | `crash_of_the_renderer_reports_cefcrashed` | kill the CEF renderer pid found by the adapter → `CefCrashed{gen}` within 2 s |
  | `restart_holds_the_last_overlay_frame` | after `restart(2)`, and before gen 2 produces a frame, `FrameStore::overlay()` is still gen 1's last frame (pointer-equal or same PTS) |
  | `score_stale_signal_is_relayed` | `?stale=1` (the page logs `seazn:score-stale=true`) → `ScoreStale(true)`; a line `seazn:score-stale=false` → `ScoreStale(false)` |
  | `rss_over_cap_reports_crash` | construct with `rss_cap_mb = 1` → `CefCrashed{gen}` within 3 s |
  | `stale_generation_events_are_tagged_with_their_own_gen` | `restart(1)`, `restart(2)` quickly; any `CefReady` from the first page carries `gen:1`, never 2 |

- [ ] **Step 2: Run in the image, expect a FAIL.**
  `docker run --rm <c12-image> bash -lc 'Xvfb :99 -screen 0 1920x1080x24 & DISPLAY=:99 cargo test -p adapters --features cef --test graphics -- --test-threads=1'`.
  If C12 is not built yet, use the C00 spike image plus the Rust toolchain.

- [ ] **Step 3: Implement it.** Use the readiness mechanism exactly as F-001 pinned it. If F-001 found
  no in-element mechanism, use the fallback it recorded, such as a console-message or title hook.
  Never infer readiness from "first frame arrived": a page that rendered an error is not ready.

- [ ] **Step 4: Run, expect a PASS.** Paste the counts.

- [ ] **Step 5: Mutate once each.**
  1. Emit `CefReady` on the first frame. Expected red: `page_without_ready_signal_times_out`.
  2. Clear the overlay slot on restart. Expected red: `restart_holds_the_last_overlay_frame`.

  3. **(Amendment 2026-10-15)** Map every `seazn:phase-channel=` line to `Loopback`. Expected red:
     `phase_channel_signal_is_relayed` (the `web` case).

- [ ] **Step 6: Commit.** `feat(media): CEF overlay renderer with held frame and readiness (C11)`

## Acceptance

- ~~7~~ 8 tests pass in the image (amended 2026-10-15).
- ~~2~~ 3 mutants went red (amended 2026-10-15).
- The readiness mechanism matches F-001 verbatim.
- (Amended 2026-10-15) Step 0's finding is in `_FINDINGS.md`, with one of the outcomes (a), (b) or (c)
  and the raw console and server lines.
- (Amended 2026-10-15, G1) The first `cefsrc` compatibility record (the "Compatibility" section) is in
  `_FINDINGS.md`, with the scene leg either run or recorded as owed to C17.
- (Amended 2026-10-18, Owed 18) `seazn:alive` is relayed as `CefAlive{gen}`, the harness page
  beats, and the reaper kill fallback is proven: three tests, and mutants 4 and 5 went red.
- (Amended 2026-10-18, C05b PR #22 review I1) `stop()` takes the reaper path and returns at
  once, and the slot gate closes at `restart`, `stop` and a reported crash. Both new tests pass with
  non-zero dropped-sample counts, and mutants 7–9 went red.
- (Amended 2026-10-18, Owed 17) Ready-gated slot writes:
  `frames_before_ready_never_reach_the_slot` and
  `the_hidden_page_stays_off_air_and_the_retry_reaches_air` pass with non-zero counts on both
  sides, and mutant 6 went red.
- (Amended 2026-10-18, Owed 17) F-0xx `cef error pages` is in `_FINDINGS.md`, with all three
  legs' alpha samples.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.3 (D10; verbatim, `29fa9f59c`): "the page is https and the endpoint is loopback, so the compositor
answers the preflight with `Access-Control-Allow-Private-Network: true` and a CORS origin locked to
the app origin. **CEF 139 behaviour must be verified as C11's first step.** If it's blocked, the
fallback becomes primary." §6, C11 row: "**Step 0:** verify a https page → `127.0.0.1` fetch in CEF 139
with the PNA preflight. Record the result in `_FINDINGS.md`; if it's blocked, R2005 flips to fallback B
as primary." §5: "the page is tested in OBS's browser source and in the compositor's `cefsrc` every
release (PDF §4). The C11/C12 image tests load a scene URL in CEF."

### Step 0: Loopback probe (run first; its own docs commit on `main`)

R2005 starts its phase hook in the channel this step picks, so this step runs **as soon as C00 has
passed**, in the C00 spike image (`spike/t00` @738eb88, the F-001 build: CEF
`139.0.28+g55ab8a8+chromium-139.0.7258.139`). It doesn't wait for C09. Nothing here is assumed:
Chromium's handling of a public HTTPS page calling `http://127.0.0.1` has changed across recent
releases (Private Network Access, then Local Network Access), so the answer for **this** CEF is
measured.

- `harness/phase-probe-server.py`: Python stdlib only, bound to `127.0.0.1:7419` (the R2007 PR B
  `RELAY_PHASE_PORT`). Flags:
  - `--variant none|pna`. With `none`, no CORS headers at all. With `pna`, `OPTIONS` answers 204 with
    `Access-Control-Allow-Origin: <the request's Origin>`, `Access-Control-Allow-Methods: GET`,
    `Access-Control-Allow-Private-Network: true` and `Vary: Origin`, and `GET` answers carry the same
    `Access-Control-Allow-Origin`.
  - `--hold-ms N`, default 2 000: wait before answering `GET /phase?after=S`.
  - **(Added 2026-10-15, coordinator decision G1)** `--script waiting:5000,reconnecting:5000,final:5000`:
    instead of a fixed answer, serve that phase sequence. Each entry holds for its milliseconds,
    `seq` rises by one per entry, and a long-poll on the current `seq` is answered at the next change.
    After the last entry, it serves `live`. **The script clock starts at the first `GET /phase`**, not
    at server start, and the server logs every transition with its wall-clock time in ms
    (`transition <phase> seq=<n> at=<epoch ms>`). An entry naming a phase outside the five programme phases
    is refused at start-up (exit 2), never served. The compatibility step below uses it.

  The answer is `{"phase":"live","seq":S+1}`, R2007 PR B's `RelayPhaseReply` shape. **Every** request
  logs one line to stdout: method, path, `Origin`, `Access-Control-Request-Private-Network` and
  `Access-Control-Request-Method`.
- `harness/pages/phase-probe.html`: on load, `fetch("http://127.0.0.1:7419/phase?after=0")`. It logs
  through F-001's console channel, so the result arrives in `cefconsole` with no second mechanism:
  - `console.info("seazn:probe=ok:" + body)` on success
  - `console.info("seazn:probe=blocked:" + err.name + ":" + err.message)` on a rejection
- **The origin must be public HTTPS**, as in production. Serve `harness/pages/` from the host with
  `python3 -m http.server 8080` and expose it with `cloudflared tunnel --url http://localhost:8080`,
  which prints an `https://<random>.trycloudflare.com` URL. Run the probe server **inside** the
  container, in CEF's own network namespace.
- **Runs.** Each is one container, `Xvfb :99`, the server, and
  `GST_DEBUG=cefconsole:4 gst-launch-1.0 cefsrc url=https://<tunnel>/phase-probe.html listen-for-js-signals=false max-video-framerate=30/1 ! video/x-raw,format=BGRA,width=1920,height=1080 ! fakesink`.
  Stop it 20 s after the server's first log line, or 30 s after start if the server never logs.

  | Run | Server | CEF flags (`chrome-extra-flags`) |
  |---|---|---|
  | V1 | `--variant none` | none |
  | V2 | `--variant pna` | none |
  | V3a | `--variant pna` | `disable-features=LocalNetworkAccessChecks` |
  | V3b | `--variant pna` | `disable-features=PrivateNetworkAccessRespectPreflightResults` |
  | V4 | `--variant pna --hold-ms 9000` | the first of V2/V3a/V3b that passed |

  `chrome-extra-flags` splits on commas, so a multi-feature `disable-features=A,B` value can't pass
  through it. That's why V3a and V3b each carry one feature. If neither single flag works, record
  that and stop; don't invent a delivery path.
- **Record, per run:**
  - the `seazn:probe=` line verbatim
  - every server log line, which shows whether a preflight arrived, with what headers, and the
    `Origin` value
  - any mixed-content or "Private Network"/"Local Network" console line

  V4 proves that a long-poll held for 9 s (inside `RELAY_PHASE_LONG_POLL_MAX_MS = 10 000`) returns
  intact.
- **Outcome and decision**, as `F-0xx: CEF loopback phase channel` in `_FINDINGS.md` (the next free
  F-id at the time of writing):
  - **(a)** V2 passes. Loopback is primary, with C12's headers and no flags.
  - **(b)** V2 fails, V3a or V3b passes. Loopback is primary. This task's adapter (Step 3, which runs
    after Step 0) sets that one feature in `chrome-extra-flags`, citing the F-id, and
    `ready_signal_arrives_with_its_generation` asserts the flag is in the element's property.
  - **(c)** V2, V3a and V3b all fail. **Fallback B becomes primary**: R2005's hook starts in `web`,
    and C12 still ships the endpoint for a later CEF. Record the reason verbatim, as spec §6 requires.

  V1 is the control. If V1 **passes**, Chromium enforces nothing here, which is the outcome (a)
  family; still keep C12's headers so a CEF bump doesn't break it.
  Also record the `Origin` header the server saw. C12's exact-origin check is written against that
  observed form (scheme + host, no trailing slash), not against an assumed one.
- Commit: `docs(findings): CEF loopback phase channel probe (C11 Step 0)`, with the harness files, on
  `main`, before R2005 starts.

### Compatibility: a scene URL through `cefsrc`, every release (coordinator decision G1)

Spec §5: "the page is tested in OBS's browser source and in the compositor's `cefsrc` every release
(PDF §4). The C11/C12 image tests load a scene URL in CEF." This step is the `cefsrc` half; R2007
step 14b is the OBS half, and C17's acceptance lists both records.

- **When:** once in this task, after Step 4, and then on every compositor release image and every web
  release that touches `components/overlay/**`. Each run is recorded, never assumed.
- **Pre-condition:** R2005 is live on stg (the scene layer exists). If it isn't when this task
  reaches here, run the **template leg** only (no `--script`; the template renders and `seazn:ready`
  arrives), and record the scene leg as owed to C17. Don't skip the record. The same applies if
  Step 0 recorded outcome (c): the page then reads its phase from web (fallback B), which needs a
  real composed session, so only C17 can drive the scenes.
- **The page:** a public stg fixture whose org has the overlay entitlement and which is **in
  progress** at run time: started, not decided (confirm pass N2). An upcoming or decided fixture shows
  a match card while the phase is `live` (R2007 check 4 counts cards in `warming` and `ended` cells),
  so the `live` alpha-0 check below would false-red. Start one on the stg pad if none is in progress
  (one scored point is enough). Read its status from the public fixture read just before the run and
  record it; if it isn't in progress, **refuse the run** rather than skip the check. C00 used
  `/overlay/fixtures/02786dd0-…` (F-001), which may have ended or moved. The URL is `https://<stg app>/overlay/fixtures/<id>?host=compositor&template=<id>`, once per
  `OVERLAY_TEMPLATES` id (re-pin the list from R2007's registry). There is no `sid`/`st`: a public
  fixture needs no relay access path, and `host=compositor` alone turns on the signals and the scene
  layer.
- **The run:** in the release image, run `Xvfb :99`, then
  `harness/phase-probe-server.py --variant pna --script waiting:5000,reconnecting:5000,final:5000`,
  then
  `cefsrc url=<that URL> … ! videorate ! video/x-raw,framerate=1/1 ! videoconvert ! pngenc ! multifilesink location=frame-%03d.png`.
  Stop it 8 s after the server logs its `live` transition, or 45 s after start if it never does
  (then fail: the page never followed the script). The probe server is the same stand-in for C12's
  endpoint that Step 0 uses.
- **Timing (confirm pass N2):** frames are timed by their file's mtime (`os.stat(..).st_mtime`,
  sub-second; same container, same clock), and each phase by the server's transition log, so CEF's
  cold start in Xvfb doesn't matter. A phase's window runs from its transition to the next one,
  shrunk by 1 s at each end to cover render latency. The `live` window ends at the last frame.
- **Assert, per template:**
  - `seazn:ready` arrives (F-001's mechanism)
  - the probe server logs at least 3 `GET /phase` lines (the page followed the script)
  - each of the four windows (`waiting`, `reconnecting`, `final`, then `live`) holds at least one
    frame; a window with none fails
  - in every frame inside the `waiting`, `reconnecting` and `final` windows, the pixel at the centre
    of the card region (`cardRegionFor(<template's sceneCardSafeArea>)`, R2007 step 9b) has alpha
    255, because the scene backdrop is opaque
  - in every frame inside the `live` window, that same pixel has alpha 0: no scene, and no card,
    because the fixture is in progress

  Print `templates checked: N, frames checked: M (per window: …)`; fail if N or M is 0.
- **Look at the PNGs** (one per scene per template) for a scene card that's legible, clear of the
  template, and not clipped. **Never commit them** (owner rule): they stay in the run's scratch
  directory. Post them in the PR or release comment.
- **Record** `F-0xx: cefsrc scene compatibility, <image tag>` in `_FINDINGS.md`, with the CEF version,
  the URLs, the probe log and a verdict per check.

### The channel signal (Steps 1–4)

R2005's page logs `seazn:phase-channel=loopback` or `seazn:phase-channel=web` on the console, once
when the channel settles and again on each change. The adapter parses it from the same `cefconsole`
stream as `seazn:ready` and sends `Inbound::Event(Event::PhaseChannel(..))` (C06's event). Any other
value is ignored and logged at WARN, never mapped to a default. C07's heartbeat emits it as
`phaseChannel`, and C17's stg run is the permanent proof that a real CEF on Fly chose `loopback`.

New test, added to Step 1's table:

| Test | What it asserts |
|---|---|
| `phase_channel_signal_is_relayed` | `?channel=web` → `PhaseChannel(Web)`; `?channel=loopback` → `PhaseChannel(Loopback)`; `?channel=bogus` → **no** `PhaseChannel` event within 3 s after `CefReady` (the guard). Print `channel values checked: 3`. |

## Amendment 2026-10-18 (hang detection, owner ruling)

Owner, 2026-10-18, in chat, relayed by the coordinator: "2 Ok", approving hang detection (`docs/_INDEX.md` Owed 18). The domain half is C05b. This task relays the beat and kills a renderer that will not
tear down.

- **Parse the beat.** R2005's page logs `seazn:alive` every `cef_alive_every_ms` (2 s) after ready,
  with `host=compositor` only, on F-001's console channel (`cefconsole`), the same stream as
  `seazn:ready`. It never uses `gstSendMsg`: that accepts only `ready`/`eos`, and F-001 found that
  the js-signal mode hangs `start()` and `set_state(NULL)`. The adapter sends
  `Inbound::Event(Event::CefAlive{gen})`, tagged with the generation of the pipeline that logged it.
  The line is matched exactly. Any other `seazn:alive…` text is logged at WARN and mapped to nothing.
- **The harness page beats too.** `overlay-test.html` logs `seazn:alive` every 2 s after its ready
  (false-premise row "The hang design needs only the real page to beat"). `?crash=1`
  (`while(true){}` after ready) stops the beat with the rest of the page, and that is the point.
  `?crash=1` gains an optional `&crash_after_ms=N` (default 500) so C14 can time the hang.
- **Reaper kill fallback.** `restart(gen, url)` tears down on the reaper thread. When the old
  pipeline's `set_state(NULL)` has not returned within `REAPER_KILL_AFTER_MS` (3 000, a const in
  `cef_graphics.rs` that cites F-001: it did not return within 10 s in the js-signal hang), the
  reaper sends `SIGKILL` to the old generation's CEF renderer pids (the pids F-001 identified and
  the crash poll already watches). It logs the pids at WARN and moves on. The new generation is
  built **without** waiting for the reaper. The held overlay frame stays in `FrameStore` throughout.
- **New tests** (Step 1's table):

  | Test | What it asserts |
  |---|---|
  | `alive_beats_are_relayed_with_their_generation` | `restart(1, …overlay-test.html)`: within 7 s, at least 2 `CefAlive{gen:1}` after `CefReady{gen:1}`, and none before it |
  | `hung_page_stops_beating` | `?crash=1`: `CefAlive{gen:1}` arrives before the hang, and **none** in the 3 s after it, while the renderer pid is still alive (it is a hang, not a crash: no `CefCrashed`) |
  | `reaper_kills_a_renderer_that_will_not_tear_down` | **Drive the kill path deterministically.** Gen 1 loads F-001's js-signal hang page, where `set_state(NULL)` is known not to return within 10 s. Then `restart(2, …overlay-test.html)`. Within `REAPER_KILL_AFTER_MS` + 2 s, every gen-1 renderer pid is gone, and `CefReady{gen:2}` arrives. The reaper reports which path ran, and the test **requires the kill path**: print the pids killed, and fail on 0. If the js-signal hang ever tears down by itself, the test fails and names it, never passing on the other path (PR #21 review m1) |

- **New mutants** (Step 5):
  4. Map `seazn:alive` to nothing. Expected red: `alive_beats_are_relayed_with_their_generation`.
  5. Delete the reaper's kill. Expected red: `reaper_kills_a_renderer_that_will_not_tear_down`.

### Owed 17: no frame of a generation reaches air before that generation's own ready (Owner, 2026-10-18, in chat, relayed by the coordinator: "apply rec 4-9")

This is the coordinator's decision of 2026-10-16.
- The adapter **never** writes `FrameStore::set_overlay` from generation `g` until `g`'s own
  `seazn:ready` line. Until then, the previous generation's last frame stays held, or nothing on
  the first. The Events table's "Each appsink sample → `FrameStore::set_overlay(sample)`" row is
  gated on that.

New image tests (Step 1's table):

| Test | What it asserts |
|---|---|
| `frames_before_ready_never_reach_the_slot` | `?noready=1`: the appsink delivers frames (count them, fail on 0), and the slot still holds the previous generation's frame, compared by bytes, or is empty on the first |
| `the_hidden_page_stays_off_air_and_the_retry_reaches_air` | The positive pair, through C09's real `ProgrammePipeline` to a `file://` target. `cef_fail_count` × `?noready=1` restarts, then `hide_overlay()` + `stop()` (C08's mapping): no decoded programme frame carries the test square's blue from the hide until the retry. Then `restart(n+1, …overlay-test.html)`: after its `CefReady`, the blue **is** present. Print the frames checked on each side, and fail on 0 |

**New mutant 6:** set the slot on every sample, regardless of ready. Expected red:
`frames_before_ready_never_reach_the_slot`.

### Owed 17: does CEF 139 paint an error page opaque? (a measurement)

Record F-0xx `cef error pages`. Load three URLs through `cefsrc` in the image:
1. a 404 from the probe server
2. a 502 from the probe server (`--status 502`)
3. a host that does not resolve (a Chromium net-error page)

For each, record whether frames arrive, the alpha at five sample pixels (the corners and the
centre), and whether any `seazn:` line appears. The gate above makes none of them reach air before a
ready, so this measures how much the gate protects. It is not a pass/fail. Paste the raw numbers.

### `stop()` and the slot gate (amended 2026-10-18, C05b PR #22 review I1; coordinator decision, relayed)

C05b made `HideOverlay → stop()` target a **hung** renderer for the first time. That is F-001's case,
where `set_state(NULL)` did not return for 10 s. Before this amendment `stop()` had no stated
semantics, and both readings broke the hide:
- If it tears down synchronously on C08's loop thread, the loop that drives the programme ticks
  blocks for 3–10 s, and a graphics hang stalls video.
- If it is asynchronous with the gate left open, a page that revives can write the slot between the
  hide and the teardown. CPU starvation at nice 10, the brief's own reason for 10 s, is exactly how a
  hung page revives. The slot would then hold that frame through the next generation's Starting.

So:
- **`stop()` is the reaper path.** It tears the current pipeline down on the reaper thread, with
  `restart()`'s `REAPER_KILL_AFTER_MS` kill fallback, and returns without waiting. It never blocks
  C08's loop.
- **The slot gate** is one adapter field: the generation whose samples may reach
  `FrameStore::set_overlay`, opened only by that generation's own `seazn:ready` (Owed 17 above).
  It closes synchronously, before the call returns or the event is sent:
  - for the old generation, on `restart(g)`
  - on `stop()`
  - for generation `g`, when the adapter reports `CefCrashed{g}`

  The last of these is what makes C05b's held frame a hold: a crashed page whose renderer is still
  alive (`cef_rss_cap_mb`) stops writing at its report. The first does the same for a recycled page
  at its restart (C05b's one bound, amended 2026-10-18 in the review's second round: a crashed or
  recycled page's last frame stays in the slot at most `cef_hang_after_ms` after that page's last
  beat, then `hide_overlay` clears it). A sample from a closed generation is dropped and counted. So
  nothing of a hidden, crashed or recycled generation reaches the slot again, and the hide holds
  until a later generation signals ready.
- **The harness page revives on request.** `?crash=1` gains `&revive_after_ms=N`: the busy loop ends
  after N ms, and the page beats and paints again. A page that never revives cannot witness the gate.

New image tests (Step 1's table):

| Test | What it asserts |
|---|---|
| `stop_never_blocks_and_closes_the_slot_on_a_page_hung_after_ready` | `restart(1, …overlay-test.html?crash=1&crash_after_ms=500&revive_after_ms=1500)`. `CefReady{1}` arrives, then at least one gen-1 sample in the slot (count them, fail on 0). During the hang, call `hide_overlay()` and then `stop()`, as C08 maps a `Hang` hide. `stop()` returns in under 200 ms (print it): it does not wait out `set_state(NULL)`. From its return on, no gen-1 sample reaches the slot. The slot stays empty, compared by bytes, while the revived page's appsink still delivers gen-1 samples that the gate drops. Count them, and fail on 0, naming it: a run in which the page never painted after `stop()` proves nothing. Every gen-1 renderer pid is gone within `REAPER_KILL_AFTER_MS` + 2 s |
| `a_reported_crash_closes_its_generations_gate` | `cef_rss_cap_mb` is overridden low, so the adapter reports `CefCrashed{1}` while the page is alive and painting. From the report on, no gen-1 sample reaches the slot. The slot keeps its last frame (the hold), compared by bytes, and the gate counts the samples it dropped, failing on 0 |

**New mutants** (Step 5):
7. `stop()` leaves the gate open. Expected red: `stop_never_blocks_and_closes_the_slot_on_a_page_hung_after_ready`.
8. `stop()` joins the teardown on the caller's thread. Expected red: the same test's 200 ms bound.
9. The crash report leaves the gate open. Expected red: `a_reported_crash_closes_its_generations_gate`.

## Amendment 2026-10-19 (C08 PR #27 review round 1: a bounded `Drop`)

C08's `run()` returns after its final-beat flush, and the driver drops with every port as it returns,
so `CefGraphics`'s `Drop` runs on the exit path, after the final beat and before the process ends. It must
return within a bound: it never waits on a pipeline, a renderer or a thread that may hang (F-001:
`set_state(NULL)` did not return for 10 s on a hung renderer); teardown that may block goes to a
reaper thread, as `stop` does, or is bounded by a timeout. **Acceptance:** a test drops the adapter
while what it owns is hung (or stands in for hung) and the drop returns within 500 ms.

## Amendment 2026-10-19 (as built, PR #32; F-013, F-014, F-015)

- **The gate is `required-features`, not `#[ignore]`.** `tests/graphics.rs` is a `[[test]]` with `required-features = ["cef"]`, and `adapters` has a feature `cef = []`. check.yml fails a run with any ignored test, and CI has no cefsrc. The C12 image runs `cargo test -p adapters --features cef --test graphics -- --test-threads=1` under `Xvfb :99`.
- **The Events table, as built:**
  - **"Each appsink sample"** is written only while its build's gate is open: from its own `seazn:ready` until `restart`, `stop`, the drop or a decided crash. It is converted to straight alpha first (cefsrc's is premultiplied, F-001).
  - **"Renderer process exit"** is cefsrc's `Render subprocess terminated, reloading URL!` warning.
  - **"The CEF child disappearing"** is not a crash signal: a killed GPU process recovers by itself (F-013).
  - The cap reads the **PSS** sum of CEF's children, not their RSS.
  - `CefReady`, `CefAlive` and `CefCrashed` carry their build's own generation; `ScoreStale` and `PhaseChannel` come only from the current build.
- **Guardrails, as built:** no flag or property sets nice, so each 1 s poll renices every thread of every CEF child (Linux nice is per thread) and writes `oom_score_adj`. Every element sets `chrome-extra-flags` to `disable-features=SpareRendererForSitePerProcess` (amending F-009's "no flags"; the flag touches no networking) and `log-severity=disable`.
- **Logging:** the first `CefGraphics` replaces GStreamer's default log handler for the process, and every line it prints is scrubbed. Never set `GST_CEF_LOG_SEVERITY` (Owed 52).
- **The tests:**
  - 19 in the image: the brief's 15, plus `a_renderer_born_just_before_a_restart_stays_the_retiring_pages`, `cef_children_run_at_nice_10_with_oom_score_adj_1000`, `the_page_token_never_reaches_a_log_a_file_or_an_argv` and `drop_returns_within_500_ms_while_the_page_is_hung`.
  - 11 unit tests run in CI.
  - The stop test waits for the observed hang (300 ms without a sample) before its hide and stop, and holds the teardown with the `delay_teardowns` hook. A console-mode teardown returns in 7 ms even for a hung page (F-013).
  - The reaper and drop tests use the js-signal hook, F-001's real hang.
- **Mutation:** the brief's 9, plus 22 on this task's guards: 31 red.
- **The compatibility step (G1) was refused** (F-015): no stg fixture was in progress, and none could be started without a staff login. Both legs are owed (Owed 21).
- **Owed for others:** 52–54 (C12), 55 and 56 (owner questions), 57 (C08: stop graphics before hiding the overlay).

## Amendment 2026-10-19 (PR #32 review round 1: the fix loop)

The coordinator's decisions and the owner's ruling "B" are in `docs/_INDEX.md`'s Decision log (two rows). What
changed for this brief:

- **Do not touch, one exception:** `crates/app/src/driver.rs` stops graphics before hiding the overlay, and
  `crates/app/tests/scenarios.rs` pins the order (Owed 57, closed).
- **Secrets, at rest:** CEF's profile holds every URL it loaded. The adapter gives the process one private profile
  directory (`cef-cache-location`, mode 0700, under `temp_dir`), removed by the last drop and swept at the next
  start; a profile per build is impossible (CEF reads it once per process). C12 puts `TMPDIR` on tmpfs (Owed 58).
- **Guardrails:** `oom_score_adj` is graded by kind (renderer 1000, GPU and utility 800, zygote 500), not 1000 on
  every child. The cap counts no other build's renderer.
- **Health (owner ruling "B"):** `CefGraphics::health()` reports a dead renderer zygote (`unrecoverable`) and the
  leaked teardowns. Acting on them is C11b (`docs/tasks/C11b-cef-health.md`, Owed 59).
- **Waits:** the image tests wait on events and counts, bounded by the domain's thresholds; the hooks
  `hold_teardowns`, `set_reaper_kill_after` and `hold_next_write` replace `delay_teardowns`.
- **The tests:** 23 in the image. Renamed: `the_page_token_never_reaches_a_log_line_chrome_debug_log_or_an_argv`
  and `cef_children_run_at_nice_10_with_the_renderer_first_in_the_oom_order`. New:
  `a_page_token_rests_on_disk_only_in_the_private_profile_until_the_drop`,
  `a_hide_before_the_stop_can_leave_a_stale_frame_and_after_it_cannot`,
  `a_dead_renderer_zygote_is_reported_and_the_gpu_zygotes_is_not` (two child processes, one zygote each) and
  `a_retiring_builds_renderer_does_not_count_against_the_new_builds_cap`. 18 unit tests in `cef_graphics` run in CI, and
  check.yml now compiles `tests/graphics.rs` too.
- **The harness page** gains `echo_token=1` and a `hang starts` warning before its busy loop.
- **F-014's residual:** a ready page that navigates paints the new document (white, on an error) until the hang
  hide; no producer is known.

## Do not touch

The programme and input adapters, and the page in `seazn.club` (that's R2005). (Amended 2026-10-15)
The HTTP endpoint itself is C12's (`phase_http.rs`); Step 0's probe server is a harness file and is
never shipped.

## Output cap

Under 15 lines.
