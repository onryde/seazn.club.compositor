# C12: Binary wiring, process metrics, Dockerfile, image CI

**Repo:** `seazn.club.compositor`. **Depends on:** C10, C11. Read F-001 (gstcefsrc commit and CEF
version) and F-005 (cold start). **Amended 2026-10-15:** also **R2004's PR 0** (the standalone web
"relay stop contract" PR: the SIGTERM `stop_config`, and the 20 s grace ruled by the owner on
2026-10-16, spec D15) deployed to
stg, and C11 Step 0's loopback finding. (Corrected 2026-10-15, review m9: C12 waits on PR 0 alone,
not on R2004 or R2007.) See "Amendment 2026-10-15 (overlay templates spec)" at the end.

## Context

R1's runner (pinned at `seazn.club` `apps/web/src/server/relay/runner-fly.ts:234-238`, 2026-10-14;
re-pin before relying on it) creates the Machine with:

- `image: RELAY_IMAGE`
- `env: { SESSION_ID, JOB_TOKEN, APP_URL, RELAY_DEADLINE_AT }`. `RELAY_DEADLINE_AT` is an **ISO-8601
  string** from `Date.toISOString()`, e.g. `2026-10-14T18:30:00.000Z`.
- the stop signal is `SIGINT` (`config.ts` `RUNNER_STOP_SIGNAL`), with a grace of
  `RUNNER_STOP_GRACE_SECONDS = 10` (20 after R2004 PR 0: owner ruling 2026-10-16, spec D15)
- **(Amended 2026-10-15, coordinator decision I4)** after R2004's PR 0, also
  `stop_config: { signal: "SIGTERM", timeout: <grace> }`. So web's own stop (an explicit
  `signal: "SIGINT"` on the stop call) arrives as SIGINT, while a Fly-initiated stop (deploy,
  `fly machine stop`, auto-stop) arrives as SIGTERM. Without PR 0, Fly's default is SIGINT with a 5 s
  timeout (docs.fly.io `kill_signal`/`kill_timeout`). Re-pin `runner-fly.ts` after PR 0.

The binary reads those four values, builds every adapter, and runs `app::run::run`. It is the only
place that reads `std::env`, the system clock, or signals.

### `COMPOSITOR_DRIVERS`

| Value | Adapters |
|---|---|
| `live` | `HttpControlPlane`, `GstInput`, `GstProgramme`, `CefGraphics`, `ProcMetrics` |
| `fake` | testkit fakes: a scripted control plane that serves `session_720p()` and replies `live` |
| unset, **release build** | refuse: print `COMPOSITOR_DRIVERS must be set (live|fake)` to stderr, exit 78 |
| unset, debug build | `fake` |

This mirrors R1's `RELAY_DRIVERS` refuse-when-unset rule, so a production image can never silently
run fakes. The Dockerfile sets `COMPOSITOR_DRIVERS=live`.

### Signals

`SIGINT` → `Inbound::Event(Event::Signal(SignalKind::Int))` and `SIGTERM` → `…Signal(SignalKind::Term)`, via the `signal-hook` crate's iterator on a
dedicated thread. A second signal during drain is ignored. (Amended 2026-10-15: `SIGINT` now starts
C06's final hold and `SIGTERM` still drains at once; a second `SIGINT` during the hold is ignored.) The domain owns the drain deadline
(C06 `drain_timeout_ms`, which must be below the 10 s grace; **assert that in a test here**, deriving
10 s from a constant `R1_STOP_GRACE_MS` in `crates/compositor/src/main.rs` with a doc line citing
`config.ts RUNNER_STOP_GRACE_SECONDS`). (Amended 2026-10-16: the grace is 20 s by owner ruling, spec
D15, and the test is the amendment's `final_hold_and_drain_fit_inside_r1_stop_grace`.)

### Process priorities (spec §6)

At start: write `-900` to `/proc/self/oom_score_adj`. That's allowed for a non-root process only when
lowering is permitted, so on `EACCES`, log once and continue (the Machine runs as non-root; F-005
records whether it works). CEF children get `1000` (C11).

### Metrics (`metrics_proc.rs`)

| Field | Source |
|---|---|
| `cpu_pct` | `/proc/self/stat` + children (`utime+stime` delta ÷ wall delta ÷ ncpu × 100), summed over the process tree |
| `mem_mb` | sum of `VmRSS` over the tree |
| `fps`, `bitrate_kbps`, `egress_bytes`, `encoder_speed` | `adapters::stats::Stats::snapshot()`, shared as `Arc<Stats>`. C09 defines and tests every field's arithmetic; C12 only wires the `Arc` into `ProcMetrics`. A field C09 reports as `None` stays absent on the wire. |
| `measured_latency_ms` | **None** in R2 (needs SEI; F-003). Omitted, never 0. |

The first sample after start returns `cpu_pct: None`, because there's no delta yet.

## Files

- Create: `crates/compositor/src/{main.rs,env.rs,wiring.rs,logged.rs}`. `logged.rs` holds
  `LoggedProgramme<P: ProgrammeOutput>`, a decorator that logs `programme started` on a successful
  `start()` and `sink swap gen=<n> target=<redacted>` on `swap_sink`, then delegates. It wraps both the
  live and the fake programme, so the log lines tests rely on exist in both driver modes.
- Create: `crates/adapters/src/metrics_proc.rs`
- Create: `docker/Dockerfile`, `docker/entrypoint.sh`, `.dockerignore`
- Create: `.github/workflows/image.yml`
- (Amended 2026-10-15, review m4) Create: `crates/compositor/examples/smoke_budget.rs`. Modify: the
  workspace `Cargo.toml` lint comment (the second sanctioned `print_stdout` exception).
- Test: `crates/compositor/tests/env.rs`, `crates/adapters/tests/metrics_proc.rs`,
  `crates/compositor/tests/fake_run.rs`
- `crates/compositor/Cargo.toml`: `signal-hook = "0.3"`,
  `time = { version = "0.3", features = ["parsing"] }`

## Interfaces

- **Consumes:** everything from C07–C11.
- **Produces:**
  - `env::Env { session_id, job_token, app_url, deadline_ms: u64, drivers: Drivers }`
  - `env::parse(get: impl Fn(&str) -> Option<String>, release: bool) -> Result<Env, EnvError>`
    is a pure function and testable. `EnvError` maps to exit 78 and names only the missing key,
    never a value.
  - `ProcMetrics::new(stats: Arc<Stats>) -> Self`, implementing `Metrics`
  - the image `seazn-relay:<sha>`, entry `tini -- /entrypoint.sh`

## Steps

- [ ] **Step 1: Write the failing env tests** (`crates/compositor/tests/env.rs`):
  - `parses_r1_iso_deadline_with_millis`: `2026-10-14T18:30:00.000Z` gives `1_792_002_600_000`.
    Write the arithmetic in the test, never the parser's output: 2026-10-14 is day 20_740 since
    1970-01-01, so `20_740 × 86_400_000 + 18 × 3_600_000 + 30 × 60_000`. A second case,
    `2026-10-14T18:30:00.250Z`, must give `1_792_002_600_250`, which catches a parser that drops the
    fraction.
  - `missing_each_required_key_is_refused_naming_the_key`: a table over the 4 keys. Each error
    string contains the key and does **not** contain the other values (use a token value of
    `SECRET-TOKEN-VALUE`, and assert it's absent).
  - `drivers_unset_refuses_in_release_and_defaults_fake_in_debug`
  - `drivers_rejects_unknown_value`
  - ~~`drain_timeout_fits_inside_r1_stop_grace`:
    `Thresholds::default().drain_timeout_ms < R1_STOP_GRACE_MS`~~ SUPERSEDED 2026-10-15 by
    `final_hold_and_drain_fit_inside_r1_stop_grace` (amendment).

- [ ] **Step 2: Write the failing metrics tests** (`crates/adapters/tests/metrics_proc.rs`):
  - `first_sample_has_no_cpu`
  - `busy_loop_raises_cpu`: spin a thread for 300 ms between two samples, then assert `cpu_pct > 0`
  - `mem_is_positive`
  - `latency_is_never_reported`
  - `copies_every_stats_field_into_measured`: not Linux-gated. Build a `Stats`, set known values
    with `force_snapshot` (C09; enable `adapters/test-hooks` in `crates/adapters` dev-dependencies), call `ProcMetrics::sample()`, and assert `fps`, `bitrate_kbps`,
    `egress_bytes` and `encoder_speed` equal the snapshot. The same test with a fresh `Stats` asserts
    all four are `None`. This is the join between C09's counters and the heartbeat, so it's caught
    here, not in C14.

  These need Linux. Gate them with `#[cfg(target_os = "linux")]` and print `skipped: not linux`
  elsewhere. CI runs Linux, so the gate is not vacuous there; `check.yml` greps for the passed count.

- [ ] **Step 3: Write the failing fake-run test** (`crates/compositor/tests/fake_run.rs`).

  **Fake mode drains for real.** `wiring.rs` wraps testkit's `FakeProgramme` in
  `DrainingFake { inner, tx }` (in `crates/compositor/src/wiring.rs`). Its `drain()` delegates, then
  sends `Inbound::Event(Event::OutputDrained)` on `tx`. Without it, the supervisor would leave fake
  mode only via `drain_timeout_ms`, and this test would be judging the timeout, not the drain path.

  Spawn the
  built binary (`env!("CARGO_BIN_EXE_compositor")`) with these settings:
  - `COMPOSITOR_DRIVERS=fake`
  - a deadline 3 s in the future
  - a dummy token and URLs

  Assert all of:
  - exit code 0, no earlier than 2.5 s, and **sooner than** `3 s + Thresholds::default().drain_timeout_ms`
    (both from constants, never a flat number: AGENTS.md class 20). **Amended 2026-10-15:** the deadline
    now holds the Final scene first, so the bounds are `2.5 s + final_hold_ms` and
    `3 s + final_hold_ms + drain_timeout_ms`, both from `Thresholds::default()`. An exit through the drain
    timeout lands at the bound or later and goes red. That's the point of the bound: it proves
    `OutputDrained` ended the run.
  - stderr contains `programme started` and `exit code=0`
  - stderr never contains the token
  - a second run with a deadline 1 h in the past exits 0 within 1 s and logs **no**
    `programme started` (review focus 4, now end-to-end)

- [ ] **Step 3b: Write the failing fixture-equality test** in `crates/compositor/tests/fake_run.rs`:
  `fake_session_matches_the_vendored_fixture` asserts
  `testkit::fakes::session_720p() == adapters::control_http::parse_session(&<vendored session-composed-720p.json>).unwrap()`.
  The `compositor` crate may depend on both.
- [ ] **Step 4: Run, expect a FAIL.** `cargo test -p compositor -p adapters --test metrics_proc`

- [ ] **Step 5: Implement `env.rs`, `wiring.rs`, `main.rs` and `metrics_proc.rs`.**
  - Logging: plain `eprintln!` lines `ts=<rfc3339> level=<l> msg=<…>`. Every URL passes through
    `redact`. No logging crate is needed in R2 (YAGNI).
  - `main`: parse env → build `FrameStore`, `(tx, rx)` channels, adapters → spawn the control worker
    → spawn the signal thread → `Driver::new(Thresholds::default(), ports, env.deadline_ms, now_ms())`
    → `let code = run(driver, rx, &now_ms); eprintln!(… "exit code={code}"); std::process::exit(code)`.
  - `now_ms` is **monotonic**: `Instant::now()` minus one `Instant` captured at process start, in ms,
    plus one epoch offset captured once at the same moment (`SystemTime::now()` since `UNIX_EPOCH`).
    The domain needs both meanings, so they come from one function: frame ages, thresholds and
    backoff run on it, and because the offset is fixed at start it also stays comparable with the
    epoch `deadline_ms`. A wall-clock step (NTP or a VM resume) can never move `now` backwards. That
    matters, because a backward step makes a frame's age read 0: Reconnecting would go Live with no new
    frames, and a camera loss would be detected late (C03 review, `_INDEX`). This is the **only**
    clock read in the workspace.
  - Test: `now_ms_is_monotonic_across_a_wall_clock_step`. Inject the wall source, step it back 5 s,
    and assert that `now_ms` never decreases. Mutant: read `SystemTime` per call; the test goes RED.

- [ ] **Step 6: Run, expect a PASS.** Paste the counts.

- [ ] **Step 7: Dockerfile** (`docker/Dockerfile`). It's multi-stage, based on the C00 spike's
  working Dockerfile. **Copy the CEF and gstcefsrc pins from F-001 verbatim.**
  ```dockerfile
  # syntax=docker/dockerfile:1.7
  FROM ubuntu:24.04 AS gst-base
  ENV DEBIAN_FRONTEND=noninteractive
  RUN apt-get update && apt-get install -y --no-install-recommends \
        gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
        gstreamer1.0-plugins-ugly gstreamer1.0-libav libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 \
        xvfb tini ca-certificates fonts-dejavu-core libnss3 libatk-bridge2.0-0 libgbm1 libxkbcommon0 libasound2t64 \
      && rm -rf /var/lib/apt/lists/*

  FROM gst-base AS cef-build
  RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake git curl \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev && rm -rf /var/lib/apt/lists/*
  ARG GSTCEF_COMMIT   # from F-001 — no default; the build fails without it
  RUN test -n "$GSTCEF_COMMIT" && git clone https://github.com/centricular/gstcefsrc /src/gstcefsrc \
      && git -C /src/gstcefsrc checkout "$GSTCEF_COMMIT"
  RUN cmake -S /src/gstcefsrc -B /build -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release \
      && cmake --build /build -j"$(nproc)"

  FROM gst-base AS rust-build
  RUN apt-get update && apt-get install -y --no-install-recommends build-essential curl pkg-config \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev && rm -rf /var/lib/apt/lists/*
  RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.91.0
  ENV PATH=/root/.cargo/bin:$PATH
  WORKDIR /src
  COPY . .
  RUN cargo build --release -p compositor

  FROM gst-base
  COPY --from=cef-build /build/Release /opt/gstcef
  COPY --from=rust-build /src/target/release/compositor /usr/local/bin/compositor
  COPY docker/entrypoint.sh /entrypoint.sh
  ENV GST_PLUGIN_PATH=/opt/gstcef COMPOSITOR_DRIVERS=live DISPLAY=:99
  RUN useradd --system --uid 10001 --home /home/relay --create-home relay
  USER relay
  ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
  ```
  `docker/entrypoint.sh`:
  ```bash
  #!/usr/bin/env bash
  # Start Xvfb for cefsrc, then exec the compositor so it receives SIGINT directly from tini.
  set -euo pipefail
  Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp &
  for _ in $(seq 1 50); do [ -e /tmp/.X11-unix/X99 ] && break; sleep 0.1; done
  [ -e /tmp/.X11-unix/X99 ] || { echo "Xvfb did not start" >&2; exit 70; }
  exec /usr/local/bin/compositor
  ```
  If F-001 recorded different CEF output paths or extra runtime libs, follow F-001 and note the
  deviation.

- [ ] **Step 8: `image.yml`.** Triggers: `pull_request` and `push` to `main`. Steps:
  1. Build with `docker build --build-arg GSTCEF_COMMIT=<F-001 value> -t seazn-relay:${{ github.sha }} -f docker/Dockerfile .`
  2. **Plugin check:** `docker run --rm seazn-relay:$SHA gst-inspect-1.0 cefsrc`, which must exit 0.
  3. **cef tests:** run C11's graphics tests inside a build-stage image
     (`--target rust-build` plus the cef plugin path) with `cargo test -p adapters --features cef --test graphics -- --test-threads=1`.
     Fail if the passed count is < ~~7~~ 8 (amended 2026-10-15: C11's `phase_channel_signal_is_relayed`).
  4. **60 s smoke:** start the image with `COMPOSITOR_DRIVERS=fake`, a deadline 60 s ahead and the
     dummy env. Expect exit 0 within ~~75 s~~ ~~85 s~~ the **computed** budget (corrected 2026-10-15,
     review m4: a typed number goes stale the moment a threshold moves), and stderr containing
     `programme started` and `exit code=0`.
     - Create `crates/compositor/examples/smoke_budget.rs`. It prints one integer to stdout:
       `60_000 + t.final_hold_ms + t.drain_timeout_ms + 8_000`, with `t = Thresholds::default()`.
       The terms are the deadline, the final hold, the drain, and container start; each gets a
       comment naming its source.
     - It starts with `#![allow(clippy::print_stdout)] // prints the smoke budget for image.yml (C12 Step 8)`.
       Amend the workspace `Cargo.toml` lint comment, which today names one sanctioned
       `print_stdout` exception (C13's harness), to name this as the second.
     - `image.yml` runs it in the `rust-build` stage container
       (`cargo run -q -p compositor --example smoke_budget`), refuses an empty or non-numeric
       output, and uses `timeout $(( (BUDGET_MS + 999) / 1000 ))s` on the smoke. Print
       `smoke budget: <n> ms` in the log. At today's thresholds that is 85 000; the workflow never
       contains the number.
  5. Print the image size, and fail above 2.5 GB (CEF is large; the cap stops bloat creep).

  Push is C16's job; this workflow never pushes.

- [ ] **Step 9: Mutate once each.**
  1. Make unset `COMPOSITOR_DRIVERS` default to fake in release. Expected red:
     `drivers_unset_refuses_in_release…`.
  2. Include the token value in `EnvError`'s message. Expected red:
     `missing_each_required_key_is_refused_naming_the_key`.
  3. Drop the `encoder_speed` copy in `ProcMetrics::sample`. Expected red:
     `copies_every_stats_field_into_measured`.
  4–8. **(Amendment 2026-10-15)** mutants 4–8 from the amendment section.

- [ ] **Step 10: Commit and open the PR.** `feat(compositor): binary wiring, proc metrics and the relay image (C12)`

## Acceptance

- (Amended 2026-10-18) `graphics_log_lines_exist_in_both_driver_modes` passes in both modes.
- Env, metrics and fake-run tests pass.
- `image.yml` is green on the PR: plugin present, ~~7~~ 8 cef tests, smoke exits 0, size under the cap.
- ~~3~~ 8 mutants went red (amended 2026-10-15).
- (Amended 2026-10-15) The phase endpoint tests pass, and `final_hold_and_drain_fit_inside_r1_stop_grace`
  is green against the ruled 20 s grace (D15). It is never ignored or weakened to get there.
- The image runs as non-root.


## Final heartbeat with the exit reason (owner ruling 2026-10-15)

The main loop executes the supervisor's final `SendHeartbeat` before `std::process::exit`. Add to
the fake-mode smoke a run where fake-control serves `reply-ending.json`, then assert that the last POST
fake-control received has `state: "stopped"` and `exitReason: "ended"`. Add a second run with
`ProgrammeStartFailed` forced via the fake driver: exit 70, and the last POST carries
`exitReason: "fatal"` with a non-empty `exitDetail`.
Exits **before** a session exists (`EnvError`, exit 78 at config parse) cannot send a beat, because
they have no URL or token yet. They stay stderr-only, and that's expected, not a gap.

(Amended 2026-10-15) The Ended path now holds first: a fake-control `reply-ending.json` run lasts
`final_hold_ms` longer. Assert also that **an earlier POST carries `programmePhase: "final"`** (C07's
wire, owed by the plan's C07 note), so the Final scene reaches the heartbeat. A third run sends `SIGTERM`
to the child 2 s after `programme started`: exit 0 within `drain_timeout_ms + 1 s`, the last POST
carries `exitReason: "signalled"`, and **no** POST carries `final` (a host stop never shows "Stream
ended").

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.3 (D10; verbatim, `29fa9f59c`): "the compositor serves `GET http://127.0.0.1:<RELAY_PHASE_PORT>/phase`
on loopback inside the Machine. It returns `{ phase, seq }` and supports long-poll (`?after=<seq>`,
held ≤ 10 s)" and "It is never exposed outside the Machine and is not part of the outbound-only host
contract (ADR 0007)". §6, C08/C12 row: "Adds the loopback `/phase` endpoint (127.0.0.1 only; CORS + PNA
headers; long-poll)."

**Ownership split, stated in both briefs.** C08 owns the `PhasePublisher` port, the driver's
publish-on-change rule and the fake. **C12 owns the HTTP adapter**, its wiring in `main`, and the
stop-grace test. C11 Step 0's finding decides whether the page reads this endpoint first (outcomes
(a)/(b)) or only as a later upgrade (outcome (c)). The endpoint ships either way.

### Files

- Create: `crates/adapters/src/phase_http.rs`. Test: `crates/adapters/tests/phase_http.rs`.
- Modify: `crates/adapters/Cargo.toml`: move `tiny_http = "0.12"` from C07's `[dev-dependencies]`
  to `[dependencies]` (same version; one HTTP server crate in the workspace).
- Modify: `crates/compositor/src/{env.rs,wiring.rs,main.rs}`, and the tests above.

### Interface

```rust
// phase_http.rs
/// The loopback programme-phase endpoint (overlay templates spec 2026-10-15 §4.3). The page reads it;
/// Fly never exposes it. Values are drift-tested against the vendored contract's `$defs.phaseEndpoint`.
pub const RELAY_PHASE_PORT: u16 = 7419;
pub const RELAY_PHASE_PATH: &str = "/phase";
pub const RELAY_PHASE_LONG_POLL_MAX_MS: u64 = 10_000;
/// The page holds one poll; a CEF reload can briefly hold two. 4 bounds a stuck client; the 5th gets 503.
pub const PHASE_HTTP_MAX_CONCURRENT: usize = 4;

pub struct PhaseHttp { /* Arc<(Mutex<(ProgrammePhase, u64)>, Condvar)>, accept thread */ }
impl PhaseHttp {
    /// Refuses a non-loopback `addr` (`PhaseBindError::NotLoopback`) before binding. That's the guard
    /// for "127.0.0.1 only".
    pub fn bind(addr: SocketAddr, allowed_origin: String) -> Result<Self, PhaseBindError>;
    pub fn local_addr(&self) -> SocketAddr;
}
impl app::ports::PhasePublisher for PhaseHttp { fn publish(&mut self, phase: ProgrammePhase, seq: u64); }
```

It serves `(Waiting, 0)` until the first `publish`, which matches C08's driver start.

### Behaviour

| Request | Answer |
|---|---|
| `OPTIONS /phase`, `Origin` == `allowed_origin` | 204 with `Access-Control-Allow-Origin: <origin>`, `Access-Control-Allow-Methods: GET`, `Access-Control-Allow-Private-Network: true` and `Vary: Origin` |
| `GET /phase?after=S`, allowed `Origin`, current `seq` > S | 200 at once, `{"phase":<word>,"seq":<seq>}` |
| same, `seq` ≤ S | wait on the condvar until a publish raises `seq`, or `RELAY_PHASE_LONG_POLL_MAX_MS`, then 200 with the current value |
| any request whose `Origin` differs or is missing | 403, no `Access-Control-*` header |
| `after` missing or not a `u64` | 400 |
| another path | 404; another method → 405 |
| a 5th concurrent request | 503 |

- Every 200 and 204 carries `Cache-Control: no-store`.
- `allowed_origin` is `Env.app_origin`: APP_URL's scheme + host + port, no path and no trailing slash.
  That's the form C11 Step 0 observed in the `Origin` header; cite its F-id at the derivation.
  **(Amended 2026-10-16, C11 Step 0 = F-009, and its review on PR #20):**
  - **The observed form** is `https://<host>`: scheme + host, the default port omitted, no trailing slash. A
    non-default port stays. Serialize APP_URL exactly that way, and cite F-009.
  - **The check must match the page's real origin (review M6).** stg serves `/overlay` on two hosts,
    `stg.seazn.club` and `seazn-club-stg.fly.dev`; both answered on 2026-10-16. The page's `Origin` is the host
    its `overlayUrl` names. Web builds that URL's base from `captureOrigin()` = `OAUTH_BASE_URL ||
    NEXT_PUBLIC_BASE_URL || appUrl` (`seazn.club` `server/usecases/capture-phone.ts:97-99`, `c64e0a217`; re-pin
    R2004's composed `overlayUrl`). A single `allowed_origin` from a different base answers that page 403, and the
    page silently drops to fallback B. Pick one of these, record the choice and cite F-009:
    - (i) the check accepts both stg hosts;
    - (ii) C12 configures the allowed origin per environment, from the same base URL web uses.
  - A test owes the cross-check: the `Origin` of the `overlayUrl` the job facts carry equals `allowed_origin` (or
    is in the accepted set). C17's stg run is the live witness.
  - **`Access-Control-Allow-Private-Network: true` stays, but it future-proofs nothing.** CEF 139 sends the
    PNA preflight without enforcing its result (F-009 result 3). Chromium replaced PNA enforcement with Local
    Network Access, a permission that no response header satisfies (Owed 22). It's kept because it's harmless
    and the spec asks for it.
  - **No `Access-Control-Allow-Headers`, by design.** F-009 proved only a CORS-simple GET. R2005's hook must
    stay one (GET, no request headers; R2005 amendment 2026-10-16). A non-safelisted header would bring an
    enforced CORS preflight, and this table's 204, which carries no `Access-Control-Allow-Headers`, fails it.
- `<word>` comes from the **same** function C07's `heartbeat_wire` uses for `programmePhase`
  (`programme_phase_wire`, owed by the plan's C07 note). If C07 merged without that function, extract
  it in C07's module and record a one-function deviation in the commit body. Two mappings would drift.
- Wiring in `main`: bind `127.0.0.1:RELAY_PHASE_PORT` **before** the driver starts and before CEF loads
  the page, so the page's first poll can't race the bind. A bind failure logs
  `level=warn msg=phase endpoint unavailable` once and wires `NoopPhase`, and the run continues: the
  page falls back to B after 3 failures, and the run must never die for a graphics hint.

### Tests (write them failing first)

`crates/adapters/tests/phase_http.rs` binds `127.0.0.1:0` (an ephemeral port) for every test, so
tests run in parallel. Expected values come from the vendored contract or the spec, never from the
module.
1. `serves_the_published_phase_with_cors_for_the_app_origin`: `publish(Live, 1)`; `GET after=0` → 200;
   the body validates against the vendored `$defs.phaseReply` (C07's `schema_def` helper) and equals
   `{"phase":"live","seq":1}`; `Access-Control-Allow-Origin` is exactly the origin (not `*`); `Vary:
   Origin`; `Cache-Control: no-store`.
2. `preflight_allows_private_network_for_the_app_origin_only`, over 3 origins:
   - the allowed origin with `Access-Control-Request-Private-Network: true` → 204 carrying
     `Access-Control-Allow-Private-Network: true`
   - `https://evil.example` → 403 with no `Access-Control-*` header
   - no `Origin` → 403

   Print `origins checked: 3`.
3. `long_poll_waits_for_a_newer_seq_and_answers_at_once_when_behind`:
   - `publish(Live, 1)`. A `GET after=1` blocks; `publish(Frozen, 2)` 300 ms later answers it with
     `seq 2` in under 800 ms.
   - `GET after=0` answers in under 200 ms.
   - `GET after=2` with no publish answers no sooner than `RELAY_PHASE_LONG_POLL_MAX_MS − 100` and no
     later than `+ 1 000`, with `seq 2` unchanged.
4. `binds_loopback_only`: `bind("0.0.0.0:0")` → `Err(NotLoopback)`; `bind("127.0.0.1:0")` →
   `local_addr().ip()` is `127.0.0.1`.
5. `fifth_concurrent_poll_is_refused_503`: 4 parked polls, then a 5th → 503. Release one, and a new
   poll is accepted again.
6. `before_any_publish_serves_waiting_seq_0`.
7. `bad_requests_are_refused`: unknown path 404, `POST` 405, `after=abc` 400.
8. `endpoint_constants_match_the_vendored_contract`: port, path and long-poll max each equal
   `$defs.phaseEndpoint.properties.<field>.const`.
9. `phase_word_matches_the_heartbeat_wire_for_every_phase`: for every `ProgrammePhase` (an exhaustive
   `match` builds the list, so a new variant fails to compile here), the served word equals
   `heartbeat_wire`'s `programmePhase` and is a member of the vendored
   `$defs.phaseReply.properties.phase.enum`. Print `phases checked: 5`; fail on 0.

`crates/compositor/tests/env.rs`:
- `app_origin_is_scheme_host_port`: `https://seazn.club` → `https://seazn.club`;
  `https://stg.seazn.club/` → `https://stg.seazn.club`; `http://localhost:3000/x` →
  `http://localhost:3000`.
- `final_hold_and_drain_fit_inside_r1_stop_grace`, replacing the superseded test:
  `t.final_hold_ms + t.drain_timeout_ms + EXIT_HEADROOM_MS <= R1_STOP_GRACE_MS`, where
  `EXIT_HEADROOM_MS = 3_000` (the headroom in `config.rs`'s `drain_timeout_ms` doc line). With R1's grace
  at 10 s, this is **red** (10 + 7 + 3 = 20 > 10). It goes green with the ruled 20 s grace
  (owner ruling 2026-10-16, spec D15 (`seazn.club` PR #969, `9bf31b38a`)), which ships in R2004's PR 0 Part 2: set `R1_STOP_GRACE_MS` to `20_000`, citing D15 and the
  web commit that changed `RUNNER_STOP_GRACE_SECONDS`. This PR merges only after that web change is
  deployed to stg, because otherwise Fly kills mid-drain.

  Never `#[ignore]` it.

`crates/compositor/tests/fake_run.rs` (the tests hold a file-level `static SERIAL: Mutex<()>`, so two
spawned binaries never race for port 7419):
- `fake_run_serves_final_on_the_loopback_endpoint_at_the_deadline`. This is the seam, from the real
  driver through the real publisher to the real endpoint:
  - start the fake run with a 3 s deadline
  - loop `GET http://127.0.0.1:7419/phase?after=<last seq>` with `Origin: <the dummy APP_URL origin>`
  - assert that `final` arrives no sooner than 2.5 s, with every `seq` strictly rising, and that the
    process then exits 0
- `phase_port_taken_still_runs_to_exit_zero`: hold a `TcpListener` on `127.0.0.1:7419`, then run the
  3 s fake run. It exits 0, and stderr contains `phase endpoint unavailable` exactly once.

**Mutants:**

| # | Mutant | Expected red |
|---|---|---|
| 4 | wiring binds `0.0.0.0` (drop the `NotLoopback` guard) | 4 |
| 5 | `Access-Control-Allow-Origin: *` | 1, 2 |
| 6 | drop `Access-Control-Allow-Private-Network` | 2 |
| 7 | answer at once, never wait | 3 |
| 8 | a bind failure returns `Err` from `main` (exit 70) | `phase_port_taken_still_runs_to_exit_zero` |

## Amendment 2026-10-18 (the log lines C14 cases 8 and 11 read)

C14 cases 8 and 11 assert on the compositor's own log, not on a 5 s beat landing in a ~1–4 s
window (Owed 19; PR #21 review I3). So these lines must exist in both driver modes. They use the
existing `eprintln!` form, `ts=<rfc3339> level=info msg=<…>`.

- ~~`LoggedProgramme` (`logged.rs`) also logs `overlay hidden` on `hide_overlay()`.~~ (Amended
  2026-10-18, C05b PR #22 review I2.) The driver logs `overlay hidden reason=<hang|crash|failed>
  gen=<g>` when it applies `HideOverlay { reason, gen }` (C06, C08). `reason` and `gen` are the
  domain hide's own: `hang` for every hang, `crash` for a crash's hold that ran out, and `failed` for
  a crash or load timeout entering Failed. `gen` is the generation whose failure hid the overlay.
  `LoggedProgramme` logs nothing on `hide_overlay()`, so there is exactly one line per hide.
- A new `LoggedGraphics<G: GraphicsRenderer>` decorator, beside it, logs:
  - `graphics restart gen=<n>` on `restart`
  - `graphics stop` on `stop`
- The driver logs each graphics event it hands to `decide`:
  - `cef ready gen=<n>`
  - `cef crashed gen=<n>`
  - `cef alive gen=<n>`, one every 2 s, so a 60 s case has about 30
- New test, `graphics_log_lines_exist_in_both_driver_modes`. In each mode, it drives ready → alive
  → crash → restart → hide through the fakes, and asserts each line once in order. It prints the
  lines matched, and fails on 0. (Amended 2026-10-18, C05b PR #22 review I2.) It drives one hide of
  each reason: a hang, a crash whose reload never readies, and the count-th failure. It asserts
  `overlay hidden reason=hang gen=1`, `reason=crash gen=<the crashed generation>` and
  `reason=failed gen=<the count-th>`, each exactly once, and no bare `overlay hidden`.
- ~~(Amended 2026-10-18, C05b) **No line names a hang, by design.** C05b's hang returns the shared
  `GraphicsAction::Hide` (its brief: no new action), so C06 emits the same `HideOverlay`, and
  `LoggedProgramme` logs the same `overlay hidden`. A reader identifies a hang from the lines above
  alone: an `overlay hidden` whose current generation `g` (the last `graphics restart gen=g`) logged
  `cef ready gen=g` and no `cef crashed gen=g` before the hide, `cef_hang_after_ms` after its last
  `cef alive gen=g` (or its ready, if it never beat), within one tick (C05b's
  `a_page_that_stops_beating_hides_on_the_frame_tick_grid`). A load timeout at the count also hides
  with no crash line, but its generation never logged `cef ready`. C14 case 11, C15 check (b) and
  the C17 runbook read this chain (`docs/_INDEX.md` false-premise row "C15's soak check (b) has a
  hang line to match").~~ **Superseded the same day (PR #22 review I2; coordinator decision,
  relayed).** The chain needed millisecond timestamps that nothing pins (`ts=` is wall-clock RFC 3339
  of unstated precision, while the domain's clock is monotonic). It also misread a dropped line: C15
  (a) itself tolerates 5 % missing `cef alive` lines. And three readers each rebuilt it. The hide
  now names its reason, and C14 case 11, C15 (b) and the C17 runbook match the single
  `overlay hidden reason=<r> gen=<g>` line.

## Do not touch

Domain and app crates (except to read them), and fly configs (C16). (Amended 2026-10-15) No Fly
service or port config for 7419: it is loopback-only and never exposed.

## Output cap

Under 15 lines: counts, image size, cold start observed in the smoke, deviations.
