# Relay R2 Compositor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to carry out this plan one task at a time. Each task
> brief lives in `docs/tasks/<ID>-*.md` and is self-contained. An implementer reads **this file's
> Global Constraints plus their own brief**, and nothing else is required. Steps use checkbox
> (`- [ ]`) syntax for tracking.

**Goal:** a phone camera becomes a scored broadcast on YouTube with no OBS. A per-match Rust/GStreamer
compositor on Fly pulls the camera from Cloudflare Input A, composites the Seazn overlay page rendered
by CEF, and pushes the programme to Cloudflare Input B. R1's control plane in `seazn.club` creates,
feeds and destroys it.

**Architecture:** a hexagonal Rust workspace.
- A pure `domain` crate holds one `Supervisor` aggregate with table-driven sub-machines. Its signature
  is `decide(event, now) → Decision{next, effects, frame}`.
- An `app` crate runs the event loop and applies effects through ports (traits).
- An `adapters` crate holds the GStreamer, CEF and HTTP implementations, as four isolated pipelines:
  input, graphics, programme and output. The programme pipeline is never rebuilt.
- A `testkit` crate provides a fake for every port.
- `seazn.club` gains Input B provisioning, a vendored job contract, Fly `on-failure` restarts and a
  composed-mode UI.

**Tech stack:**
- Rust 1.91 (pinned)
- gstreamer-rs 0.24 (`gstreamer`, `gstreamer-app`, `gstreamer-video`) against GStreamer 1.24
  (Ubuntu 24.04)
- `gstcefsrc` built from source, with the CEF version pinned in T00
- x264, `ureq` 2, `serde`/`serde_json`, `crossbeam-channel`, `proptest`
- harness: MediaMTX + Docker Compose + `tc netem`
- web: Next.js (`seazn.club`), zod, Playwright, vitest

**Spec:** `docs/specs/2026-10-14-r2-compositor-design.md`, with decisions in `docs/decisions/0001`–`0006`.

---

## Global Constraints

These apply to every task. Values are copied from the spec and ADRs.

- **Two repos.** Tasks `C*` land in `onryde/seazn.club.compositor`. Tasks `R2*` land in
  `onryde/seazn.club`, in a worktree, never on the main checkout. Every R2 task obeys
  `seazn.club/AGENTS.md`, `docs/superpowers/RULES.md` and `docs/superpowers/TEST-STRATEGY.md`.
- **Pipelines:** two-pipeline rule (spec §3.2.1), realised as four pipelines (spec §3.3 refinement).
  The programme pipeline is **never rebuilt** during a session.
- **Output:** 720p30 by default (`1280×720`, 30 fps, 4000 kbps), with a 1080p30 flag (`1920×1080`,
  30 fps, 6000 kbps). CEF always renders the page at `1920×1080`.
- **Programme thresholds:** Frozen at 0.5 s with no frame; slate at 3 s; back to live after 1 s of
  continuous frames. The score layer is on in every state.
- **Input backoff:** 1 s → 10 s cap, retried forever. A kick alert fires at **more than** 3 kicks in
  5 min.
- **Output:** stall when no bytes are acked for 5 s. Reconnect within 5 s. Encoded ring buffer 2 s,
  dropping the oldest.
- **Queues:** decoded video 1 s, audio 400 ms. All leaky, dropping the oldest. Nothing unbounded,
  anywhere.
- **Memory and CPU:** 1 GB soft limit (alert), 2 GB hard. The OOM order kills CEF before the encoder.
  CEF is capped at about 600 MB and recycled every 2 h. CPU ≤70 % sustained.
- **Machine:** Fly `performance-4x`, 8 GB, region `lhr`, restart `on-failure` with `max_retries: 2`,
  `auto_destroy: true`, no inbound port.
- **Machine env (R1, unchanged):** `SESSION_ID`, `JOB_TOKEN`, `APP_URL`, `RELAY_DEADLINE_AT`.
- **Heartbeat:** every 5000 ms. Optional fields are omitted when not measured, **never sent as 0**.
- **Exit codes:**
  - `0`: ended, deadline, signal, or session gone (410)
  - `78`: unauthorized or not found (401/404)
  - `70`: fatal
- **Domain purity:** `crates/domain` has no dependency on gstreamer, cef, ureq or tokio, and no use
  of `SystemTime::now`, `Instant::now`, `std::env`, `std::fs`, `std::net` or `std::thread`. Time is
  passed in as `Ms` (a `u64` of milliseconds).
- **Port boundary:** `crates/app` never depends on `crates/adapters`.
- **Numbers:** every number lives in `crates/domain/src/config.rs`, with a doc line naming its source
  (PDF §, spike F-id, or the R1 contract). A spike measurement overrides the PDF value and must cite
  its F-id.
- **Tests:**
  - Expected values come from `Thresholds::default()` declarations or the PDF, **never** from the
    code under test.
  - Every property or sweep reports the count it checked, and **zero checked is a failure**.
  - Mutate each guard you add once and watch a test fail. Record the mutation in the task's commit
    body.
  - Test the sequence: a second call, empty input, after an interruption, and the other mode.
- **Secrets** never appear in logs. Redact everything after `/live/` and every query string. No
  secret is ever passed as argv or formatted into an error.
- **Scoped verification only.** Run the tests that cover the changed files. For R2 web tasks (R2001–R2006) this means
  never the full vitest or e2e suite locally (house rule). Judge vitest green from
  `--reporter=json` (`numPassedTests`/`numTotalTests`), not from the `rtk` summary.
- **Commits:** Conventional Commits, ending with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. PR bodies end with
  `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- **UI (R2006 only):** screenshots at 320, 768 and 1280 with no horizontal scroll; e2e at all seven
  widths; every string in all 4 locales; the `frontend-design` skill is used.

## Review Focus

These are input classes the spec implies but that no task's headline tests would naturally hit. Each
line names the input and the expected behaviour, and points to the task that owns the pinning test.

1. **A camera that comes back at a different resolution or frame rate after reconnecting** (a phone
   rotated mid-match, or a CF transcode change). The programme stays 1280×720@30 and the camera is
   letterboxed, with no caps renegotiation reaching the programme pipeline. Owner: **C10**, test
   `input_caps_change_does_not_reach_programme`.
2. **A score page that never signals ready** (deploy mid-match, a JS error, a 500 from the page). The
   programme still goes live with the camera after `cef_load_timeout_ms`, and the graphics status
   reads `restarting`/`failed`, never `ready`. Owner: **C05** (domain) and **C11** (adapter test
   `page_without_ready_signal_times_out`).
3. **A bootstrap that returns 200 but for a passthrough session** (a mode mismatch through a race or
   an operator change). The compositor refuses to stream and exits `78`, and never pushes to a
   `target` it was not given for composed mode. Owner: **C07**, test
   `composed_fixtures_decode_and_passthrough_is_refused`.
4. **A deadline already in the past at boot** (a slow cold start, or a Machine restarted after
   `RELAY_DEADLINE_AT`). Exit `0` within one tick; never start the pipelines. Owner: **C06**, test
   `deadline_in_past_at_bootstrap_exits_clean`.
5. **Fly restarting the Machine in place after a crash, seen from web.** The sweep keeps the same
   attempt number `r<n>` and does not create `r<n+1>` while beats resume. Owner: **R2003**, test
   `in_place_restart_keeps_attempt`.

---

## File structure (compositor repo)

```
Cargo.toml                         workspace (members below), shared lints
rust-toolchain.toml                channel = "1.91.0"
AGENTS.md / CLAUDE.md              rulebook / "@AGENTS.md"                       (C01)
_FINDINGS.md                       F-ids                                          (C00, appended by all)
crates/domain/src/
  lib.rs                           module list, #![forbid(unsafe_code)]          (C02)
  time.rs                          Ms, elapsed()                                  (C02)
  config.rs                        Thresholds, OutputProfile — the one number authority (C02)
  backoff.rs                       delay(attempt, base, cap)                      (C02)
  window.rs                        bounded sliding timestamp window               (C02)
  programme.rs                     ProgrammePhase, FrameChoice, PROGRAMME_TABLE   (C03)
  input_link.rs                    InputLink sub-machine                          (C04)
  output_link.rs                   OutputLink sub-machine                         (C04)
  graphics.rs                      Graphics sub-machine                           (C05)
  ladder.rs                        DegradeLadder                                  (C05)
  lifecycle.rs                     Lifecycle, ExitReason, exit codes              (C06)
  health.rs                        HealthReport, BeatState                        (C06)
  supervisor.rs                    Supervisor aggregate, Event, Effect, Decision  (C06)
crates/domain/tests/               purity.rs, deps.rs (C01); properties.rs (C06)
crates/app/src/
  ports.rs                         ControlPlane + DTOs (C07); InputSource, ProgrammeOutput,
                                   GraphicsRenderer, Metrics, ControlQueue, Inbound, Ports (C08)
  driver.rs                        Driver: step()/tick_due(), Effect → port calls (C08)
  control_worker.rs                HTTP off the frame clock, bounded queues       (C08)
  run.rs                           blocking loop: recv_timeout → step / tick_due  (C08)
crates/app/tests/boundary.rs       port-boundary guard                            (C01)
crates/testkit/src/                fakes for every port, session_720p()           (C08)
crates/adapters/src/
  control_http.rs                  ureq ControlPlane                              (C07)
  frame_store.rs                   latest-frame slots shared by pipelines         (C09)
  stats.rs                         fps/bitrate/egress/encoder-speed counters      (C09)
  slate.rs                         pre-rendered slate frames                      (C09)
  gst_programme.rs                 programme pipeline (never rebuilt)            (C09)
  gst_output.rs                    output pipeline + 2 s encoded ring             (C09)
  gst_input.rs                     input pipeline                                 (C10)
  cef_graphics.rs                  graphics pipeline                              (C11)
  metrics_proc.rs                  cpu/mem from /proc                             (C12)
  redact.rs                        URL redaction                                  (C07)
crates/compositor/src/            main.rs, env.rs, wiring.rs, logged.rs; COMPOSITOR_DRIVERS (C12)
crates/harness/src/bin/           fake-camera, fake-control, checker, dump-thresholds (C13, C14)
contracts/                         vendored relay-job.v1 + VENDORED.json          (C07)
docker/Dockerfile, entrypoint.sh   build + runtime (GStreamer, gstcefsrc, CEF, xvfb, tini) (C12)
docker/GSTCEF_COMMIT               the one pin both image.yml and deploy-*.yml read (C12/C16)
harness/                           compose + fakes + checker                      (C13)
harness/cases/                     smoke (C13) + 10 chaos cases .sh/.jq (C14)
harness/thresholds.json            generated from Thresholds + budgets; drift-gated (C14)
harness/soak/                      split-Machine soak, verdict.jq + fixtures      (C15)
docs/runbooks/relay-compositor.md  one entry per heartbeat signal                 (C17)
fly/stg.toml, fly/prod.toml        (C16)
.github/workflows/                 check (C01), image (C12), harness (C13), chaos (C14),
                                   soak (C15), deploy-stg/deploy-prod/promote (C16)
```

## Task order and dependencies

| ID | Task | Repo | Depends on | Brief |
|---|---|---|---|---|
| R2001 | Composed-mode hazard fix | seazn.club | — | `tasks/R2001-composed-hazard-fix.md` |
| C00 | **Spike gate** (throwaway; HALTS the plan on a fail of item 1 or 2) | compositor | — | `tasks/C00-spike-gate.md` |
| C01 | Workspace scaffold, rulebook, CI check, guard tests | compositor | — | `tasks/C01-workspace-scaffold.md` |
| C02 | Domain foundations: time, config, backoff, window | compositor | C01 | `tasks/C02-domain-foundations.md` |
| C03 | Domain: Programme | compositor | C02 | `tasks/C03-domain-programme.md` |
| C04 | Domain: InputLink + OutputLink | compositor | C02 | `tasks/C04-domain-links.md` |
| C05 | Domain: Graphics + DegradeLadder | compositor | C02 | `tasks/C05-domain-graphics-ladder.md` |
| C06 | Domain: Lifecycle, HealthReport, Supervisor, properties | compositor | C03, C04, C05 | `tasks/C06-domain-supervisor.md` |
| R2002 | relay-job.v1 contract + schema extension | seazn.club | R2001 | `tasks/R2002-relay-job-contract.md` |
| C07 | Vendor contract, ControlPlane port + HTTP adapter | compositor | C06, R2002 merged | `tasks/C07-control-plane.md` |
| C08 | Ports, testkit fakes, event loop, scenario tests | compositor | C07 | `tasks/C08-app-event-loop.md` |
| C09 | Programme + output pipelines | compositor | C08, **C00 pass** | `tasks/C09-gst-programme-output.md` |
| C10 | Input pipeline | compositor | C09 | `tasks/C10-gst-input.md` |
| C11 | Graphics pipeline (CEF) | compositor | C09, **C00 pass** | `tasks/C11-cef-graphics.md` |
| C12 | Binary wiring, metrics, Dockerfile, image CI | compositor | C10, C11 | `tasks/C12-binary-and-image.md` |
| C13 | Harness: fakes + reference checker | compositor | C12 | `tasks/C13-harness.md` |
| C14 | Chaos suite | compositor | C13 | `tasks/C14-chaos-suite.md` |
| C15 | Soak | compositor | C14 | `tasks/C15-soak.md` |
| R2003 | Runner `on-failure`, same-attempt sweep, 15 s composed stale, exit 78 never retries | seazn.club | R2002 | `tasks/R2003-runner-restart-and-beats.md` |
| R2004 | Composed provisioning: Input B, composed session facts, delay column | seazn.club | R2003, **C00 F-002** | `tasks/R2004-input-b-provisioning.md` |
| R2005 | Overlay page: `sid`+`st` access, `host=compositor` signals, stale, clamp | seazn.club | R2004, **C00 F-001** | `tasks/R2005-overlay-page-compositor-host.md` |
| R2006 | Organiser UI: composed option + delay override | seazn.club | R2004, R2005 | `tasks/R2006-organiser-composed-ui.md` |
| C16 | Deploy workflows, Fly apps config | compositor | C12 | `tasks/C16-deploy.md` |
| C17 | Stg end-to-end acceptance + runbook | both | all | `tasks/C17-stg-acceptance.md` |

**Parallel lanes:**
- Safe to run in parallel (provably disjoint): C00 alongside C01–C06 (spike branch vs main), and
  R2001 alongside any C task.
- After C02: C03, C04 and C05 touch disjoint files, but all three add a line to
  `crates/domain/src/lib.rs`. **Run them sequentially**, or give them worktree isolation and merge
  `lib.rs` by hand.
- R2003, R2004 and R2005 all touch `seazn.club/apps/web/src/server/relay/*`, so they run **sequentially**.

**Wave gates:**
1. After C06: domain review plus all domain tests and properties.
2. After C12: the image builds, and a 60 s harness smoke passes.
3. After C15: three consecutive chaos runs are green, and the soak has passed.
4. C17: the owner watches the end-to-end run.

The orchestrator re-runs each wave's **scoped** gate itself and pastes the raw counts.
