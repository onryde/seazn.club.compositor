# C11: Graphics pipeline (CEF renders the overlay page)

**Repo:** `seazn.club.compositor`. **Depends on:** C09, and **C00 PASS**. Read F-001 first: it pins
the gstcefsrc commit, the CEF version, the alpha mode, and **the exact readiness mechanism**.

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

- [ ] **Step 6: Commit.** `feat(media): CEF overlay renderer with held frame and readiness (C11)`

## Acceptance

- 7 tests pass in the image.
- 2 mutants went red.
- The readiness mechanism matches F-001 verbatim.

## Do not touch

The programme and input adapters, and the page in `seazn.club` (that's R2005).

## Output cap

Under 15 lines.
