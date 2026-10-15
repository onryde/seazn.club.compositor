# C12: Binary wiring, process metrics, Dockerfile, image CI

**Repo:** `seazn.club.compositor`. **Depends on:** C10, C11. Read F-001 (gstcefsrc commit and CEF
version) and F-005 (cold start).

## Context

R1's runner (pinned at `seazn.club` `apps/web/src/server/relay/runner-fly.ts:234-238`, 2026-10-14;
re-pin before relying on it) creates the Machine with:

- `image: RELAY_IMAGE`
- `env: { SESSION_ID, JOB_TOKEN, APP_URL, RELAY_DEADLINE_AT }`. `RELAY_DEADLINE_AT` is an **ISO-8601
  string** from `Date.toISOString()`, e.g. `2026-10-14T18:30:00.000Z`.
- the stop signal is `SIGINT` (`config.ts` `RUNNER_STOP_SIGNAL`), with a grace of
  `RUNNER_STOP_GRACE_SECONDS = 10`

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
dedicated thread. A second signal during drain is ignored. The domain owns the drain deadline
(C06 `drain_timeout_ms`, which must be below the 10 s grace; **assert that in a test here**, deriving
10 s from a constant `R1_STOP_GRACE_MS` in `crates/compositor/src/main.rs` with a doc line citing
`config.ts RUNNER_STOP_GRACE_SECONDS`).

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
  - `drain_timeout_fits_inside_r1_stop_grace`:
    `Thresholds::default().drain_timeout_ms < R1_STOP_GRACE_MS`

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
    (both from constants, never a flat number: AGENTS.md class 20). An exit through the drain
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
     Fail if the passed count is < 7.
  4. **60 s smoke:** start the image with `COMPOSITOR_DRIVERS=fake`, a deadline 60 s ahead and the
     dummy env. Expect exit 0 within 75 s, and stderr containing `programme started` and
     `exit code=0`.
  5. Print the image size, and fail above 2.5 GB (CEF is large; the cap stops bloat creep).

  Push is C16's job; this workflow never pushes.

- [ ] **Step 9: Mutate once each.**
  1. Make unset `COMPOSITOR_DRIVERS` default to fake in release. Expected red:
     `drivers_unset_refuses_in_release…`.
  2. Include the token value in `EnvError`'s message. Expected red:
     `missing_each_required_key_is_refused_naming_the_key`.
  3. Drop the `encoder_speed` copy in `ProcMetrics::sample`. Expected red:
     `copies_every_stats_field_into_measured`.

- [ ] **Step 10: Commit and open the PR.** `feat(compositor): binary wiring, proc metrics and the relay image (C12)`

## Acceptance

- Env, metrics and fake-run tests pass.
- `image.yml` is green on the PR: plugin present, 7 cef tests, smoke exits 0, size under the cap.
- 3 mutants went red.
- The image runs as non-root.


## Final heartbeat with the exit reason (owner ruling 2026-10-15)

The main loop executes the supervisor's final `SendHeartbeat` before `std::process::exit`. Add to
the fake-mode smoke a run where fake-control serves `reply-ending.json`, then assert that the last POST
fake-control received has `state: "stopped"` and `exitReason: "ended"`. Add a second run with
`ProgrammeStartFailed` forced via the fake driver: exit 70, and the last POST carries
`exitReason: "fatal"` with a non-empty `exitDetail`.
Exits **before** a session exists (`EnvError`, exit 78 at config parse) cannot send a beat, because
they have no URL or token yet. They stay stderr-only, and that's expected, not a gap.

## Do not touch

Domain and app crates (except to read them), and fly configs (C16).

## Output cap

Under 15 lines: counts, image size, cold start observed in the smoke, deviations.
