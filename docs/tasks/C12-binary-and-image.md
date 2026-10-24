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

  Push is C16's job; ~~this workflow never pushes.~~ this workflow never pushes an image (amended
  2026-10-23: its `cache` job writes BuildKit's layer cache, never a runnable image, to GHCR on a push
  to main or a manual run; `_INDEX.md` decision log, "CI: CEF layer from GHCR").

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
- (Amended 2026-10-19, Owed 35) Both driver modes wire a stderr `LogSink` (`ts=<rfc3339> level=info
  msg=<line>`) into `Ports.log`, the log-line test reads the driver's own lines through it, and a final
  beat whose call returned an error is logged with its scrubbed text, never the token.
- Env, metrics and fake-run tests pass.
- `image.yml` is green on the PR: plugin present, ~~7~~ 8 cef tests, smoke exits 0, size under the cap.
- ~~3~~ 8 mutants went red (amended 2026-10-15).
- (Amended 2026-10-15) The phase endpoint tests pass, and `final_hold_and_drain_fit_inside_r1_stop_grace`
  is green against the ruled 20 s grace (D15). It is never ignored or weakened to get there.
- The image runs as non-root.
- (Amended 2026-10-20, C11b) Exit 75 reaches the process's exit status in the fake-mode smoke, its last POST ~~`fatal`~~
  `restart` (amended 2026-10-20, Owed 66: web's #1000, contract re-vendored at `dec7b95d9`) with a `restart in place: `
  `exitDetail`; the `cef health` line is in
  `graphics_log_lines_exist_in_both_driver_modes`, once per mode.
- (Amended 2026-10-20, Owed 68) RLIMIT_CORE is ~~0~~ 1 byte, soft and hard (amended 2026-10-23, the PR #36 final
  review's m4: the round-1 amendment below superseded 0; the probe demands 1, and Fly measured 1 1 at `341ae7a`, Owed 76)
  for the compositor and its CEF children, read from `/proc/<pid>/limits` in the image test.
- (Amended 2026-10-23, Owed 73, the PR #36 final review's I-1) No `JOB_TOKEN` at any CEF child's exec, witnessed by
  `scripts/cef_env_witness.sh` in the probe's one witness container (never in the image): it ran (N > 0), every live
  CEF process came through it, `DISPLAY`, `SESSION_ID` and `CEF_SUBPROCESS_PATH` set N of N, `JOB_TOKEN` unset N of N.
  Mutant 29 (`remove_var` deleted) goes red on it (#46, Owed 75). Xvfb's environment check carries
  the control `SESSION_ID`.
- (Amended 2026-10-23, the PR #36 final review's m1) `supervise_test.py`'s 500 races draw 400 INTs from +0 to +3 ms
  after the child's exit and 100 from ±15 ms; mutant 37 (round 1's trapped `wait`) goes red on the races 3 runs of 3.
- (Amended 2026-10-24, Owed 85) After a restart in place that keeps the container's writable layer (Docker's
  `--restart on-failure:2`, ADR 0007 item 5), CEF is ready again in the new life after each of 75, 133 and 137, with
  exactly one Xvfb and the X lock naming it: image.yml's in-place restart probe (`image_probe.py --restarts 6`, each
  kind twice, 0 checked is a failure). `start_xvfb` (`docker/xvfb.sh`) is ready only on Xvfb's own `-displayfd` line
  and fails at once (70) on an Xvfb that exits first (`scripts/xvfb_start_test.py`, check.yml).


## Final heartbeat with the exit reason (owner ruling 2026-10-15)

The main loop executes the supervisor's final `SendHeartbeat` before `std::process::exit` (amended
2026-10-18, wave gate 1 review I4: `run()` returns only after C08's bounded flush, so `main` exits
at most 2 s after `Exit`, never before the beat's one attempt; see that amendment). Add to
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
  2026-10-18, C05b PR #22 review I2; `recycle` added in its second round.) The driver logs
  `overlay hidden reason=<hang|crash|recycle|failed> gen=<g>` when it applies
  `HideOverlay { reason, gen }` (C06, C08). `reason` and `gen` are the domain hide's own. The first
  three are a frame reaching its bound, `cef_hang_after_ms` after its page's last beat (C05b's one
  bound): `hang` for the Ready page's own, `crash` for a crashed page's held frame, and `recycle` for
  a recycled page's. `failed` is a crash or load timeout entering Failed. `gen` is the page whose
  frame came off (`failed`: the generation that failed). `LoggedProgramme` logs nothing on
  `hide_overlay()`, so there is exactly one line per hide.
- A new `LoggedGraphics<G: GraphicsRenderer>` decorator, beside it, logs:
  - `graphics restart gen=<n>` on `restart`
  - `graphics stop` on `stop`
- The driver logs each graphics event it hands to `decide`:
  - `cef ready gen=<n>`
  - `cef crashed gen=<n>`
  - `cef alive gen=<n>`, one every 2 s, so a 60 s case has about 30
- New test, `graphics_log_lines_exist_in_both_driver_modes`. In each mode, it drives ready → alive
  → crash → restart → hide through the fakes, and asserts each line once in order. It prints the
  lines matched, and fails on 0. (Amended 2026-10-18, C05b PR #22 review I2 and its second round.)
  It drives one hide of each reason: a hang, a crash whose reload never readies, a recycle whose
  reload never readies (with a short `cef_recycle_every_ms`), and the count-th failure. It asserts
  `overlay hidden reason=hang gen=1`, `reason=crash gen=<the crashed generation>`,
  `reason=recycle gen=<the recycled generation>` and `reason=failed gen=<the count-th>`, each
  exactly once, and no bare `overlay hidden`.
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

## Amendment 2026-10-18 (wave gate 1 review of C06, PR #24; the coordinator's decisions)

**The final beat's flush, reconciled with C07** (review I4; `docs/_INDEX.md` Owed 28). This brief's
"executes the supervisor's final `SendHeartbeat` before `std::process::exit`" and C07's "best-effort
… never delays `Exit`" disagreed, and as briefed neither held: C08 dropped a final beat behind a slow
one, and `main` called `process::exit` as soon as `run()` returned (Step 5). The ruling: C08 puts the
final beat on a never-dropped slot, and `run()` returns only once it was posted or C07's 2 s timeout
passed. So `main` needs no wait of its own, the beat always gets its one attempt, and `Exit` is
delayed by at most 2 s. That fits D15's 20 s grace: the hold (10 s) and the drain (7 s) leave 3 s,
and a drain's own final beat goes out when the drain starts, so only a fatal error during the drain
adds a flush there (10 + 7 + 2 = 19 s).
- The fatal smoke's "non-empty `exitDetail`" now has a producer (Owed 27, option a): C08 maps the
  start error's text into `Event::Fatal { detail }`, C06 carries it to the final beat, and C07
  redacts and caps it.
- **New fake-mode smoke runs:**
  - The fatal run with fake-control's heartbeat slowed (it answers each POST after 1.5 s), so the
    periodic slot is full when the start fails: the last POST still carries `exitReason: "fatal"`
    and a non-empty `exitDetail`, and the process exits 70 (C08's
    `a_hung_final_beat_delays_exit_at_most_2_s` pins the bound).
  - A run where fake-control answers a heartbeat with `410 SESSION_ENDED` (review I5): the process
    exits 0 within 1 s of that reply, and fake-control receives no POST after it.

## Amendment 2026-10-19 (C07 PR #25 review: the job token and the fallible plane)

- **`JOB_TOKEN` is read from env, never argv** (C07 review gap hunt; `docs/_INDEX.md` Owed 32). No
  production code reads it yet.
- `HttpControlPlane::new(app_url, session_id, job_token, timeout_ms)` returns
  `Result<Self, PlaneConfigError>`. It refuses an empty token, a token with a byte outside visible
  ASCII (a CRLF `.env` line, a pasted secret) and a 0 ms timeout, and no variant carries the token.
  The `live` mode handles the refusal before the event loop starts. It is a misconfigured Machine,
  never a transient. It exits **78** (`EX_CONFIG`, coordinator 2026-10-19), sending no beat (as at
  :285); web's runner sees the exit-78 refusal and tears the Machine down (runner.ts:254-258). Test:
  a `JOB_TOKEN` ending in CR exits 78, and stderr carries no token byte; mutant: exit 70 goes red.
  An empty `JOB_TOKEN` takes the same path (`PlaneConfigError::EmptyToken`).
- The fatal text is a `domain::health::ExitDetail`. Its `Debug` prints the length alone. Never log
  `ExitDetail::as_str` raw. (Amended 2026-10-19, the coordinator, C07 PR #25 confirm pass) A fatal
  text logged to stderr goes through a `Scrubber` built from the `SessionConfig` (its credentials,
  by value) AND `redact` (the patterns), never `redact` alone: a stream key in a GStreamer error
  follows no marker, and `redact` cannot see it.
  - Build it as the plane does: `adapters::redact::Scrubber::default()`, `add` the job token, and
    once the session is fetched, `add` `input.stream_id`, `input.passphrase`, `target.stream_key`
    and the overlay URL's `st=` (the contract's `pageToken`; it is not a `SessionConfig` field).
    C07's `HttpControlPlane::learn` holds the same list. Rather than copy it, C12 may lift it into a
    `pub` `Scrubber` method that the plane calls too, so the two lists cannot drift.
  - `Scrubber::add` ignores a value shorter than 8 characters (the coordinator's minimum); every
    credential is longer, and `redact`'s patterns still apply.
  - **Before the session is fetched**, only `redact` plus the token scrub apply: no session
    credential exists yet to scrub.
  - The fatal smoke run plants a **marker-less** stream key (no `/live/`, no `?`, at least 8
    characters) in fake-control's session, forces a `ProgrammeStartFailed` whose text names that key
    bare, and asserts the key is absent from stderr and from the final beat's `exitDetail`. It also
    asserts that the forced text did name the key, so the check cannot pass on a text without it
    (Owed 32).

## Amendment 2026-10-19 (C08 as built: the log port, a failed final beat)

`docs/_INDEX.md` Owed 35; decision row "C08, as built".

- **The driver logs through a port.** The app crate reads no clock and never names the adapters
  crate, so it cannot `eprintln!` the `ts=` form itself. C08 added `app::ports::LogSink`
  (`fn info(&mut self, line: &str)`) as `Ports.log`. The driver writes the bare message:
  `overlay hidden reason=<r> gen=<g>` (before `hide_overlay()`), `cef ready gen=<n>`,
  `cef crashed gen=<n>` and `cef alive gen=<n>` (before it hands the event to `decide`, a stale one
  included), `ticks skipped total=<n>` and `heartbeats dropped total=<n>` (at most one line a minute
  each, and the rest at the exit; C08 review round 1), and `final beat not confirmed within 2000 ms`
  (when `run()`'s flush times out or a final beat found its lane gone). C12 wires one stderr `LogSink` in both driver modes that writes
  `ts=<rfc3339> level=info msg=<line>`; `graphics_log_lines_exist_in_both_driver_modes` reads the
  driver's lines through it. `LoggedProgramme` still logs nothing on `hide_overlay()`.
- **A failed final beat is logged by the binary.** C07's final-heartbeat section says its failure is
  logged (redacted), but nothing produces that line: `HttpControlPlane` logs nothing, and C08's worker
  keeps no result's text. C12 logs a final beat whose call returned an error, with its text (C07 has
  already scrubbed it), for example through a `ControlPlane` decorator around `HttpControlPlane`
  beside `LoggedProgramme`. Never the job token, never `ExitDetail::as_str` raw. (C08 review round 1)
  Final beats run on the instance `ControlPlane::final_lane()` returns, so such a decorator's own
  `final_lane()` must wrap the inner plane's lane in the decorator, or no final beat passes through it.
- **The inbound channel is C12's to bound** (AGENTS.md §4). The control worker's `control` thread
  posts each fetch and periodic result with a blocking `send`, so a full channel holds that thread up.
  Its final lane never blocks on the channel: it takes the next final beat while a result waits for
  room, so neither a final beat nor `flush` is held up by a full channel (corrected by C08 review
  round 1; this said the final-beat ack alone kept `flush` free, which was false, since a final beat
  queued behind a periodic result blocked on the channel). A drop-oldest channel must never drop a
  worker result (a `Session`, a refusal, a 410). Record the choice in this brief.

## Amendment 2026-10-19 (C10 and C11 owe C12: pointers)

These items are owed by C12 and are written in full in `docs/_INDEX.md` Owed (the PR #32 confirm
pass asked for these pointers). Read each row there before building the composition root and the
image:
- **Owed 51:** ~~build `GstInput` from the Driver's own `Thresholds` (one value passed to both),
  and~~ pass `Thresholds::cef_rss_cap_mb` and the programme profile's fps to `CefGraphics::new`.
  (Amended 2026-10-20, C10b: the input adapter keeps no copy of the domain's rules, so only its SRT
  latency is read from the `Thresholds` it is given; passing the driver's value is still the
  natural wiring, but no mismatch can give a generation up.)
- **Owed 52:** never set `GST_CEF_LOG_SEVERITY` in the image or the environment: it overrides the
  element's `log-severity=disable`, and CEF then writes every console line, the job token included,
  to `chrome_debug.log`.
- **Owed 53:** install C11's log handler first (build the graphics adapter before the other
  adapters build pipelines, or install the handler at start), and keep `gst-launch` off tokened
  URLs.
- **Owed 54:** the CEF user's HOME exists and is writable (GStreamer's registry and NSS's database
  are still there), and only one CEF uses it.
- **Owed 58:** `TMPDIR` on a tmpfs for the compositor, nothing else writing there, and a short path
  (85 characters killed CEF at its start in a test, 59 ran). The adapter fails closed: a `TMPDIR` it
  cannot write is no overlay at all, never a profile under HOME.

## Amendment 2026-10-20 (C11b: the in-place restart's exit, and no core file)

- **Exit 75** (C11b, PR #34): `run()` returns 75 (`ExitReason::Restart`, sysexits' EX_TEMPFAIL) when CEF's health
  asks for an in-place restart, after the final beat (`exitReason: "fatal"` until Owed 66, `exitDetail` starting
  `restart in place: `). `main` exits with it, and tini passes it through, so the host's `on-failure` restarts the
  process. Add a fake-mode smoke run: the fake graphics driver reports a dead zygote after `programme started`, and
  the run exits 75 with that last POST.
- **The log port gains a driver line** (C08 as amended): `cef health zygote_gone=<gen|-> leaked_teardowns=<n>
  gpu_process_deaths=<n>`, on a change. `graphics_log_lines_exist_in_both_driver_modes` asserts it once in each mode,
  driven by the fake graphics health.
- **Owed 68: no core file** (the final C11 review's G1). Chromium's abort (a dead GPU zygote's next GPU death, the 6th
  GPU death, C14 case 8's `pkill`) is SIGTRAP with a core: in the rig, with `ulimit -c` unlimited and `core_pattern`
  `core`, a 93–117 MB core of the whole process landed in the cwd, the page token in it 46 and 61 times (and the
  stream key and control token live in the same heap). The compositor runs with **RLIMIT_CORE 0**, inherited by CEF:
  `setrlimit(RLIMIT_CORE, 0, 0)` first thing in `main` (or `ulimit -c 0` in the entrypoint before `exec`). An image
  test reads `/proc/<compositor pid>/limits` and a CEF child's: `Max core file size 0 0`. Fly's own `core_pattern`
  and RLIMIT_CORE are unmeasured, so the limit is ours either way.

## Amendment 2026-10-20 (C12 as built; Owed 66's compositor follow-up; the owner's rulings)

`docs/_INDEX.md` decision rows "C12, as built" and "Owner rulings 2026-10-20" hold the detail.

- **Exit 75's final beat says `restart`** (Owed 66, compositor follow-up). Web's #1000 (`dec7b95d9`) added the word to
  the strict beat enum; the contract is re-vendored at that commit (`relay-job.v1.json` sha `e6568677…a06a23`, plus
  `heartbeat-final-restart.json`), `exit_reason_wire` maps `ExitReason::Restart` to `restart`, and the contract sweep's
  `restart` row and `the_final_beat_under_a_410_is_gone` moved with it. The exit-75 smoke asserts the vendored
  fixture's `state`, `exitReason` and `exitDetail` on the last POST.
- **The inbound channel** (C08 amendment: "record the choice in this brief"): bounded at 1 024
  (`wiring::INBOUND_CAPACITY`), never drop-oldest. The control worker and the signal thread `send` (blocking), so no
  worker result is lost; every adapter and the fake drain `try_send` (each counts or logs a drop), so the loop never
  blocks on its own channel.
- **"fake-control" is this task's own server**, in `tests/fake_run.rs` (tiny_http): C13's harness is not built. The
  fake mode takes three knobs, read only when `COMPOSITOR_DRIVERS=fake`: `COMPOSITOR_FAKE_CONTROL=http` (the real
  `HttpControlPlane` against `APP_URL`; the scripted plane otherwise), `COMPOSITOR_FAKE_START_FAIL=<text>` and
  `COMPOSITOR_FAKE_ZYGOTE_GONE=1`.
- **`GstInput` is built lazily** (false premise): its frame size is the session's profile, so `LazyInput` builds it
  at the first rebuild after a successful `start` and refuses an earlier rebuild with a warn line.
- **The phase endpoint's origin is the page's** (F-009; the PR #20 review's M6, option (ii) taken per page): it starts
  as APP_URL's origin and moves to the overlay URL's origin before each graphics restart (`PageOrigin`).
- **`R1_STOP_GRACE_MS` and `EXIT_HEADROOM_MS` live in `crates/compositor/src/lib.rs`**, not `main.rs`: a test cannot
  import from a binary.
- **The Dockerfile follows F-001 and the C00 spike**, not this brief's sketch: the runtime and build library lists,
  `strip` of `libcef.so`, the pin in `docker/GSTCEF_COMMIT` (image.yml reads it; a trailing `# from F-001` on the
  `ARG` line would be part of the instruction), a `cef-test` stage running C11's tests as uid 10001, and a
  `gst-inspect-1.0` check of every element factory the adapters make.
- **The image test of Owed 58 and 68** is `scripts/image_probe.py`: live-mode runs against a fake control plane on
  the runner, reading `/proc/<pid>/limits` of the compositor and every CEF child, `TMPDIR` (`/dev/shm/seazn`, 0700,
  tmpfs, the CEF profile inside) and the environment (no `GST_CEF_LOG_SEVERITY`, no `GST_DEBUG`), then three stops,
  one container each: `docker kill -s INT` (the Final hold, exit 0 inside R1's grace, an earlier POST carrying
  `final`, the last POST `signalled`), `docker stop` (the drain, exit 0, the last POST `signalled`, no `final`), and a
  measurement of a SIGTERM sent straight to the compositor; no token in any log.
- **The entrypoint does not `exec` the compositor** (false premise; F-016, medium). Once CEF starts in the process,
  a SIGINT or SIGTERM it receives exits it with status 0 at once: no Final scene, no drain, no final beat (image.yml's
  first probe: `docker stop` exited in 0.1 s with one periodic POST). The entrypoint runs the compositor as its child
  and forwards SIGINT as SIGUSR1 and SIGTERM as SIGUSR2, which CEF leaves alone; `wiring::STOP_SIGNALS` reads both
  pairs (the binary run alone keeps SIGINT and SIGTERM), and the child's own exit status is the entrypoint's.
  `the_entrypoint_forwards_each_stop_signal_as_the_binary_reads_it` pins the two sides against each other; the fake-run
  stop tests drive each signal by both names.
- **The hand mutants ran on a real runner** (Owed 75): the owner stopped local VMs on 2026-10-20 (relayed by the
  coordinator), so each batch ran in check.yml (ubuntu-24.04, GStreamer) on a throwaway draft PR from PR #36's head
  (#37: 19 mutants, #38: 5; expected-red sets disjoint), each closed and deleted once read. **23 of 24 killed; 22 is
  equivalent:** `Logger` scrubs every line through `Scrubber::scrub`, which ends in `redact`, so the swap line's own
  `redact` is a second guard nothing can kill alone. Mutant 16 was also witnessed before the wire fix. Mutants 25
  (`STOP_SIGNALS` reads SIGUSR1 as nothing) and 26 (the entrypoint forwards SIGTERM as itself) are on draft PRs #39 and
  #40, not yet read. The list (expected red in brackets):

  | # | Mutant | Expected red |
  |---|---|---|
  | 1 | unset `COMPOSITOR_DRIVERS` defaults to fake in release (`env.rs`) | `drivers_unset_refuses_in_release…` |
  | 2 | `EnvError::Missing` carries the token (a leaked `format!`) | `missing_each_required_key_is_refused_naming_the_key` |
  | 3 | `encoder_speed: None` in `ProcMetrics::sample` | `copies_every_stats_field_into_measured` |
  | 4 | the `NotLoopback` guard off (`if false && …`) | `binds_loopback_only` |
  | 5 | `Access-Control-Allow-Origin: *` | `serves_the_published_phase…`, `preflight_allows_private_network…` |
  | 6 | the private-network header dropped | `preflight_allows_private_network…` |
  | 7 | `wait_past` answers at once | `long_poll_waits_for_a_newer_seq…` |
  | 8 | `phase_endpoint`'s bind failure exits 70 | `phase_port_taken_still_runs_to_exit_zero` |
  | 9 | no `setrlimit` in `main` | `the_compositor_runs_with_no_core_file_limit` (now `the_compositor_runs_non_dumpable_with_a_one_byte_core_limit`) |
  | 10 | `Clock::now_ms` reads the wall per call | `now_ms_is_monotonic_across_a_wall_clock_step` |
  | 11 | `LazyInput` builds at 720p with no started profile | `lazy_input_is_built_once_from_the_started_profile` |
  | 12 | `PageOrigin` never moves the origin | `the_phase_endpoint_answers_the_overlay_urls_origin` |
  | 13 | `LoggedPlane::final_lane` returns the inner lane | `a_failed_final_beat_is_logged_scrubbed_on_the_final_lane` |
  | 14 | the plane's refusal exits 70 | `a_job_token_with_a_cr_exits_78_and_never_prints_it` |
  | 15 | `LoggedPlane` never learns the session | `start_failure_exits_70_with_the_detail_scrubbed_of_a_bare_key` |
  | 16 | `Restart` on the wire as `fatal` | `the_restart_beat_is_the_vendored_restart_fixture`, `the_final_beat_under_a_410_is_gone`, `a_dead_zygote_exits_75_posting_restart` (witnessed) |
  | 17 | `DrainingFake` never queues `OutputDrained` | `fake_mode_drain_reports_output_drained`, `fake_run_exits_zero_through_the_drain…` |
  | 18 | no CR/LF escape in `Logger::line` | `log_lines_are_rfc3339_ms_one_per_message_and_masked` |
  | 19 | `add_session` drops the page token | `add_session_learns_all_four_credentials` |
  | 20 | an empty value is not missing | `missing_each_required_key…` |
  | 21 | `ProfileTap` records a failed start | `lazy_input_is_built_once_from_the_started_profile` |
  | 22 | the swap line's URL unredacted | `logged_programme_logs_start_swap…`, `graphics_log_lines_exist_in_both_driver_modes` |
  | 23 | a 5th poll admitted | `fifth_concurrent_poll_is_refused_503` |
  | 24 | the first sample's CPU is `Some(0.0)` | `linux::first_sample_has_no_cpu` |
  | 25 | `STOP_SIGNALS`' SIGUSR1 row reads SIGINT | `the_entrypoint_forwards_each_stop_signal…`, `sigint_holds_the_final_scene_then_drains` (killed on both, #39) |
  | 26 | the entrypoint forwards SIGTERM as SIGTERM | `the_entrypoint_forwards_each_stop_signal…` (#40: killed on its parse precondition only, the marker on the trap line; re-run on #43, the review round) |
- **The owner's rulings of 2026-10-20** change no C12 code: the exit-based restart stays (C14 measures its outage),
  leaked teardowns below 3 stay until the session ends, the outage target is 15 s, and web caps self-restarts at 6 a
  session.

## Amendment 2026-10-20 (the PR #36 review, round 1: the exit, the core, the stop supervision)

The review (Needs fixes: C1, I1–I3, m1–m9) and the coordinator's Owed 73 probe on a Fly Machine. `docs/_INDEX.md`'s
decision row "C12 review round 1" holds the choices; `_FINDINGS.md` F-016 (Fly's signals), F-017 and F-018 the facts.

- **`main` ends through `hard_exit::now`, never `std::process::exit`** (C1, F-017). libc's `exit(3)` ran libcef's and
  GLib's exit handlers against the teardowns the adapters' drops start and never join: after a clean `exit code=0`
  line, 1 of 6 live exits on the runner and 6 of 12 stops on a Fly Machine aborted 134 (`double free or corruption`).
  `crates/hard-exit` (`_exit(2)` after flushing stderr) is the workspace's one sanctioned `unsafe`; its own test
  shows it runs no `atexit` handler. An env failure exits through it too (78).
- **No core under any `core_pattern`** (I1, F-018; supersedes "RLIMIT_CORE 0" in the amendment above and its "the
  limit is ours either way"). A pipe ignores the limit, so 0 stopped nothing on the runner. In order, first in
  `main`: the stop signals' handlers (`wiring::arm_signals`, m1), `PR_SET_DUMPABLE 0`, then RLIMIT_CORE 1 byte soft
  and hard (`CORE_LIMIT_BYTES`), which CEF inherits (the dumpable flag does not survive `execve`; the kernel refuses
  a piped core at exactly 1, and 1 is below a file core's ELF minimum). Fly's pattern is a file, where 0 already
  held (measured); 1 there is the post-fix probe's to confirm. `the_compositor_runs_non_dumpable_with_a_one_byte_core_limit`
  replaces `the_compositor_runs_with_no_core_file_limit`.
- **`JOB_TOKEN` leaves the compositor's environment once parsed** (m2): edition 2021's `remove_var` is safe, and no
  thread exists yet. Xvfb starts without it (`env -u`).
- **The stop forwarding moved to `docker/supervise.sh`** (I2, m1, m6), sourced as the entrypoint's first command and
  installed at `/usr/local/lib/seazn/supervise.sh`. It traps INT and TERM before Xvfb starts; a stop that arrives
  before the compositor exists, or before both USR handlers show in its `/proc/<pid>/status`, is held and then
  forwarded; the loop keeps the child's own status, also when the child dies of the forwarded signal; a set
  `TINI_KILL_PROCESS_GROUP` exits 78 before anything runs. `scripts/supervise_test.py` drives it with a stub child
  (check.yml, the image's bash, `--require-procfs`). The entrypoint sets core 1:1 on itself and tini (`prlimit`).
- **`tini -s`** (m5): Fly runs tini as a child of `/fly/init`, and tini printed its warning on every boot.
- **The probe** (`scripts/image_probe.py`, m3, m7, C1, I1): every process's core limit against the kernel's 1, the
  compositor non-dumpable, `core_pattern` printed, all five session credentials against every command line and log,
  `JOB_TOKEN` against Xvfb's and each CEF child's environment, a forced CEF-child abort and a forced compositor
  abort (134, no `(core dumped)`), 32 live exits with CEF up at 720p and 1080p (24 stops), each exact with no abort
  text, and a SIGTERM and a SIGINT 0.3 s into a boot. image.yml refuses `TINI_KILL_PROCESS_GROUP` in the image's env.
- **BUILDINFO's `cef_dir`** names the extracted directory (m8). **m4:** "91a5123" was the merge ref's image tag;
  the citations now name `c080c3f` and its runs. **m9:** mutant 26 is re-run with its marker on its own line.
- **Owed 73's Fly plan** (I3) checks each stop's drain lines and the `signalled` beat before the exit line, not the
  exit code alone (F-016's broken case also exited 0). The coordinator runs it; C1 stays open until it shows 0 aborts
  (Owed 76).
- **New hand mutants** (Owed 75), on throwaway draft PRs from this round's head:

  | # | Mutant | Expected red | PR |
  |---|---|---|---|
  | 27 | `main` exits through `std::process::exit` | phase B: a 134 or abort text in some of 32 exits (probabilistic) | #41 |
  | 28 | no `PR_SET_DUMPABLE` | the compositor's `/proc/<pid>` is not root-owned | #41 |
  | 29 | `JOB_TOKEN` stays in the environment | ~~each CEF child's environment check~~ (it SURVIVED that, F-020) the exec-time witness: "no JOB_TOKEN at any CEF child's exec" (amended 2026-10-23) | #41, #44, #46 |
  | 30 | `CORE_LIMIT_BYTES` 0 | the compositor's and each CEF child's limit; the CEF child's kernel refusal line | #42 |
  | 31 | no `prlimit` in the entrypoint | tini's, bash's and Xvfb's limit | #42 |
  | 32 | Xvfb keeps `JOB_TOKEN` | Xvfb's environment check | #42 |
  | 33 | no arming wait in `supervise.sh` | "a stop before the child's handlers is held" | #43 |
  | 34 | no pending flush | "a stop before the child exists is held" | #43 |
  | 35 | `c080c3f`'s wait loop | "the child dies of the forwarded USR2" (survived on macOS bash 3.2 and 5.2) | #43 |
  | 36 | no `TINI_KILL_PROCESS_GROUP` refusal | the refusal case (killed on the host) | #43 |
  | — | `hard_exit::now` made `std::process::exit` | `hard-exit`'s `exit.rs` (killed on the host) | — |
- **Round 1's CI reds at `6b077fb`, fixed 2026-10-23** (`docs/_INDEX.md`'s row of that date; F-019):
  `supervise` now runs the compositor under a waiter subshell that writes its pid and exit status to files in a private
  `/tmp` directory, and never `wait`s on it from the trapping shell (2 of 150 races lost the status on the runner). The
  probe and the binary test stat `/proc/<pid>/status`, not the directory (which keeps the euid), each against a dumpable
  control; the probe reads environments with `docker exec --privileged` (CAP_SYS_PTRACE, which Docker's default set
  lacks) and fails an unreadable one. check.yml's supervision step runs 500 races, last.

## Amendment 2026-10-23 (the PR #36 final review: the CEF children's environment at exec, the race draw)

The final review at `341ae7a` (Needs fixes: I-1, m1–m8) and the coordinator's Owed 76 Fly probe at the same head.
`docs/_INDEX.md`'s decision row "C12 final review fixes" holds the choices; `_FINDINGS.md` F-020 the fact.

- **The CEF children's `JOB_TOKEN` check is witnessed at exec** (I-1, F-020). Chromium rewrites a child's
  `/proc/<pid>/environ` for its process title, so the old check passed with mutant 29 (`remove_var` deleted) in place.
  The probe now runs one dedicated container with `scripts/cef_env_witness.sh` bind-mounted and gstcefsrc's
  `GST_CEF_SUBPROCESS_PATH` pointing at it (the knob exists at the F-001 pin: gstcefsrc.cc lines 852-856). For every
  child CEF execs, the witness records whether `JOB_TOKEN`, `DISPLAY`, `SESSION_ID` and `CEF_SUBPROCESS_PATH` are set
  (never a value), then execs the real gstcefsubprocess. The probe requires N > 0 records, every live CEF process
  descended from a recorded exec (the zygotes' forks included), the three controls set N of N and `JOB_TOKEN` unset N
  of N. The witness is never in the image (no Dockerfile change). Xvfb's environment check now needs the control
  `SESSION_ID` in the file, so padding cannot pass it.
- **The races** (m1): 400 of the 500 INTs land +0 to +3 ms after the child's exit (every printed loss was at 0.301 s),
  100 at ±15 ms; the tally prints both shares. The "about once in 800 runs" text was wrong: at the old draw mutant 37
  lost 1 of 500, so a clean 500 came by luck in about 1 run in 3 to 1 in 10.
- **m6:** the probe's dumpable control is the compositor's parent, the waiter, found by ppid (it was the first bash in
  `/proc`'s glob order, which is the waiter or the entrypoint by the pids' digits). **m7:** "INT then TERM" asserts that
  the child saw USR1 then USR2 (a forward-once mutant passed it before; killed on the host now). **m4:** the acceptance
  line above reads 1 byte, not 0. **m2, m3:** Owed 75 records #44 and #45, and the late-trap stimulus's 143 as
  intermittent. **m5:** a residual, Owed 77. **m8** (`supervise` is single-use; unreachable, the entrypoint calls it
  once): not changed in this round.
- **Round-3 mutants** (Owed 75), on one throwaway draft PR from this round's head:

  | # | Mutant | Expected red | PR |
  |---|---|---|---|
  | 29 | `remove_var(JOB_TOKEN)` deleted | "no JOB_TOKEN at any CEF child's exec", and no other probe line | #46 |
  | 37 | round 1's `supervise` (the trapped `wait`), the killed-by line kept | the races, in each of 3 runs | #46 |
  | 35 | `c080c3f`'s loop, the killed-by line kept | the races, once | #46 |

## Amendment 2026-10-24 (Owed 85: CEF never initialised after a container restart)

C14's second round (F-024) found CEF failing `init_cef` after 13 of 18 container restarts. F-025 proves the cause on the
real Xvfb and `docs/_INDEX.md`'s decision row "Owed 85 fixed" holds the choices.

- **The cause** (F-025): a restart that keeps the writable layer leaves `/tmp/.X99-lock` and `/tmp/.X11-unix/X99`. The
  restarted container's new pid namespace gives the new Xvfb its predecessor's pid, so Xvfb's lock check finds that
  pid live (itself) and exits; the entrypoint's `[ -e /tmp/.X11-unix/X99 ]` wait passed on the old socket file, and CEF
  found no X server. Step 7's entrypoint snippet ("start Xvfb", then wait for `/tmp/.X11-unix/X99`) is superseded
  by the next bullet.
- **The X server start moves to `docker/xvfb.sh`** (sourced after `supervise.sh`, installed beside it): `start_xvfb 99
  /tmp` removes the display's lock and socket (nothing of an earlier life can own them), starts Xvfb without the job
  token and with `-displayfd`, and returns once Xvfb writes its display number, or 70 with the reason: "it exited before
  it was ready" at once, "not ready after 5 s" after the old wait's 5 s.
- **Tests:** `scripts/xvfb_start_test.py` (check.yml's "Entrypoint X server start", a stub Xvfb that refuses a lock it finds,
  as the real one does on a restart) and image.yml's "In-place restart probe" (the real Xvfb; `scripts/xvfb_witness.sh`, test only, records each
  Xvfb start's pid, the lock it found and Xvfb's stderr). The acceptance bullet above (Amended 2026-10-24, Owed 85).
- **Fly** keeps nothing across a start (F-018), so the change is a no-op there; it is owed to any host whose restart
  keeps the writable layer (ADR 0007 item 5's Docker equivalent).

## Do not touch

Domain and app crates (except to read them), and fly configs (C16). (Amended 2026-10-15) No Fly
service or port config for 7419: it is loopback-only and never exposed.

## Output cap

Under 15 lines: counts, image size, cold start observed in the smoke, deviations.
