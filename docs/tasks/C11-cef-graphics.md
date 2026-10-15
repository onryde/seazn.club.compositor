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

**Events:**

| Trigger | Event |
|---|---|
| The page's readiness signal (F-001 mechanism) | `CefReady{gen}` |
| Each appsink sample | `FrameStore::set_overlay(sample)` |
| Bus error, renderer process exit, or the CEF child disappearing (poll the pids F-001 identified, every 1 s) | `CefCrashed{gen}` |
| Page signals score-stale (R2005 adds a JS signal on the same channel as ready) | `ScoreStale(true/false)` |
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
  - `?stale=1` (signal score-stale)
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
  | `score_stale_signal_is_relayed` | `?stale=1` → `ScoreStale(true)` |
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

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.3 (D10): "Primary (A): the compositor serves its programme phase on a loopback HTTP endpoint …
Chromium may require Private Network Access headers or a CEF switch for a public HTTPS origin calling
`127.0.0.1`; this is to be proven in C11." §6: "C11 (amend): proves CEF can call the loopback endpoint
from the public HTTPS overlay origin (Private Network Access); if not, B becomes primary and the
reason is recorded in `_FINDINGS.md`."

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

## Do not touch

The programme and input adapters, and the page in `seazn.club` (that's R2005). (Amended 2026-10-15)
The HTTP endpoint itself is C12's (`phase_http.rs`); Step 0's probe server is a harness file and is
never shipped.

## Output cap

Under 15 lines.
