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
**Amended 2026-10-15** by `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
(the overlay templates spec, D1–D13; branch `docs/overlay-templates-and-scenes` until it merges). Its
§6 adds R2007 and amends R2004, R2005, R2006, C06, C08, C09, C11 and C12. Each amended brief has a
section headed "Amendment 2026-10-15 (overlay templates spec)", and that section overrides the text it
supersedes. See "Amendments 2026-10-15" at the end of this plan for what is still owed elsewhere.

---

## Global Constraints

These apply to every task. Values are copied from the spec and ADRs.

- **`_FINDINGS.md` (repo root) overrides any brief it contradicts.** C00 measured the real platform;
  read the F-entries your task touches before starting (C09–C12, C14 and R2005 especially).

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
  (Amended 2026-10-15) The same bar applies to R2006's template picker. R2007's
  `/admin/overlay-gallery` is staff-only and is verified at **1280 only** (house rule: `/admin` is
  desktop). R2005's scene cards are judged in that gallery and in its 1920×1080 e2e.
- **Overlay templates (spec 2026-10-15, amended):**
  - **One render function** (`OverlayPreview`, R2007) draws the page, the gallery and the picker (D6).
  - **Text** is ≥ 30 px at 1080 (D9).
  - **The score bar's bottom edge** sits ≥ 150 px above the frame's bottom, in both hosts (D9).
  - **Mockups are layout only.** The build uses the house theme (D11).
  - **A fixed Seazn library.** No organiser HTML (D3).
  - **Every moment** is covered in every template and in the compositor (D13).
  - **The Machine's slate never carries text** (C09).

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
  phase_http.rs                    loopback /phase endpoint (127.0.0.1:7419)      (C12, amended 2026-10-15)
  frame_store.rs                   latest-frame slots shared by pipelines         (C09)
  stats.rs                         fps/bitrate/egress/encoder-speed counters      (C09)
  slate.rs                         pre-rendered slate frames (textless, amended)  (C09)
crates/adapters/assets/logo-wide-night.png  vendored Seazn logo for the slate  (C09, amended 2026-10-15)
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
harness/phase-probe-server.py, harness/pages/phase-probe.html   loopback probe (C11 Step 0, amended 2026-10-15)
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
| C05 | Domain: Graphics + DegradeLadder. **Amended 2026-10-18 (owner, Owed 20):** no `FastPreset`; `Full → OverlayHalfRate`, and the bottom holds (built) | compositor | C02 | `tasks/C05-domain-graphics-ladder.md` |
| C05b | **New 2026-10-18 (owner, Owed 18 "2 Ok").** Domain: hang detection, with `Graphics::on_alive` and a Ready-arm hang as a counted failure | compositor | C05. Run it before C06's `CefAlive` routing | `tasks/C05b-domain-graphics-hang.md` |
| C06 | Domain: Lifecycle, HealthReport, Supervisor, properties. **Amended 2026-10-15:** the final hold, `ProgrammePhase::Final`, `PhaseChannel` | compositor | C03, C04, C05 | `tasks/C06-domain-supervisor.md` |
| R2002 | relay-job.v1 contract + schema extension | seazn.club | R2001 | `tasks/R2002-relay-job-contract.md` |
| C07 | Vendor contract, ControlPlane port + HTTP adapter. **Amended 2026-10-15:** see "Amendments 2026-10-15" below; the brief is not yet edited | compositor | C06, R2002 merged, **R2007 PR B merged** | `tasks/C07-control-plane.md` |
| C08 | Ports, testkit fakes, event loop, scenario tests. **Amended 2026-10-15:** the `PhasePublisher` port | compositor | C07 | `tasks/C08-app-event-loop.md` |
| C09 | Programme + output pipelines. **Amended 2026-10-15:** the textless slate | compositor | C08, **C00 pass** | `tasks/C09-gst-programme-output.md` |
| C10 | Input pipeline | compositor | C09 | `tasks/C10-gst-input.md` |
| C11 | Graphics pipeline (CEF). **Amended 2026-10-15:** Step 0 (the loopback probe) needs only C00 pass and runs **first**; R2005 waits on it | compositor | C09, **C00 pass** | `tasks/C11-cef-graphics.md` |
| C12 | Binary wiring, metrics, Dockerfile, image CI. **Amended 2026-10-15:** the `/phase` endpoint and the stop-grace test | compositor | C10, C11, **R2004 PR 0 deployed to stg** (the stop contract; corrected by review m9) | `tasks/C12-binary-and-image.md` |
| C13 | Harness: fakes + reference checker | compositor | C12 | `tasks/C13-harness.md` |
| C14 | Chaos suite | compositor | C13 | `tasks/C14-chaos-suite.md` |
| C15 | Soak | compositor | C14 | `tasks/C15-soak.md` |
| R2003 | Runner `on-failure`, same-attempt sweep, 15 s composed stale, exit 78 never retries | seazn.club | R2002 | `tasks/R2003-runner-restart-and-beats.md` |
| R2007 | **New 2026-10-15.** PR A: the overlay template registry, text floor and bar lift, `/admin/overlay-gallery`, `OverlayPreview`. PR B: the owed R2002 contract follow-up (`final`, `phaseChannel`, the phase endpoint `$defs`) | seazn.club | PR A: the end-card-undo fix (`fix/overlay-end-card-undo`) merged. PR B: **R2003 (#967) merged** (corrected by the briefs review: they share three files) | `tasks/R2007-overlay-template-system.md` |
| R2004 PR 0 | **New 2026-10-15.** The relay stop contract, its own small web PR: composed Machines get `stop_config { signal: "SIGTERM" }` while web's stop stays an explicit SIGINT (coordinator decision I4), a measured signal-precedence F-id, and the grace raised to 20 s (**owner ruling 2026-10-16, spec D15**) | seazn.club | R2003 (#967) merged. **Not** R2007 or the undo fix | `tasks/R2004-input-b-provisioning.md` ("PR 0") |
| R2003b | **New 2026-10-16 (owner-approved follow-up).** Crash-loop cap: a session fails as `machine_crash` after 3 fatal crashes, with a schema addition | seazn.club | R2003 (#967) merged. It touches `server/relay/*`, so the coordinator places it in the web relay merge order when dispatching | none here (dispatched separately by the coordinator) |
| R2004 | Composed provisioning: Input B, composed session facts, delay column. **Amended 2026-10-15:** `template=` in `overlayUrl`, the stored choice | seazn.club | R2003, **C00 F-002**, **R2007 PR A**, **R2004 PR 0 and R2007 PR B merged** (shared files; merge order under "Parallel lanes") | `tasks/R2004-input-b-provisioning.md` |
| R2005 | Overlay page: `sid`+`st` access, `host=compositor` signals, stale, clamp. **Amended 2026-10-15:** scenes, cards, the phase channel with fallback B | seazn.club | R2004, **C00 F-001**, **R2007 PR A + PR B**, **C11 Step 0 finding** | `tasks/R2005-overlay-page-compositor-host.md` |
| R2006 | Organiser UI: composed option + delay override. **Amended 2026-10-15:** the template picker with live previews, persisted | seazn.club | R2004, R2005, **R2007 PR A + PR B** | `tasks/R2006-organiser-composed-ui.md` |
| C16 | Deploy workflows, Fly apps config | compositor | C12 | `tasks/C16-deploy.md` |
| C17 | Stg end-to-end acceptance + runbook | both | all | `tasks/C17-stg-acceptance.md` |

**Parallel lanes:**
- Safe to run in parallel (provably disjoint): C00 alongside C01–C06 (spike branch vs main), and
  R2001 alongside any C task.
- After C02: C03, C04 and C05 touch disjoint files, but all three add a line to
  `crates/domain/src/lib.rs`. **Run them sequentially**, or give them worktree isolation and merge
  `lib.rs` by hand.
- R2003, R2004 and R2005 all touch `seazn.club/apps/web/src/server/relay/*`, so they run **sequentially**.
- (Amended 2026-10-15) **R2007 PR A** (`components/overlay/**`, `app/admin/overlay-gallery/**`,
  `server/overlay/gallery-snapshots.ts`, the dictionaries, `scripts/smoke.ts`) runs in parallel with
  R2003 and with every C task. The one possible overlap is the 4 locale dictionaries, if R2003 owes
  failure-reason copy; whichever merges second rebases and re-runs `pnpm i18n:gen-keys`. It runs
  **after** the end-card-undo fix, because both touch `components/overlay/**`.
- (Amended 2026-10-15; **corrected by the review**) **R2007 PR B** (`lib/relay-phase.ts`,
  `server/api-v1/schemas.ts`, `server/relay/sanitise.ts`, `scripts/gen-relay-contracts.ts`,
  `docs/contracts/**`, `openapi/v1.json`, `relay-internal-routes.test.ts`) **does share files with
  R2003**. The earlier "shares no file" claim was false. R2003 (#967, `origin/feat/relay-runner-restart`
  `f9034f575`, not merged at `b01d07cd5`) also edits `server/api-v1/schemas.ts`, `openapi/v1.json` and
  `relay-internal-routes.test.ts` (+340 lines), as well as `stream-sessions.ts`,
  `stream-session-view.ts`, `fly-client.ts`, `runner-fly.ts` and `config.ts`. So **PR B branches after
  #967 merges**, and it must merge **before C07 vendors** the contract.
- (Amended 2026-10-15) **R2004 PR 0** (the stop contract: `runner-fly.ts`, `fly-client.ts`, `config.ts`
  and their tests) branches after #967 merges, too. It shares no file with R2007 PR A, so they can run
  in parallel; with PR B it shares none either, but both wait on #967.
- (Confirm pass N4) **The web relay merge order, stated explicitly:**
  1. **R2003 (#967)** merges.
  2. **R2004 PR 0** and **R2007 PR B** branch from that `main` and run in parallel. Their files are
     disjoint: PR 0 has `runner-fly.ts`, `fly-client.ts`, `config.ts`, `config.test.ts` and the
     runner-fly tests; PR B has `schemas.ts`, `openapi/v1.json`, `sanitise.ts`, `lib/relay-phase.ts`,
     the contracts and `relay-internal-routes.test.ts`. Either may merge first.
  3. **R2004 (main PR)** branches only after **both** have merged, plus R2007 PR A. It shares
     `config.ts` and `config.test.ts` with PR 0 (its `RELAY_OUTPUT_PROFILE`), and
     `relay-internal-routes.test.ts` with PR B (tests 1–10). So PR B goes **before** R2004, not after:
     PR B is on C07's critical path, and R2004 already waits on PR A.
  4. **R2005**, then **R2006**, as before.
- (Amended 2026-10-15) **C11 Step 0** is a docs/harness commit on the compositor `main`. It can run as
  soon as C00 has passed, in parallel with C06–C08.

**Wave gates:**
1. After C06: domain review plus all domain tests and properties.
2. After C12: the image builds, and a 60 s harness smoke passes.
3. After C15: three consecutive chaos runs are green, and the soak has passed.
4. C17: the owner watches the end-to-end run.

The orchestrator re-runs each wave's **scoped** gate itself and pastes the raw counts.

## Dependency graph (amended 2026-10-15)

```
end-card-undo fix ──▶ R2007 PR A ──┬──▶ R2004 (+ R2003, C00 F-002, R2004 PR 0, R2007 PR B)
                                   ├──▶ R2005 (+ R2004, C00 F-001, C11 Step 0, R2007 PR B)
                                   └──▶ R2006 (+ R2004, R2005, R2007 PR B)
R2003 (#967) ──┬──▶ R2007 PR B ──▶ C07 vendors the contract ──▶ C08 ──▶ … ──▶ C12
               └──▶ R2004 PR 0 (stop contract; grace 20 s, ruled D15) ───────▶ C12
C00 PASS ──▶ C11 Step 0 (_FINDINGS F-0xx) ──▶ R2005
```

## Amendments 2026-10-15 (overlay templates spec)

Rulings, false premises and open questions are logged in `docs/_INDEX.md`. These are the items owed by
tasks whose briefs this amendment did **not** edit. The dispatch for each of these tasks must carry
the relevant line.

- **C07 (before it vendors):**
  - Vendor only after R2007 PR B merges. Web's strict `programmePhase` enum answers **400** to
    `"final"` until then, and that includes the final exit beat.
  - `heartbeat_wire` emits `programmePhase: "final"` for `ProgrammePhase::Final`, and `phaseChannel`
    (`"loopback"` / `"web"`) only when `HealthReport.phase_channel` is `Some`; absent otherwise,
    never defaulted.
  - Extract the phase mapping as `pub fn programme_phase_wire(p: ProgrammePhase) -> &'static str`.
    C12's endpoint reuses it.
  - `every_heartbeat_we_can_emit_validates_against_the_contract` must drive a script that reaches
    `Holding`/`Final` and sets a `PhaseChannel`, so both new values are validated against the vendored
    schema. Print the count of beats that carry each; 0 fails.
- **C13/C14 (harness):**
  - Every run now ends with C06's final hold: the Final scene, or the textless slate when graphics
    failed, for `final_hold_ms`, then the drain. The textless slate still fails the strip checksum, so
    `slate_frames` classification is unchanged.
  - `run-case.sh` must wait `final_hold_ms + drain_timeout_ms` after the case's end request before
    `down`, or it cuts the drain.
  - Any verdict that counts slate spans must exclude the span that starts at the end request.
- **C17 (stg acceptance):** now edited in its brief (review 2026-10-15). See C17's "Amendment
  2026-10-15": rows 1, 3, 7 and 8 amended, rows 9–11 added (picked template, platform stop, OBS
  record), and the compatibility records in Acceptance.

**Plan decisions recorded by the 2026-10-15 review** (not owner rulings):
- **`overlay_template` has two readers** (review m10; coordinator decision). One is
  `sessionFactsForJob`'s composed `overlayUrl`. The other is the capture descriptor's `overlayUrl`
  (`capture-phone.ts:176`), which today falls back to the sport default before any session. R2004
  change 7 resolves the stored choice there. No phone reads that field today (owner ruling
  2026-10-08, capture `AGENTS.md` §7), but it is public API v1, so it must not name a stale template.
- **Compatibility is checked every release in both browsers** (coordinator decision G1): C11's
  `cefsrc` scene check and R2007 step 14b's OBS check, each recorded as an F-id, with C17 requiring
  both.
