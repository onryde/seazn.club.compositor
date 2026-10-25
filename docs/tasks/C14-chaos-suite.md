# C14: Chaos suite (~~10~~ ~~12~~ 16 cases, automated pass/fail; amended 2026-10-18, 2026-10-20 and 2026-10-24)

**Repo:** `seazn.club.compositor`. **Depends on:** C13. Read spec §6 (the robustness table and
budgets) and §9.4 (the cases).

## Context

Each case is a `harness/cases/<name>.sh` that:
1. drives the harness controls on a timeline
2. sleeps for its duration
3. leaves the verdict to `harness/cases/<name>.jq`, which reads `report.json`

**Every expected number is read from `harness/thresholds.json`**, which this task generates from the
domain: `cargo run -p harness --bin dump-thresholds > harness/thresholds.json` serialises
`Thresholds::default()` plus the spec §6 budgets, which are added to `config.rs` here as named consts
with doc lines, plus C02's `ENCODER_SPEED_REALTIME_MIN` (exported as `encoder_speed_realtime_min`). **No number is typed into a `.jq` file.** If a `.jq` needs a number, it must come from
`thresholds.json` (`$t.slate_after_ms` etc.) or from the case's own timeline variables. When the
spike re-pins a threshold, the cases move with it.

### Budget constants to add to `crates/domain/src/config.rs`

These come from spec §6 "Budgets" (a doc line on each cites it):

| Constant | Value |
|---|---|
| `BUDGET_SLATE_AFTER_LOSS_MS` | 3_100 |
| `BUDGET_RETURN_TO_LIVE_MS` | 1_200 |
| `BUDGET_OUTPUT_RECONNECT_MS` | 5_000 |
| `BUDGET_OUTPUT_LOST_MAX_MS` | 2_000 |
| `BUDGET_LADDER_REALTIME_MS` | 10_000 |
| `BUDGET_PROCESS_RESTART_OUTPUT_MS` | 15_000 |
| `BUDGET_FRAME_TO_ENCODER_MS` | 150 |
| `BUDGET_AV_OFFSET_MS` | 40 |

Add a unit test that each budget is consistent with its threshold. For example,
`slate_after_ms + one frame period ≤ BUDGET_SLATE_AFTER_LOSS_MS`. A spike-driven threshold change
that breaks a budget then reds at `cargo test`, not in a 2-minute chaos run.

### The 10 cases (12 with the 2026-10-18 amendment's cases 11 and 12; 16 with the 2026-10-20 amendment's cases 13–16)

The PDF's kill-switch case is replaced by "CEF killed".

| # | File | Script | Verdict (`.jq`) |
|---|---|---|---|
| 1 | `camera-stop` | at 20 s `POST camera /pause`; at 30 s `/resume`; run 50 s | a slate span begins ≤ `BUDGET_SLATE_AFTER_LOSS_MS` after the last pre-pause frame; first strip frame after resume ≤ `BUDGET_RETURN_TO_LIVE_MS` after the camera's first post-resume `emit` wall time; `output_outages == []`; `counter_regressions == 0` |
| 2 | `camera-flap` | `/pause` then `/resume` every 5 s for 120 s | `output_outages == []`; `counter_regressions == 0`; **hysteresis from the strip, not the beats** (5 s heartbeats can't see a 1 s rule): every `camera.resumes[].suppressed_ms ≥ resume_after_ms − one frame period` and `≤ BUDGET_RETURN_TO_LIVE_MS`; anti-vacuity: `resumes` has ≥ 10 entries (120 s of 5 s flaps) |
| 3 | `reader-kicked` | kick the SRT reader every 20 s, 6 times | after each kick, strip frames return ≤ `BUDGET_RETURN_TO_LIVE_MS + input_backoff_cap_ms`; heartbeats show `inputKickAlert: true` once `kick_alert_count` kicks fall in `kick_window_ms`, and **not before** (assert the first `true` beat comes after kick #`kick_alert_count`); `output_outages == []` |
| 4 | `dest-disconnect` | at 20 s `netem out loss 100%`; at 50 s `clear`; run 80 s | output resumes ≤ `BUDGET_OUTPUT_RECONNECT_MS` after clear; the programme lost per outage, measured as the counter gap across the outage minus the outage wall span, is ≤ `BUDGET_OUTPUT_LOST_MAX_MS` worth of frames; heartbeats show `input: connected` throughout (input untouched); `pipelineRebuilds` unchanged |
| 5 | `dest-stall` | at 20 s `netem out rate 1kbit` (accepts, but no acks); at 50 s `clear` | a `sinkSwaps` increase appears in a beat ≤ `output_ack_stall_ms + heartbeat_interval_ms` after 20 s; output recovers after clear as in case 4 |
| 6 | `cpu-starved` | start a `stress-ng --cpu 4` sidecar in the compositor netns/cgroup at 20 s, for 40 s | `fps_mean` over the window stays ≥ 29.5 (the clock holds); `pts_gaps == []`; ~~heartbeats show `droppedFrames` increase **or** `rung != "full"`, and at least one of them changes~~ (amended 2026-10-25, Owed 86: the honest beat reads under `output_fps_floor` and its alarm raises in the window and clears after it, and the ladder never moves; see "Amendment 2026-10-25, case 6's honest beat"); after stress ends, `encoderSpeed ≥ $t.encoder_speed_realtime_min` within `BUDGET_LADDER_REALTIME_MS` |
| 7 | `score-down` | `POST fake-control /control/score-down?ms=60000` at 20 s | heartbeats show `scoreStale: true` within 10 s (spec §6, page side) + one beat interval; the overlay index never changes during the outage and never regresses; `scoreStale: false` after the first poll following recovery |
| 8 | `cef-killed` | at 20 s `docker exec compositor pkill -KILL -f cef` | the overlay strip shows the **same** index (held frame) with no transparent gap; camera strip frames continue (no freeze > 2 frames); ~~heartbeats go `graphics: restarting` → `ready`~~ (amended 2026-10-18, Owed 19: restart evidence, see "Amendment 2026-10-18"); `output_outages == []` |
| 9 | `process-killed` | at 20 s `docker exec compositor pkill -KILL -x compositor` (tini exits non-zero, compose's `on-failure:2` restarts it) | output resumes ≤ `BUDGET_PROCESS_RESTART_OUTPUT_MS` after the kill; heartbeats resume; fake-control sees the **same** `SESSION_ID`; `counter_regressions == 0` |
| 10 | `input-impaired` | at 20 s `netem.sh in delay 400ms 100ms loss 2%`; at 80 s `clear`; run 100 s | the programme clock holds through a bad camera leg: `pts_gaps == []`, `fps_mean ≥ 29.5`, `counter_regressions == 0`, `output_outages == []`; anti-vacuity is witnessed **where the fault is injected**, because SRT's fixed 1 000 ms TSBPD latency absorbs a 400 ± 100 ms delay and keeps glass-to-glass flat: (a) `tc -s` on `netem-in` shows the filter's hit count > 0 and the netem child's `dropped` grew during 20–80 s; (b) MediaMTX's reader stats for the compositor's SRT connection show retransmitted packets > 0 in the window (field name pinned with the MediaMTX tag in C13's F-entry). `latency_windows_ms` is reported, never gated |

**Case 10** (`input-impaired`, spec §9.4 "Camera uplink impaired") covers spec §9.3's input
impairment (100–800 ms latency, 1–5 % loss), which no other case exercises.

**Score correction (void) and clock skew are not chaos cases.** Both are **page** logic, owned and
tested by **R2005** (vitest on the derive and clamp rules, plus an e2e) and proven live in C17. A
copy of those rules in the harness page would only test the copy. The harness keeps `score-down`
(case 7), because that one proves the compositor's relay of the page's signal into the heartbeat.

**MediaMTX is off the shelf.** It's the official `bluenviron/mediamtx` image at a pinned tag (C13),
configured, never forked. It runs only in our CI and on our own Machines, never on a public hosted
instance: stream keys and footage stay on infrastructure we control. Real Cloudflare behaviour is
measured in C00 (F-002), copied into the MediaMTX config and case scripts, and proven live in C17.

## Amendment 2026-10-18 (owner rulings on the ladder and on hang detection)

Owner, 2026-10-18, in chat, relayed by the coordinator: "no lite mode; if CPU impacts, measure and
raise the CPU size" (`docs/_INDEX.md` Owed 20), and "2 Ok" (Owed 18). The CPU-cost **measurement**
and the size rule are C15's (the soak runs on the production guest; this suite runs on the harness
host). This suite gains the following.

**Case 6 (`cpu-starved`), the verdict gains three clauses** (Owed 20: "whichever is chosen, C14
case 6 asserts what the overlay strip shows after the stress ends"):
- The score is never removed. The overlay strip keeps following the timeline through the stress and
  after it. Every overlay index change that `score.json` schedules in the window appears in the
  strip, and no strip frame in the window has a transparent overlay.
- Every beat's `rung` is `full` or `overlay_half_rate`, and no other value. A beat that reads
  `overlay_half_rate` is followed by no `full` beat in the same process (the bottom holds, C05).
- The verdict prints the rung of the last beat. A stepped rung is not a red here, because the
  stress is meant to starve the encoder. In production it is the sizing alarm (C17 runbook).

**The budget consistency test:** C05 deleted `ladder_step_min_gap_ms` (it gated only a second
step). `BUDGET_LADDER_REALTIME_MS` is checked against `ladder_window_ms` instead: the ladder's one
step lands within one window of its overruns, so `ladder_window_ms ≤ BUDGET_LADDER_REALTIME_MS`.

**Case 11, `cef-hung`** (Owed 18, approved 2026-10-18). With case 12 below, the suite grows from 10
cases to 12, and the Step 7 commit title says twelve:

| # | File | Script | Verdict (`.jq`) |
|---|---|---|---|
| 11 | `cef-hung` | At 20 s, the harness page hangs: C11's `?crash=1` (`while(true){}` after ready), armed with `crash_after_ms` to fire at 20 s. Run 60 s | Hide at once (owner-approved design (2026-10-18); "at once" is the coordinator's reading of it). **The strip, frame-exact:** the overlay strip holds the **same** index from the hang until the hang is detected, at most `$t.cef_hang_after_ms` + one frame period after the page's last `seazn:alive`. From there it shows **no overlay** (transparent, so no stale score) until generation `g+1`'s first frame after its `CefReady`, and then the overlay again (the positive pair). Camera strip frames continue (no freeze > 2 frames). `output_outages == []`. **The evidence, from the compositor's structured log (C12 amendment), never a beat in a window** (PR #21 review I3: the ~3.8 s non-ready span catches a 5 s beat only ~3 runs in 4):
- ~~the last `cef alive gen=g`~~
- ~~then `overlay hidden` at the last beat + `$t.cef_hang_after_ms` (± one tick), with no `cef crashed gen=g` before it~~
- ~~then `graphics restart gen=g+1` at + `$t.cef_backoff_base_ms`~~
- ~~then `cef ready gen=g+1`~~
- (Amended 2026-10-18, C05b PR #22 review I2; the hide names its reason.) In order:
  - exactly one `overlay hidden reason=hang gen=g`
  - then `graphics restart gen=g+1`
  - then `cef ready gen=g+1`

  There is no `overlay hidden` with `reason=crash`, `reason=recycle` or `reason=failed` in the run. The timing is the
  strip's, frame-exact (above), never a log timestamp: `ts=` is wall-clock time of unstated precision,
  and the domain's clock is monotonic.

Every beat in that span reads `ready`, `restarting` or `starting`, and never `failed`. **Anti-vacuity:** the log has at least 2 `cef alive gen=g` lines before the hang and none after it, the hang time comes from the page's own log line (never the script's timer), and the strip's transparent span is ≥ 1 frame |

- **Every case's page beats.** The harness page logs `seazn:alive` like the real one (C11
  amendment; false-premise row "The hang design needs only the real page to beat"). Without the
  beat, C05b restarts the page every `cef_hang_after_ms` in every case, and cases 1–10 measure
  restarts.
- **Step 5 gains mutant 4.** Delete C05b's Ready hang arm in a scratch build. Expected red: case 11,
  on its `graphics` clause. As with mutant 1, the verdict reads the committed
  `harness/thresholds.json`.
- **Files:** `harness/cases/cef-hung.{sh,jq}`.

### Owed checks as acceptance criteria (Owner, 2026-10-18, in chat, relayed by the coordinator: "apply rec 4-9"; and owner, 2026-10-18, in chat, relayed by the coordinator: "fix the product gaps along with tasks")

**Case 8 (`cef-killed`), reworded (Owed 19).**
- **Superseded 2026-10-19 (PR #32's confirm pass; `docs/_INDEX.md` Owed 62).** Measured on CEF 139:
  every CEF child SIGKILLed at once aborts the compositor process (Chromium's FATAL "GPU process
  isn't usable", SIGTRAP), 3 of 3, before C11's next child poll. So no `CefCrashed{g}` or
  `CefReady{g+1}` follows, and case 8 asserts case 9's shape instead. The rewrite, and the in-place
  restart's own cases, are in `docs/tasks/C11b-cef-health.md` ("C14's case 8, rewritten").
- *Why.* "Heartbeats go `graphics: restarting` → `ready`" asks a 5 s beat to land inside a ~1 s
  state. After a kill, the domain reads `restarting` only for the first back-off,
  `cef_backoff_base_ms` = 1 s. It reads `starting` for the ~2.8 s load (F-001). Before C11's 1 s
  pid poll notices the kill, it still reads `ready`. About one beat in five sees `restarting`,
  which is the same point case 2 makes.
- *What it asserts instead* is the restart evidence, never a beat in the window:
  - The compositor log has `CefCrashed{gen: g}` within 2 s of the kill (the 1 s pid poll plus
    slack).
  - Then `RestartGraphics{gen: g+1}` and `CefReady{gen: g+1}`, in that order.
  - Every beat from the kill to `CefReady{g+1}` reads `ready`, `restarting` or `starting`, and
    none reads `failed`.
  - The first beat after `CefReady{g+1}` reads `ready`.
  - **Anti-vacuity:** `g+1` is exactly one generation past the last ready before the kill.
- *A false premise in the coordinator's wording.* The request was to assert on "the exit/restart
  evidence the web reads (exit events and restart_count)". Case 8 has no process exit: CEF's
  children die, the compositor process lives and restarts the page itself, so Fly writes no exit
  event and no `restart_count` moves. That evidence belongs to case 9, below. F-007's ~1.3 s
  window is the Machine restart, also case 9.

**Case 9 (`process-killed`) gains the exit evidence.**
- It reads the harness's equivalent of what web reads off Fly's Machine GET in production
  (`events[]`: the newest `exit` with `request.exit_event` and `request.restart_count`; R2003b).
- Since compose's `on-failure:2` restarts the container, `docker inspect compositor
  --format '{{.RestartCount}}'` rises by exactly 1 across the kill.
- The `die` event of `docker events --filter container=compositor --filter event=die` carries a
  non-zero `exitCode`.
- These are read after the restart completes, never from a beat inside the ~1.3 s gap. The live
  Fly version of this read is C17 row 12.

**Case 5 (`dest-stall`): the bound reads progress, not acks (Owed 13/15).**
- `output_ack_stall_ms` becomes `output_stall_after_ms` (C04's rename).
- The bound is `output_stall_after_ms + OUTPUT_PROGRESS_POLL_MS + heartbeat_interval_ms`, with the
  poll term taken from C09's recorded choice in `thresholds.json`, never a literal.

**Case 12, `dest-trickle` (Owed 16(b)).**

| # | File | Script | Verdict (`.jq`) |
|---|---|---|---|
| 12 | `dest-trickle` | At 20 s, `netem out rate 8kbit`: the link stays open and bytes still trickle. At 50 s, `clear`. Run 80 s | A `sinkSwaps` increase appears in a beat ≤ case 5's bound after 20 s, so a trickle is a stall, not progress. Output recovers after `clear`, as in case 4. **Anti-vacuity:** the compositor log's `OutputProgress` totals between 20 s and the swap take at least 2 distinct values, which proves the bytes really trickled, and the case is not case 5 again |

- A red case 12 is the witness Owed 16 asks for. It reopens C04 for a rate floor, and the `.jq` is
  never loosened.

**Mutants (Step 5):**
- 4, as above: delete C05b's Ready hang arm. Expected red: case 11.
- 5. Comment out case 12's `netem` line. The `.jq` must fail **on its anti-vacuity clause**, as with
  mutant 3.

**Acceptance (this amendment):** 12 cases, each with an anti-vacuity clause, and 5 mutants red.

## Amendment 2026-10-20 (C11b: the in-place restart's cases and its outage measurement)

C11b (PR #34; `docs/tasks/C11b-cef-health.md`, "As built") restarts the process in place when CEF's health asks
for it: a dead zygote (either one), `cef_gpu_deaths_to_restart` (~~4~~ 3, the PR #34 review's I1) GPU-process deaths, or `cef_leaks_to_restart` (3)
teardowns leaked at once. The process sends its final beat (`exitReason: "fatal"` until web adds a word, Owed 66;
`exitDetail` starting `restart in place: `) and exits **75**; compose's `on-failure:2` (Fly's `on-failure` in
production) starts it again on the same session.

**Case 8 (`cef-killed`)** takes case 9's shape (Owed 62, unchanged), plus: the `die` event's `exitCode` is non-zero
and not 75 (an abort: SIGTRAP, which tini reports as 128 + 5 = 133), and **no core file** lands (Owed 68).

**Cases 13–16, the in-place restart** (every number from `thresholds.json`; each a fresh compose up):

| # | File | Script | Verdict (`.jq`) |
|---|---|---|---|
| 13 | `cef-gpu-zygote-killed` | At 20 s, SIGKILL the compositor's child zygote that parents the `--type=gpu-process` child (read from `/proc` in the container: `ppid` = the compositor, `--type=zygote`). Run 60 s | The log has `cef health zygote_gone=<g>` within 2 s of the kill (the 1 s child poll, a tick, slack); the final beat carries `exitDetail` `restart in place: a CEF zygote is gone (gen <g>)`; the `die` event's `exitCode` is 75 and `RestartCount` rises by exactly 1; the log has no `GPU process isn't usable` (the process must not abort); output resumes, the same `SESSION_ID`, `counter_regressions == 0`; the outage (below) is recorded and judged; no core file. **Anti-vacuity:** the killed pid was a zygote that parented the GPU process at the kill |
| 14 | `cef-renderer-zygote-killed` | As 13, the zygote that parents the renderer | As 13; after the restart no process from the old tree is left (every `--type=` process is younger than the restart) |
| 15 | `cef-gpu-deaths` | From 20 s, SIGKILL the `--type=gpu-process` child `$t.cef_gpu_deaths_to_restart` times (amended: was 4), each time once a NEW pid has replaced the last (Chromium's relaunch), at least 2 s apart. A second run kills it one time fewer. A third run, **the fast loop** (the PR #34 review's gap; amended 2026-10-20, its N1: ~~`$t.cef_gpu_deaths_to_restart` kills~~, which stopped at the threshold, so the lag it measures was always 0): kills, each the moment a new pid appears (a tight `/proc` loop, no wait), until the process exits or 5 are done (one short of Chromium's abort at the 6th death). Run 90 s | At the threshold: `cef health … gpu_process_deaths=<n>`, the restart as in 13 (`restart in place: <n> CEF GPU-process deaths`), never a `GPU process isn't usable`. One fewer: no restart and no exit in the run, `gpu_process_deaths=<n−1>` logged. The fast loop: the restart, never an abort. Each kill is stamped on the clock `docker logs --timestamps` uses, and the verdict splits the kills at the `cef health … gpu_process_deaths=<n>` line that decided the restart: **unread** = the kills at or before that line − n (how far the 1 s child poll lags a real loop), and **at the exit** = the kills after that line and before the `die` (deaths during the final beat's flush, while CEF still runs and Chromium relaunches). Both are printed. Owed 70 (a) budgets one survivable death to each, so each of these is a red and a finding for the owner: unread over 1; at the exit over 1; the loop's 5th kill before the `die` (`at the exit` is then a floor, and an uncapped loop's next death is Chromium's abort at the 6th); 5 kills with no restart; unread below 0 (the log counted a death no kill made). (Amended 2026-10-20, the PR #35 review's m5: the first wording judged the kills before the `die` against the deaths named, which adds the exit's deaths to the lag, and its "lag over 2" could never fire, a restart naming at least 3 of at most 5 kills.) **Anti-vacuity:** each kill's pid differs from the one before |
| 16 | `cef-leaks` | 3 leaked teardowns at once, then 2 (a second run). ~~It needs C11's `hold_teardowns` test hook, which a release image does not carry: a harness-only build with the `test-hooks` feature and a control to hold every teardown, or the case is reported as not runnable and the owner told (never faked)~~ (amended 2026-10-24, Owed 82: C11's `hold_teardowns` cannot hold a teardown for this case, docs/_INDEX.md false premise "C11's `hold_teardowns` hook holds a teardown for case 16". The case runs on the `relay-test-hooks` image with `COMPOSITOR_TEST_HOLD_TEARDOWNS`, driven and judged as the amendment "Owed 82's handoff" below sets out; Owed 82's row has the same list) | 3: the restart as in 13 (`restart in place: 3 CEF teardowns leaked`); 2: no restart, `leaked_teardowns=2` logged |

**The false-positive sweep** (the PR #34 review's m4; Owed 70 (f)): since C11b a false zygote verdict, or a GPU death
counted that did not happen, costs an outage and (until Owed 66) a web crash, where before it cost a log line. Every
case without a CEF kill (1–7, 9–12) asserts, beside its own verdict: no `cef health zygote_gone=<g>` line (only
`zygote_gone=-`, or no `cef health` line at all), every `cef health` line reads `gpu_process_deaths=0`, and no exit 75
in the run (`docker events`). The table prints the number of runs swept; 0 is a red.

**The outage measurement** (C11b's "outage budget", corrected by its false premise 4):
- **What is measured**, per restart case and run, from the strip and the logs:
  - the outage: the last programme packet before the exit to the first after the restart (the strip's counter gap,
    `output_outages[]`);
  - its parts: the decision (the `cef health` line) to the `die` event (the final beat's flush, bounded by C08's
    `FINAL_BEAT_TIMEOUT_MS`), the `die` to the container's `start`, and the `start` to the first programme packet
    (bootstrap, pipelines, the first sink connect).
- **The budget** is `BUDGET_PROCESS_RESTART_OUTPUT_MS` (15 s), which is `OUTPUT_GAP_BUDGET_MS`: YouTube kept the
  organiser's broadcast through a 15 s gap and split it into a new broadcast after a 30 s one (F-004 addendum). The
  60–90 s auto-stop ends the broadcast outright, but the split comes first, so 15 s binds. Three runs of each case;
  the verdict judges the worst.
- **Compose is not Fly.** Compose's restart is not F-007's Fly restart (exit to started 1.4–1.5 s, the Machine
  rebooted). The live number is C17's (a C11b restart on a stg Machine, read like row 12); this suite's number bounds
  the compositor's own part.
- **The fallback condition** (the coordinator's decision in the stub, narrowed by the final C11 review's G3): if a
  case's worst outage is over 15 s here, or on Fly in C17, the measurement goes to the owner as a finding (the budget
  and the `.jq` never move). The fallback "stay on air without the overlay" may be put to the owner **only for case
  14's trigger and case 16's** (the renderers' zygote; the leaks). For case 13's (the GPU zygote) and case 15's (the
  GPU deaths) there is no safe fallback: the process stays armed to abort (off air at the kernel's moment, with a
  core), so the restart stays whatever it costs. Building the fallback would be a domain change (the restart causes
  split into the abort-armed and the degraded), C11b's code, not this suite's.

**Mutants (Step 5):** 6. In a scratch build, make `restart_cause` return `None` for a zygote: expected red, case 13 on
its `exitCode` clause (the process then aborts at the next GPU death, or runs on with a dead CEF). 7. Set
`cef_gpu_deaths_to_restart` to 6 in the scratch build: expected red, case 15 (the abort comes first). As with mutant
1, the verdict reads the committed `harness/thresholds.json`.

## Amendment 2026-10-24 (as built: branch `feat/c14-chaos`; the coordinator's dispatch of 2026-10-23)

The suite has **16 cases**. The title, Files, Step 7 and Acceptance said 10 or 12, but cases 13–16
came with the 2026-10-20 amendment, so those lines are corrected inline. It runs in CI only (owner
ruling 2026-10-23: no local Docker), so Steps 4 and 6 are `chaos.yml` runs, not local runs.

**What is built** (the coordinator's ruling: C14 owns the harness extensions it needs):
- `harness/cases/<case>.sh`, one file per case. Each holds the case's drive, its marks and its
  verdict.
  - The verdict is a list of `require` lines. Each line is jq over the checker's `report.json`,
    using `harness/cases/lib.jq`, with `$t`, `$run`, `$case` and `$timeline` bound.
  - Every number comes from `harness/thresholds.json`.
  - A case declares its check count, and a verdict that runs fewer checks fails.
- `harness/run-case.sh <case> [<variant>]` is C13's runner, extended. It adds:
  - docker events, reduced to die and start with exit codes;
  - `docker logs --timestamps`;
  - `RestartCount` at live and at the end, fake-control's `/control/state`, and the cores check;
  - the case's marks;
  - `CASE_REJUDGE=1`, which re-judges a downloaded `out/<run>` with no stack;
  - `CASE_NOT_RUNNABLE`, which reports and exits 3.
- `harness/in-container.sh` makes every kill from `/proc`, as root in the compositor's container.
  The relay image has no procps, and it is not changed for the harness.
- `harness/chaos.json` lists 31 legs: the 16 cases, case 15's three runs and case 16's two. Cases 8,
  9, 13 and 14, and case 15's `threshold` and `fast` runs, run 3 times each.
- `.github/workflows/chaos.yml` runs each case, variant and repeat as its own matrix leg, with
  fail-fast off. It runs on pull requests that touch `harness/**`, `crates/harness/**` or the
  workflow, and on `workflow_dispatch` (`cases=all`, or a list of legs).
- `harness/run-chaos.sh --table` prints the table.
  - It goes red on FAIL, ERROR or MISSING, and on a false-positive sweep that ran on no leg.
  - It reports NOT RUNNABLE and never counts it red.
  - It prints the worst outage of each repeated case.
- `crates/harness/tests/case_verdicts.rs` simulates each case's run from the product's declared
  behaviour, runs it through `checker::analyze`, and re-judges it with run-case.sh.
  - Every good run passes, printing every declared check.
  - Each of 31 red runs fails on its named check.
  - The two case 16 runs report NOT RUNNABLE.
  - `crates/harness/tests/chaos_table.rs` checks the plan and the table.

**Wave gate 3 (three consecutive green full runs):** run
`gh workflow run chaos.yml --ref <branch> -f cases=all` three times. Dispatch each run only after the
previous one has finished green. Paste the three tables from the table job's summary.

**Deviations** (the implementer's, inside the dispatch; none is an owner ruling):
1. The budgets live in `crates/harness/src/thresholds.rs`, not `crates/domain/src/config.rs`
   (Step 1), because the dispatch kept the domain read only. The drift gate is a test
   (`harness::thresholds` and `tests/thresholds.rs`), not a check.yml step (Step 2), because
   check.yml was another PR's.
2. Verdicts are `require` lines in each case file, sharing `lib.jq`, not one `.jq` file per case.
3. `chaos.yml` runs on harness pull requests and on dispatch, as the dispatch set it. It has no
   nightly `schedule` (Files).
4. `run-chaos.sh` tables the verdicts of the parallel legs; it does not run the cases in sequence.
5. Case 6's stress is a stress-ng container beside the stack. It competes for the host's CPU, not
   from inside the compositor's cgroup. Only the harness image gains `stress-ng`.
6. Case 11's detect bound adds `BUDGET_FRAME_TO_ENCODER_MS`. The hide is judged on the recording's
   strip, and that frame reaches MediaMTX after the decision.
7. Cases 13 and 14 run for 70 s, not 60, to fit the restart's outage (up to 15 s) and case 14's
   process-tree read at 60 s.
8. Case 8's restart evidence (Owed 19) is the new process's own `cef ready`, and every beat after
   the kill reads `ready`, `restarting` or `starting`. Since Owed 62 the kill takes the whole
   process, so the old process has no generation g+1.
9. Case 3's return is timed from the first content gap that ends after the kick's mark. The mark
   is stamped before the kick lands, so the gap can start just after it.

**False premises:** see `docs/_INDEX.md`, rows dated 2026-10-24.

**Mutants** run on the throwaway draft PR `mut/c14-r1`. Each needs its own named FAIL line, plus a
control check that proves the stack ran:

| # | Mutant | Case | Required |
|---|---|---|---|
| 1 | `resume_after_ms` 0 in a scratch relay build; the verdict reads the committed thresholds.json | `camera-flap` | `FAIL flap_hysteresis` |
| 2 | Comment out case 8's kill line. Replaces "disable the held overlay frame", which is stale: since Owed 62 the kill takes the process, and no held frame is judged | `cef-killed` | `FAIL kill_applied` |
| 3 | Comment out case 1's `/pause` | `camera-stop` | `FAIL camera_paused` |
| 4 | Delete C05b's Ready hang arm (`on_air`'s Ready branch) in a scratch relay build | `cef-hung` | `FAIL one_hang_hide` |
| 5 | Comment out case 12's `netem` line | `dest-trickle` | `FAIL trickled` |
| 6 | `restart_cause` returns nothing for a zygote, in a scratch relay build | `cef-gpu-zygote-killed` | `FAIL died_in_place` |
| 7 | `cef_gpu_deaths_to_restart` 6, in a scratch relay build | `cef-gpu-deaths threshold` | `FAIL died_in_place` |

## Amendment 2026-10-24, fix round 1 (the PR #52 review; chaos run 40045826841)

The review (NEEDS FIXES) read the first CI run: 7 PASS, 2 NOT RUNNABLE, 18 FAIL, 4 hung. This
section supersedes the lines of "Amendment 2026-10-24" it names. The rows are in `docs/_INDEX.md`
(decision "C14 fix round 1", false premises dated "C14 fix round 1", Owed 79, 82, 83 and 84) and
`_FINDINGS.md` F-023.

**What changed:**
- **Kills (C1).** `harness/in-container.sh` reads a process's `--type=` by the product's rule
  (`split_cmdline` in `crates/adapters/src/cef_graphics.rs`, mirrored): a NUL-only split found no
  CEF child, because Chromium's renamed children hold one space-joined string. Every kill prints
  its targets before it kills (`kill` mark) and its status after (`kill_rc`). A kill with no target
  fails the leg. `crates/harness/tests/in_container.rs` runs the script on a fake /proc of real
  space-joined CEF 139 command lines.
- **Case 5 (I1).** The witness is the netem qdisc's backlog (`netem.sh stats`: `backlog_bytes`,
  `backlog_pkts`), not its Sent count.
- **Case 12 (I2).** The witness is the qdisc's sent packets (at least 2) and the swap. The beats'
  "2 distinct egressBytes" is withdrawn: a correct swap ends that window after one or two beats.
- **Case 6 (I3).** Replaces deviation 5. The burn runs inside the compositor's container
  (`in-container.sh burn`, 4 workers per CPU of its quota), so it shares the compositor's cgroup.
  `ladder_moved` requires a beat at `overlay_half_rate` and droppedFrames up by at least
  `ladder_overruns_to_step`; a burn that moves nothing is a FAIL. The harness image is unchanged.
- **Cases 5 and 12 (I4).** `stall_swap` times the swap from the log's `sink swap` line against the
  last beat whose egressBytes grew, within `output_stall_after_ms + output_progress_poll_ms`. A
  doubled threshold reds, except in about 1 run in 20 by phase.
- **Budgets (I4, m7).** Each `BUDGET_*` is pinned to the sentence its source states (spec §6, or
  this brief), so a budget edited and regenerated reds `cargo test` (Owed 83).
- **Cases 1 and 2 (I5).** fake-camera sends a key frame at every resume, so `suppressed_ms` no
  longer counts a decoder's wait for the next key. Case 2 pauses
  `(slate_after_ms + input_stall_after_ms) / 2` (4 000 ms), one flap every 10 s. Its floor is
  `resume_after_ms − BUDGET_FRAME_TO_ENCODER_MS`: the hold-off starts at the first frame's arrival,
  and the frame shown at the switch is up to that much older.
- **NOT RUNNABLE (I6, the coordinator's ruling).** Replaces "It reports NOT RUNNABLE and never counts
  it red". A NOT RUNNABLE leg fails in `chaos.yml`. The table ends with a wave gate 3 line, and with
  case 16 unrun it reads `WAVE GATE 3: BLOCKED: 2 NOT RUNNABLE (...; Owed 82, the owner's ruling)`
  and exits 1. The false-positive sweep must run on every leg that calls it, not on any one.
- **Case 15 (I8).** `gpu-loop` has a deadline: the drive's end less
  BUDGET_PROCESS_RESTART_OUTPUT_MS. A loop that reaches it fails `kill_loop_in_time`.
- **Cases 4 and 12 (I9).** The two extra outages are product behaviour plus MediaMTX's recorder:
  a sink that connects after the clear sends its backlog, and the recorder discards it (F-023).
  The verdicts are not loosened. Owed 84 is for the owner.
- **Case 11.** Replaces deviation 6. The detect bound is the brief's again (`cef_hang_after_ms` +
  one frame period): CI read the strip 6 ms behind the log.

**Wave gate 3** cannot count a run while case 16 is NOT RUNNABLE (Owed 82), nor while cases 4 and
12 are red (Owed 84).

**Mutants**, two added to the table above:

| # | Mutant | Case | Required |
|---|---|---|---|
| 8 | `in-container.sh` splits each command-line field on NUL only (CI's first build, C1) | `cef-killed` | `FAIL kill_applied` |
| 9 | `output_stall_after_ms` 10 000 in a scratch relay build; the verdict reads the committed thresholds.json (I4) | `dest-stall` | `FAIL stall_swap` |

## Amendment 2026-10-24, fix round 2 (chaos runs 40051995233 and 40051902711; mutants 40051985045)

Both chaos runs were on 90f8a5b. Every red was read from its artifacts before anything changed.
This section supersedes the lines it names. The rows are in `docs/_INDEX.md`: decision "C14 fix
round 2", false premises dated "C14 fix round 2", Owed 70, 84, 85 and 86. The measurements are in
`_FINDINGS.md` F-024.

**What changed:**
- **Kills (items 1 and 3).** `in-container.sh` died before it printed or killed. Under bash 5.2
  with `set -e`, `$(<"/proc/$pid/stat")` on a vanished pid ends the script, even inside `||`.
  Every /proc read is a builtin `read` or `mapfile` now. `in_container.rs`'s fake /proc gains a
  vanished pid that sorts first.
- **Case 8 (item 4).** Replaces "non-zero and not 75" (amendment 2026-10-20).
  - Since F-022's listing at ready, C11b's zygote verdict can beat Chromium's abort. CI read 75 in
    1 run of 6.
  - The owner's ruling "B" (Owed 56) asks for that restart.
  - `died_to_restart` accepts any non-zero exit, and accepts 75 only with C11b's own final beat
    (`exitReason` "restart").
- **Case 14.** `old_tree_gone` requires the kill mark. Without the mark, `starts_after(null)` is
  the run's first start, so every tree passed.
- **Case 15, fast.** `deaths_logged` shows the deciding line and the kills, against the most deaths
  any line counted. The check itself is unchanged.
- **chaos.yml (item 6).** The NOT RUNNABLE error line is built by jq from the verdict. The step's
  source, which GitHub prints in every leg, no longer carries the words.
- **run-case.sh.** It keeps the names from `docker diff` in `container-diff.txt`, for Owed 85.

**Product findings, not loosened:**
- **Owed 70 (a).** The fast loop's 5 kills took about 330 ms, and the poll counted 1.
- **Owed 85.** CEF fails to start in 13 of 18 container restarts, after every kind of exit.
  - The cause is unproven. What is measured: a restarted entrypoint skips its Xvfb wait, because the
    previous Xvfb's socket persists on the container's writable layer.
  - Cases 13–15 never ask for the overlay back. The recommendation is in that row; it is not built.
- **Owed 86.** Under the burn, the window ran at 11.5 and 24.7 fps, and every beat read fps 30,
  droppedFrames 0 and rung `full`.
- **Owed 84 extends to case 5.** It depends on the clear's phase. A clear that lands within a
  pending connect's 5 s timeout bursts and resets the recorder, and one that lands after the timeout
  does not.

**Expected reds** until those rows are ruled:
- cef-leaks-two and cef-leaks-three (Owed 82);
- dest-disconnect, dest-stall and dest-trickle (Owed 84; green only by phase);
- cef-gpu-deaths-fast ×3 (Owed 70 (a));
- cef-killed ×3, wherever the restart's CEF fails (Owed 85);
- cpu-starved (Owed 86).

**Mutant 9, retargeted.** Under case 5's 1 kbit, rtmp2sink's own socket timeout swaps the sink
before the stall rule fires, so a doubled threshold passed. Case 12's trickle is the only path to
the stall rule. 10 000 would survive about 1 run in 20 by the phase of the growth beat, so the
mutant takes 20 000:

| # | Mutant | Case | Required |
|---|---|---|---|
| 9 | `output_stall_after_ms` 20 000 in a scratch relay build; the verdict reads the committed thresholds.json | `dest-trickle` | `FAIL stall_swap` (controls `trickled`, `fp_swept_inputs`) |

## Amendment 2026-10-24, fix round 3 (chaos runs 40056474269 and 40056388445; mutants 40056471001)

The coordinator's fix-round-3 dispatch. Each red was read from its artifacts first.

**Case 14 (`old_tree_gone`, 6 of 6 red): a harness clock, not a surviving tree.**
- Every typed process in `tree_after` was the new tree's. Each was parented to the new compositor
  and started 130–450 ms after the new init.
- in-container.sh's `start_ms` was `btime` plus the start ticks. btime is whole seconds, fixed at
  boot, and it read 173–1 080 ms early against docker's events.
- `start_ms` is now the wall clock less the process's age, from /proc/uptime read beside it.
- The check anchors on the sampled init (pid 1), not on docker's `start` event:
  - the init must have started after the kill;
  - the sample must come after the `start` event;
  - every typed process must be younger than the init.
- The event is the daemon's report of the start, and CEF's first child starts 130 ms in.

**Cases 5 and 12 (`stall_swap`): the swap's cause (Owed 87).**
- In 4 of 4 CI swaps, rtmp2sink's own 5 s socket timeout failed the sink first. Its
  `output gen=1 error:` line came 5.53–5.78 s after the cut, and the swap followed after the
  500 ms back-off.
- So a bound alone cannot witness the stall rule: mutant 9's 20 s threshold swapped as fast.
- The `sink swap` line names no reason. A Connected sink is swapped by the stall rule or by its
  own bus error, and the error path always logs its error first.
- `stall_swap` therefore requires `cause` "stall": no `output gen=<g> error` from the outgoing
  generation before the swap (logfacts' `output_errors`).
- Both cases red on it until Owed 87's product change.
- **Mutant 9 is unkillable on a green baseline until then.** Its baseline reds `stall_swap`.
  It stays in the list, so the run records that.

**Cases 8 and 13–15: the overlay comes back (the coordinator's decision).**
- `require_overlay_back`: the first recorded frame whose score strip reads, after the restart's
  `start`, comes within BUDGET_PROCESS_RESTART_OUTPUT_MS of the die. The budget is spec §6's "a
  process crash's output is back within 15 s", which takes §9.4's "overlay back" with it.
- It is in `require_in_place_restart` (cases 13, 14, 15 threshold, 15 fast) and in case 8.
- Case 15 one-fewer has no restart, so it is timed from the last kill.
- CI's healthy restarts had the overlay back 2.1–2.2 s after the die. Owed 85's never did, so those
  legs red `overlay_back` until Owed 85 is fixed.

**The table (item 4).**
- chaos.json's `owed` names the Owed row behind each check a leg is known to fail.
- run-chaos.sh marks each failed check with its row, or NOT OWED.
- It lists the red legs whose every failed check is owed, the reds no row explains, and each named
  row's title.
- No verdict, exit status or gate line reads it.

**Expected reds:**
- cef-leaks ×2 (Owed 82, NOT RUNNABLE);
- dest-disconnect (84);
- dest-stall and dest-trickle (87, and 84 by phase);
- cpu-starved (86);
- cef-gpu-deaths-fast ×3 (70 (a));
- cef-killed, cef-gpu-zygote-killed, cef-renderer-zygote-killed and cef-gpu-deaths-threshold,
  wherever the restarted CEF does not come back (85).

## Amendment 2026-10-24, fix round 4 (the re-review of 8b71aef)

The coordinator's fix-round-4 dispatch. Harness only; every new guard mutated once (27 mutants,
all killed, each by its intended sim).

**Case 11 (`detect_ms`): judged on the log, the strip against the log (I-1).**
- This amends the 2026-10-18 bound (strip − last alive ≤ `cef_hang_after_ms` + one frame period),
  which had no term for the strip's dating (false premise row).
- The product's rule: the hide comes at the first tick at or after `cef_hang_after_ms` past the
  last alive. So `hide − alive` (both log lines) must lie in [`cef_hang_after_ms` − 1 period,
  + 2 periods]: the rule's one period, and one more each way for two docker stamps and a late tick.
- The strip's first overlay-less frame must lie within BUDGET_FRAME_TO_ENCODER_MS of the logged
  hide, either way: a strip time is a MediaMTX segment name (±17 ms of arrival jitter, F-021)
  plus the frame's offset.
- A beat late is +2 000 ms, so both still red any meaningful mutant (m4 among them).

**Wave gate 3 counts whole first attempts only (I-2, m-7).** A selection of legs or a re-run attempt
is green for what it ran and says it never counts. The table prints the attempt, the head and the
leg count.

**Owed tags by evidence (I-3).** chaos.json's `owed_signatures` gives each Owed row a jq predicate
over the leg's verdict.json. A failed check is tagged with a row only when that row's signature
holds in the run. Otherwise it reads `NOT OWED: Owed <r>'s signature absent`.

**Case 6: a quota, not a burn (I-4).** This amends the 2026-10-18 amendment's burn (and round 1's
16 workers).
- The drive reads the container's cgroup CPU time at 15 s and 20 s, and sets `docker update
  --cpus` to 0.75 of that demand from 20 s for 40 s. Then it puts the quota back.
- `stress_ran`: the quota was under the demand, held 40 s, and restored.
- `starvation_applied`, a drive witness: the window under the fps floor, a beat under
  `encoder_speed_realtime_min`, or an overrun counted. A starvation nothing felt is the harness's
  own red, and `ladder_moved` passes only on a starved window.
- The share is a drive parameter. Whether the ladder's one rung below full can hold the clock
  against it is unmeasured, so the owner sets it (Owed 86).

**Cases 4, 5 and 12: the clear's phase is printed (I-5).** `clear_phase` gives the last sink swap
at or before the clear, and whether a connect was pending then (under
OUTPUT_CONNECT_TIMEOUT_MS). It is not driven. Wave gate 3 still needs a burst-phase run among its
three, or a driven phase.

**Minors.**
- m-1: case 15 one-fewer's `overlay_back` (vacuous: it passed on the held frame) is replaced by
  `scheduled_changes_shown` after the last kill.
- m-2: `overlay_back` is timed from the restart outage's last packet, and searched after the
  container's start. Searching after the outage's end instead would read an old-process frame as
  new wherever the outage ends before the start, so that half is not applied.
- m-3: a report with no `output_errors` reads cause "unknown", a red.
- m-4: Owed 84 (c) records why case 4's `lost_ms` reads 29 800 (the checker's double count). The
  measure's fix is owed before wave gate 3.
- m-5: case 14's `old_tree_gone` cannot see an orphan of the old tree. A restarted container is a
  new pid namespace, so it fails only without a restart, which `died_in_place` and
  `restarted_once` judge too. It stays as the sample's anchoring check.
- m-6: an owed row whose `_INDEX` line carries CLOSED fails chaos_table.

**Mutants.** m10 is added: case 6's `docker update` replaced by `true`. `starvation_applied` must
red, and `stress_ran` must pass.

**Expected reds:**
- cef-leaks ×2 (Owed 82, NOT RUNNABLE);
- dest-disconnect (84 where the clear lands in a pending connect; NOT OWED otherwise);
- dest-stall and dest-trickle (87, and 84 by phase);
- cpu-starved (86 when starved; the drive's own red otherwise);
- cef-gpu-deaths-fast ×3 (70 (a));
- cef-killed, the two zygote cases and cef-gpu-deaths-threshold, wherever graphics gave up after
  the restart (85).

## Amendment 2026-10-24, rebase onto main 263b08a (PR #54, Owed 85's fix, merged)

The coordinator's rebase dispatch. `feat/c14-chaos` is rebased onto main 263b08a, never merged.

- **Owed 85 leaves chaos.json.** Its signature, its title and its tags on cef-killed (3 checks),
  the two zygote cases, cef-gpu-deaths-threshold and the fast leg's `overlay_back` are gone. Every
  restart leg's `overlay_back` must now pass; a red there reads NOT OWED and is news.
- **`graphics_failed_after_start` is still measured** beside `overlay_back`. It is the evidence a
  return of that cause would carry.
- **A list of rows behind one check** (tried in order) is no longer carried by any leg. chaos_table
  pins it over a copy of the manifest (`CHAOS_MANIFEST`, read by run-chaos.sh for that test alone;
  chaos.yml never sets it).
- **The round-4 expected reds amended:** the last bullet's 85 is withdrawn. Those legs are expected
  green.
- **_FINDINGS.** F-023 and F-024 sit before main's F-025, and F-024's unproven cause points at F-025.
  Row 85 keeps main's text ("IN REVIEW 2026-10-24 …"). The dispatch's "FIXED text" is not on main:
  closing the row needs a green image run's id, and that is main's to record.

## Amendment 2026-10-24, rebase onto main 567cfcc (PR #57 and PR #55 merged)

The coordinator's dispatch after the confirm pass of a69b6bb (PASS). `feat/c14-chaos` is rebased
onto main 567cfcc, never merged. Both sides of the docs are kept: #57's F-027, F-028, Owed 84, 87
and 88 and its decision rows; #55's F-026, Owed 70 (a) and its decision rows; this branch's F-023
and F-024.

- **Owed 84 and Owed 70 (a) leave chaos.json**, as Owed 85 did. Their signatures, titles and tags
  are gone: `output_lost` and `no_outage_outside_fault` in cases 4, 5 and 12, and every tag on case
  15's `fast` leg. A red there now reads NOT OWED. That is the honest result of the re-run, never
  re-tagged.
- **What stays owed:** 86 (case 6's four checks), 87 (`stall_swap` in cases 5 and 12) and 82 (case
  16, NOT RUNNABLE). 84 (c), the checker's double count of the cut (the re-review's m-4), has no row
  or tag of its own.
- **Owed 85 is closed FIXED** in docs/_INDEX.md: PR #54 merged as 263b08a, main's image run
  40079483379 is green, and chaos runs 40087523077 and 40087535051 (a69b6bb) are green on all 24
  restart legs, `overlay_back` included. This supersedes the previous amendment's last bullet.
- **`CHAOS_MANIFEST` never counts.** A table over another manifest prints it on the run line, and
  the gate line reads "a run over another manifest (CHAOS_MANIFEST) never counts", even all green.
  chaos_table pins it beside the committed manifest's "this run counts" on the same verdicts, and
  the refusal is mutated once.
- **The confirm pass's minors.** c-1 (84's signature reads the phase, not the reset) is moot now
  that the signature is gone. c-2: `stress_ran` reads the drive's own marks, so it is commented as
  the quota requested, with `starvation_applied` as the witness that it was applied.
- **Expected reds of the re-run:**
  - cef-leaks ×2 (Owed 82, NOT RUNNABLE);
  - cpu-starved (86);
  - dest-stall and dest-trickle `stall_swap` (87).
  - Anything else is NOT OWED, including case 4, 5 or 12's `output_lost` (84's prediction says
    green) and case 15's `fast` leg (70 (a)'s says green).

## Amendment 2026-10-24, Owed 82's handoff (PR #62, the held-teardown hook; its review's I2, I3 and m1)

PR #62 (branch `feat/owed82-hold-teardowns`) adds the hook case 16 needs (Owner, 2026-10-24, option
A, in chat, relayed by the coordinator; docs/_INDEX.md decision log "Owed 82: the held-teardown
hook"). This amendment is what C14 wires once it merges. Owed 82's row in docs/_INDEX.md carries
the same list.

- **The image and the key.** Build the two cef-leaks legs' `RELAY_IMAGE` with
  `--target relay-test-hooks`; chaos.yml builds the default target, the relay image, today. Pass
  `COMPOSITOR_TEST_HOLD_TEARDOWNS: ${COMPOSITOR_TEST_HOLD_TEARDOWNS:-}` in compose's compositor
  `environment`, set to 3 for `three` and 2 for `two`. The relay image ignores the key.
- **The arming line is a precondition.** `WARN test hook (Owed 82): the next <N> CEF teardowns
  never return` appears once, before `compositor started`, with N equal to the leg's. If it is
  missing, the leg is a red, never a pass and never NOT RUNNABLE. Otherwise a leg built on the
  plain relay image reads as "0 leaked".
- **The driver is `kill-renderer`.** It is new in `harness/in-container.sh` (C14's code). Today
  the script has only `kill-cef` and `kill-zygote gpu|renderer`.
  - It SIGKILLs the `--type=renderer` child with the highest `--renderer-client-id`.
  - The highest id is the newest build's renderer. Client ids only grow, and a page's reload keeps
    its id (crates/adapters/src/cef_graphics.rs module doc, "Renderer attribution", F-013).
  - It prints its targets as JSON: the pid killed, its client id, and every renderer seen.
  - **Anti-vacuity:** exactly 1 killed per drive.
  - Before each kill, wait for the current gen's `cef ready gen=<g>`, where `<g>` is the gen of the
    newest `gen=<g>: loading` line. Never wait for any `cef ready`: a held page's reload also logs
    `cef ready gen=<old>` (below).
  - Killing every renderer would also work, because any extra target belongs to a retired build.
    It is not needed.
- **The chain from a renderer kill to a retirement is established.** Do not measure it. A renderer's
  exit reaches cefsrc's `OnRenderProcessTerminated`, which the adapter reads as `RendererGone` and
  then `Crashed`. That is CefCrashed, proven on real cefsrc by `crash_of_the_renderer_reports_cefcrashed`
  (crates/adapters/tests/graphics.rs). The domain counts it as a Failure, backs off, and sends
  `RestartGraphics`, and `restart` retires the old build: one held teardown.
  - The third crash in 5 minutes is Failed, then Hide(Failed), then `graphics.stop()`. That stop is
    the third retirement.
  - `three` is therefore three kills, and `two` is two.
- **Never the page-hang driver.** `POST /control/page-hang` is one-shot: a second POST answers 409
  `ALREADY_ARMED` (crates/harness/src/control.rs). It is also served to the first page that polls.
  A held build's reloaded page still polls `/page-control.json`, so it can take the arm, and then
  the current page never hangs.
- **Verdicts are keyed on gen.** Read these lines for each held teardown:
  - `WARN test hook (Owed 82) gen=<g>: this CEF teardown never returns (<left> more to hold)`, with
    `<left>` counting down from N−1 to 0;
  - the reaper's `WARN gen=<g>: teardown has not returned after <ms> ms; SIGKILL sent to renderer
    pids [..] (of [..])`;
  - `... (<k> leaked; Owed 55)` about 6 s after the retirement (`REAPER_KILL_AFTER_MS` +
    `TEARDOWN_LEAK_AFTER_MS`, the adapters' constants, 3 s each today). thresholds.json does not
    carry them yet. Every number comes from it, so `dump-thresholds` gains both.
- **Each held teardown's reaper line lists at least 1 killed pid.** This is a required verdict (the
  PR #62 review's I2). The hook parks before `set_state(NULL)`, so cefsrc is still live and the
  reaper kills its renderer, exactly as for a real stuck teardown. A hold after the NULL would close
  the browser first, and the line would read `pids [] (of [])`. The adapters' unit tests have no
  renderer. They witness only the hold's place: the held build's pipeline is still PLAYING at its
  leak count. So this verdict is the only witness of the fidelity claim on the real image.
- **A held page lives on after the kill** (the review's m1).
  - The held build is still PLAYING, so the reaper's SIGKILL makes cefsrc reload its page under the
    same client id, and nothing kills it again.
  - Every held teardown leaves a live overlay page for the rest of the process. It renders at nice
    10, polls fake-control, and its frames stay off air.
  - Its reload logs `gen=<old>: ready (retired or crashed: its frames stay off air)` and
    `cef ready gen=<old>`. These are expected, not a red.
  - This is the product's own leak model ("its thread, pipeline and renderer stay"). The PSS cap
    does not count another build's renderer, so the cap is not inflated.
  - **Report the CEF children's CPU in `two`**, from `/proc` (the container's `cpu-usage` is only a
    ceiling). Print it, never judge it: no budget exists for it.
- **Count retirements, not driver kills.** Every retirement takes one from N, whatever its cause.
  A load timeout before the first kill eats one. Judge "held lines == N" and the leak count, never
  "held lines == driver kills", and print both.
- **`three`:** exit 75 (the `die` event), and the final beat's `restart in place: 3 CEF teardowns
  leaked`, read as in case 13. **Stop driving after the exit.** The restarted container arms N
  again, because the budget is per process, and a second 75 counts against `on-failure:2`.
- **`two`:** on air to its end at `cef health zygote_gone=- leaked_teardowns=2
  gpu_process_deaths=0`, with no exit 75.
- **Leave room before the session ends** (the review's gap hunt). A session-end `stop()` takes any
  budget left. If a leg achieves fewer than N retirements before the end, the last hold is the
  session-end stop, and its leak lands about 6 s later, inside the final hold and the drain, where
  the domain's answer to `LeakedTeardowns{3}` is pinned by no test. Drive all N with at least
  `REAPER_KILL_AFTER_MS` + `TEARDOWN_LEAK_AFTER_MS` + 10 s left before the session's end. If that
  outcome is seen anyway, record it as a finding.
- **`CASE_NOT_RUNNABLE` leaves `harness/cases/cef-leaks.sh`**, with verdicts for the lines above.
  Owed 82's tags leave chaos.json when both legs are green.
- **Mutant 8 (Step 5).** In a scratch `relay-test-hooks` build, move the hook's hold in `run_build`
  to after `set_state(NULL)`.
  - Expected red: both legs, on the "at least 1 killed pid" verdict.
  - The adapters' unit tests kill the same mutant on the held pipeline's state (PR #62's mutant
    run). This one proves that the case's own verdict is not vacuous.

## Amendment 2026-10-24, wave-gate-3 follow-ups (branch `feat/c14-wg3-followups`)

*Superseded for Owed 87 by the amendment "Owed 87 decided" below (the PR #73 review's m-6): the
`stall` path, case 17's premise and m9's old target are retired there.*

The coordinator's dispatch after #52 merged at 7beb540; rebased onto main b606d1b (#59: its two
Owed 81 controls are NOT_CHAOS; #60). Harness only; no product crate changed. Decision log "C14
wave-gate-3 follow-ups".

- **Owed 87, the coordinator's decision (medium, recommendation applied).** In cases 5 and 12 the
  sink's socket errors first, so a swap with `reason=error` inside the spec's output budget is a
  PASS. `stall_swap_judged($cut; $paths)` (lib.jq) judges either path from the driver's
  `sink swap reason=<r> gen=<g>` line (logfacts.rs `sink_swap_reasons`), and the reason lines must
  pair the swap lines by generation:
  - `stall`: no error from the outgoing sink, and the swap within output_stall_after_ms +
    output_progress_poll_ms of the last growth;
  - `error`: the outgoing sink's error line first, the stall seen within the same bound, and the
    swap from output_backoff_base_ms (the domain's back-off from Connected) to
    budget_output_reconnect_ms (§6, 5 s) after the error.
  Cases 5 and 12 accept both; Owed 87 leaves chaos.json, so a `stall_swap` red reads NOT OWED.
- **Case 17, `dest-stall-rule` (new; 14 checks; one run).** A destination that keeps the socket
  open and takes bytes, too slowly for a frame to leave inside the stall threshold, and never
  errors. `harness/compose.stall-rule.yml` caps the compositor's tcp_wmem at `4096 8192 8192` in its
  network namespace, added by run-case.sh's new `CASE_COMPOSE` knob (the name is validated, and the
  file must exist); the shipped image is unchanged. The rate is frame_bits × 1000 / (STALL_DRAIN_X
  × output_stall_after_ms), with STALL_DRAIN_X 2: 13 333 bit/s for the fixture. Its `stall_swap`
  accepts `stall` only. Anti-vacuity: `buffer_capped` (the namespace's tcp_wmem reads what compose
  set; at least one RTMP socket, each one's buffer read and at most the cap) and `trickled` (netem
  sent 2 packets or more, at a non-zero rate under one frame per threshold). The physics is read from
  rtmp2 1.24.2's source and was not run here (no Docker); the first CI run is its witness, and a red
  with `reason=error` means the cap did not hold the socket open (the measures
  `sink_errors_in_fault` and `sockets_in_fault` say how).
- **m-4.** The checker reads an outage's `counter_before` and `counter_after` from the frames on its
  own sides, bounded by its neighbours' splits (`checker.rs`, `Split`). A re-dated burst no longer
  counts the cut's camera gap a second time.
- **I-5.** Cases 4, 5 and 12 each run two legs. With no variant the drive clears half a connect
  timeout after a swap in the fault, so the last sink is still connecting; with `backoff` it clears
  just after that sink's own `output gen=<g> error:` line. `clear_out_in_phase` (run-case.sh) waits
  on the compositor's docker-stamped log, and `clear_phase_driven` reds unless the log's phase and
  the drive's mark both name the variant. The manifest has 23 entries, 35 legs.
- **m-r2.** Each NOT_CHAOS control must drive exactly smoke's `case_drive` and `CASE_LIVE_S`
  (case_verdicts.rs `every_not_chaos_control_drives_what_smoke_drives`).
- **Case 6** keeps the 0.75 share; Owed 86 goes to the owner.
- **Mutants.**
  - Every new guard was mutated once on the host: 31 mutants, all KILLED by their named test.
  - The checker's split order survived the first pass. Its test gained outages out of wall order, where a wall sort slices backwards.
  - Two clauses that nothing could kill were dropped as equivalent:
    - lib.jq's `error_to_swap_ms != null`: jq sorts null below every number.
    - `buffer_capped`'s three-value count: an unset cap reds on the buffer bound.
  - CI's m9 (`output_stall_after_ms` 20 000) now runs on case 17. Its target is `stall_swap`, with `buffer_capped` and `trickled` as controls. It must end KILLED (branch `mut/c14-wg3`).
- ~~**Expected reds of the next run:** cef-leaks ×2 (Owed 82, NOT RUNNABLE) and cpu-starved (86).
  Anything else is NOT OWED.~~ (False: case 17 red too. Withdrawn by the amendment below.) A red in
  case 17 is a finding about its physics, recorded in Owed 87's row, never re-tagged.

## Amendment 2026-10-24, wave-gate-3 review round 1 (PR #65)

The PR #65 review (Needs fixes: C-1, I-1 to I-3, m-1 to m-5) and the coordinator's dispatch;
rebased onto main 2537ddc (#62). Harness only. Decision log "C14 wave-gate-3 follow-ups", its
review-round-1 amendment.

- **Case 17's premise is disproved (C-1).** In 3 of 3 CI runs the 8 KiB cap applied and the
  in-fault Send-Q still held 16 806–16 810 B, and the swap read `reason=error` (false premises, same
  date). The coordinator stopped the harness fix: the rule's fate is the owner's (Owed 87, reopened,
  with the mechanism and the options). Case 17 stays as built until the owner rules; m9 stays on it
  and cannot be killed while it reds.
- **Owed 87 is restored to chaos.json (I-2).**
  - Tags: `stall_swap` in the 4 legs of cases 5 and 12 and in case 17, and case 17's `buffer_capped`.
  - Signature: `stall_swap.reason` "error" with `error_path_passes` true (lib.jq
    `stall_swap_shown`) and, where case 17 read it, `queue_past_cap` true.
  - So case 17's two reds read Owed 87. In cases 5 and 12 the error path passing passes the check,
    so a red there reads NOT OWED with the row named as absent.
- **Case 17's anti-vacuity judges the effect (I-1).**
  - `buffer_capped`: also at least one in-fault socket, each Send-Q read and at most the cap.
  - `trickled`: netem's measured bytes over the fault (the cut to netem's stats) under one frame's
    bits per stall threshold, with the rate shown.
  - Case 17 declares 16 checks and measures, and shows `clear_phase` and `publishes_from_clear_ms`
    (m-4: each publish after the cut, against the clear, from logfacts `sink_publishes`).
- **The stall path's floor (m-3).** A `stall` swap is at least output_stall_after_ms −
  frame_period_ms after the cut, in every case that judges it.
- **Sweeps (m-1, m-2, m-5).**
  - Each NOT_CHAOS control's stack knobs and exported CAMERA_* equal smoke's, but for its one
    documented camera variable (smoke and both controls run the `probe` profile).
  - Every `$t.<key>` and `$(threshold <key>)` read in lib.jq, run-case.sh and the case files is a
    thresholds.json key; a key missing, or no reads at all, fails.
  - The checker's outage count is counted.
- **lost_ms (I-3)** is Owed 92, not fixed here: the dating step moves it by about −2 s in 11 of 15
  destination legs, the size of `output_lost`'s budget.
- **Expected reds of the next run:** cef-leaks ×2 (Owed 82, NOT RUNNABLE), cpu-starved (86) and
  dest-stall-rule (87, both checks). Anything else is NOT OWED.
- **Case 16** (Owed 82's hook) is the next C14 task, in its own PR.

## Amendment 2026-10-24, Owed 87 decided (branch `feat/owed87-retire-stall`)

Owner, 2026-10-24, in chat, relayed by the coordinator: "A": the stall rule is retired, and rtmp2sink's own
socket timeout is the only detector of a destination stall (`docs/_INDEX.md` Owed 87, decision log
"Owed 87 decided"; C04's and C09's amendments of this date).
- **`stall_swap` judges one path.** `stall_swap_judged($cut)` (lib.jq) loses its `$paths`, the
  `stall` path and `stall_swap_shown`. In cases 5, 12 and 17 it passes when:
  - the swap reads `reason=error`, and the reason lines pair the swap lines;
  - the outgoing sink's `output gen=<g> error:` line came before the swap;
  - the swap came output_backoff_base_ms − frame_period_ms to budget_output_reconnect_ms (§6, 5 s)
    after the error (the floor less one frame: the docker stamps cannot resolve the product's tick;
    PR #71's run 40137725801 read 499 ms for a swap whose own `ts=` stamps were 500 ms apart;
    **amended 2026-10-25**, the amendment "Owed 94's backstop": the cause was docker's per-line read
    latency, and the interval is now read from the lines' own `ts=` with the floor exactly the base), and
    within `bound_ms` (output_sink_timeout_ms + output_backoff_base_ms + frame_period_ms) of the
    last growth;
  - the error came no sooner than output_sink_timeout_ms − frame_period_ms after the cut, and no
    later than output_sink_timeout_ms + budget_output_lost_max_ms (review round 1's m-1: what the
    sink writes after the cut goes into a socket nothing drains, and §6 lets a stall lose at most 2 s
    of programme; the growth bound alone, anchored on a beat, passes a timeout up to about 8–9 s).
  thresholds.json gains `output_sink_timeout_ms` and loses `output_stall_after_ms` and
  `output_progress_poll_ms`.
- **Case 17 (`dest-stall-rule`) witnesses the error path under its capped send buffer.** Its premise
  (only the rule can swap) is withdrawn. It stays to show that the sink's own timeout still catches a
  socket the cap holds open; if a kernel ever honours the cap inside a message, the sink stops
  erroring there and `stall_swap` reds. `trickled` bounds its rate per sink timeout. `buffer_capped`
  keeps the cap read and the sockets' buffers before the cut; the in-fault Send-Q is measured, not
  judged. That clause leaves by the ruling, not to get green: the queue past the cap is the measured
  physics (the PR #65 review's C-1) that the ruling rests on.
- **Owed 87 leaves chaos.json**: its tags on the 4 legs of cases 5 and 12 and on case 17, its
  signature and its title. A red on those checks reads NOT OWED.
- **m9** now sets `OUTPUT_SINK_TIMEOUT_MS` to 20 000 in a scratch relay build, on case 17, with
  target `stall_swap` and controls `buffer_capped` and `trickled`. The verdict reads the committed
  thresholds.json. It must end KILLED. **KILLED on CI (run 40136067430, PR #74), by the destination
  rather than the 20 s wait** (the PR #73 review's I-3; `docs/_INDEX.md` false premises): MediaMTX
  closed each publisher whose message could not complete (gen 1 `connection closed remotely`
  10 998 ms after the cut, swapped at 11 511 ms), and `stall_swap` red on the bound
  (`from_growth_ms` 6 732 against 5 533). `case_verdicts.rs` `rule-m9-ci` models that shape;
  `rule-m9`'s four timeouts model the sink's own 20 s wait, which CI did not show.
- **Host mutants, each with a named FAIL in a `case_verdicts.rs` sim:** lib.jq's `reason == "error"`,
  the back-off floor, the output budget, the bound from growth and the floor from the cut, each
  replaced by `true` (or the floor by 0); and review round 1's ceiling from the cut, replaced by
  `true`, red in `rule-late-error`. `error_to_swap_ms != null` is dropped as equivalent: jq sorts null
  below every number, so the floor reds a null.
- ~~**Expected reds of the next run:** cpu-starved (Owed 86, still in chaos.json); and cef-leaks ×2,
  NOT RUNNABLE until case 16's PR (#71) merges, then whatever case 16 reads (review round 1's m-5).
  No Owed 87 red is expected.~~ (Amended 2026-10-25, when #71 was rebased onto this: case 16 runs,
  so the one expected red is cpu-starved, Owed 86. See the amendment "case 16 wired".) Anything
  else is NOT OWED.

## Amendment 2026-10-24, case 16 wired (branch `feat/c14-case16-leaks`)

The coordinator's dispatch after #65 merged (832b3f1), rebased onto 67986ce (#69). Harness only.
Owed 82's handoff (the amendment of that name) as built, with four choices of the implementer's,
none an owner ruling. Decision log "C14 case 16 wired".

- **The image and the key.**
  - chaos.yml's images job also builds `--target relay-test-hooks`
    (`seazn-relay-test-hooks:<sha>`) and ships it beside the other two. Every leg gets
    `RELAY_TEST_HOOKS_IMAGE`; only case 16 reads it, by pointing `RELAY_IMAGE` at it. run-case.sh's
    `up: relay …` line then names the image the leg really ran.
  - **Deviation:** the key is passed by `harness/compose.cef-leaks.yml`, case 16's own overlay
    (`CASE_COMPOSE`, `${COMPOSITOR_TEST_HOLD_TEARDOWNS:?}`), not compose.yml's `:-` pass-through.
    So no other leg's compositor sees the variable at all, and a case that forgot to set it
    refuses to start.
  - `three` holds `cef_leaks_to_restart` (3) and `two` one fewer, both read from thresholds.json.
- **The driver** is `harness/in-container.sh kill-renderer`.
  - It SIGKILLs the renderer with the highest numeric `--renderer-client-id` (10 beats 9), and
    nothing else. It prints the pid, its id and every renderer seen (pid, id or null) first, and
    fails with `pid` null when no renderer has an id.
  - Each kill waits for the newest `gen=<g>: loading` above the last killed gen, then that gen's
    `cef ready`, within the back-off cap plus the load timeout. The drive ends after N kills, or
    once N held lines are in.
  - It stops without killing on a restart, a missing build or no room. The room is
    `REAPER_KILL_AFTER_MS` + `TEARDOWN_LEAK_AFTER_MS` + 10 s before the end, plus
    BUDGET_PROCESS_RESTART_OUTPUT_MS in `three`. The stop is marked (`drive_done`).
  - `cef-cpu` prints each CEF child's utime plus stime, and `two` samples it at 40 s and 55 s.
- **Verdicts** (`three` 24 checks, `two` 16), keyed on gen, in the first process's window (the
  restarted process arms N again, and its session-end stop is held):
  - `armed`: one arming line per boot, N each, each before its boot and after the one before.
  - `killed_one_current`: 1 to N kills, each the highest id among the renderers seen. Each came
    after the `cef ready` of the build its newest `loading` named, and each status is 0.
  - `held_n`: the held lines count N−1 down to 0, of distinct gens, every killed gen among them.
    Retirements are counted, not kills: a load timeout's retirement passes.
  - `reaper_killed`: each held gen's reaper line kills at least 1 pid, at
    `reaper_kill_after_ms`.
  - `leaks_counted`: the leak count reads 1..N over the held gens in order, each leak after its
    reaper line and before the end request.
  - `restart_as_ruled`: exit 75 exactly when N reaches cef_leaks_to_restart (owner ruling 4A).
  - The measure `retirements` (held, kills, gens, the drive's stop).
  - `three`: `leaks_health` (the deciding `cef health` line reads N, no zygote, no GPU death), the
    in-place restart with `restart in place: <N> CEF teardowns leaked`, its outage, and the same
    session.
  - `two`: `leaked_logged` (the last line before the end reads N), `overlay_on_air` (the build
    after the last kill ready, every score change due after it shown, graphics never failed),
    `on_air_to_end`, no restart, no abort, an ended beat, no counter regression, no core, and the
    measure `cef_cpu` (ms per s per child between the samples, never judged).
- **Thresholds.** `reaper_kill_after_ms` and `teardown_leak_after_ms`, from the adapters'
  constants through `dump-thresholds`.
- **NOT RUNNABLE.** No case is NOT RUNNABLE now. The mechanism stays, probed on a copy of the
  harness with a case of its own (case_verdicts.rs), and every real case file is checked to
  declare no reason. run-chaos.sh's gate line no longer names Owed 82. chaos.json carried no
  Owed 82 tag, so none leaves.
- **The PR #65 confirm pass's n-3.** A shown value cut at 300 characters ends `[cut at 300 of
  <n> characters; verdict.json's measures has it whole]`, never silently. n-1, n-2 and n-4 are the
  Owed 87 agent's (the coordinator).
- **Mutants.** 50 local, all killed, each by its named sim: the verdict 38, kill-renderer and
  cef-cpu 6, the parsers 5, the cut 1. CI, on the throwaway draft PR #72 (`mut/c14-case16`), a–f
  (amended 2026-10-24, the PR #71 review's I1: the six as run; f is mutant 8, and d and e, the
  product-side twins of b and c, were added). All six were KILLED on mutant run 40135507971, each
  on its named check with its controls passing, after an unmutated baseline:
  - (a) `two` on the plain relay image (its `RELAY_IMAGE` not switched): `armed` red, with
    `killed_one_current` passing;
  - (b) `three` holding cef_leaks_to_restart + 1: `held_n` red, with `armed` and
    `killed_one_current` passing;
  - (c) the verdict expecting a restart at 2 in `two`: `restart_as_ruled` red, with `armed`,
    `held_n`, `leaks_counted` and `no_restart` passing;
  - (d) the product restarting one leak later (`>` for `>=` in `cef_health.rs`), in `three`:
    `restart_as_ruled` red, with `armed`, `held_n` and `leaks_counted` passing;
  - (e) the product restarting at 2 (`cef_leaks_to_restart: 2`), in `two`: `restart_as_ruled`
    red, with the same controls;
  - (f), mutant 8: the hold after `set_state(NULL)` on a rebuilt relay-test-hooks image, in `two`.
    `reaper_killed` is red (its reaper lines read `pids [] (of [])`), with `armed` and `held_n`
    passing.
- **The CI witness** (amended 2026-10-24, the PR #71 review). On PR chaos run 40134655269,
  `cef-leaks-three` passed 24 of 24 checks and `cef-leaks-two` passed 16 of 16, so Owed 82 is
  closed. `two`'s `cef_cpu` read each held page at about the live page's renderer CPU (Owed 93).
- **Only case 16 runs on the hooks image** (amended 2026-10-24, the review's m1).
  case_verdicts.rs sweeps every file under harness/. Only case 16's two files may name
  `RELAY_TEST_HOOKS_IMAGE`, the hold key or case 16's overlay, and only cef-leaks.sh may assign
  `RELAY_IMAGE`.
  - Amended 2026-10-25, the #71 confirm pass's n1 and n2. The sweep also reads chaos.yml: the leg
    job's `RELAY_IMAGE` must equal the images job's and name no `test-hooks`. Its positive pair
    asks the image's name only of cef-leaks.sh, and pins only the overlay's `environment` line for
    the key, never a comment.
- **Rebased onto f43c1de, 2026-10-25** (#73 merged, Owed 87 "A"). thresholds.json was regenerated
  by dump-thresholds on the merged tree, never merged by hand. It adds `reaper_kill_after_ms` and
  `teardown_leak_after_ms` to main's, and `output_stall_after_ms` and `output_progress_poll_ms`
  stay retired. thresholds.rs now counts `pairs + 4 + retired`. Both C14 amendments and both
  _INDEX rows are kept. **Expected reds of the next run:** cpu-starved (Owed 86) only. Both
  cef-leaks legs must pass, and no Owed 87 red is expected. Anything else is NOT OWED.

## Amendment 2026-10-25, Owed 94's backstop (branch `feat/owed94-egress-backstop`)

Owner, 2026-10-24, in chat, relayed by the coordinator: "B": a sink whose egress stays flat while
both encoders deliver is swapped, and the beat says so (`docs/_INDEX.md` Owed 94, decision log
"Owed 94 built"; C04's and C09's amendments of this date). The threshold, `output_egress_flat_ms`:
owner 2026-10-25: 9 s, keeps worst outage under 15 s (owner, 2026-10-25, in chat, relayed by the
coordinator: "Apply rec", the coordinator's recommendation of the derived value); it stays the single
derived constant.
- **Case 18, `output-hung`** (n 18; legs `output-hung-hold` and `output-hung-hang`, so 37 legs): on
  the `relay-test-hooks` image (`RELAY_TEST_HOOKS_IMAGE`, `harness/compose.output-hung.yml`), Owed
  94's hook stalls sink 1 `OUTPUT_HUNG_AFTER_MS` (25 s) after its build: `hold` holds its writes,
  `hang` hangs its streaming thread (its teardown hangs too). 70 s live, no drive. 18 checks
  (16 as first built; fix round 1 added `destination_outwaits` and `beat_playing_again`):
  - `destination_outwaits` (the PR #76 review's I-1): MediaMTX closes a publisher idle for its
    `readTimeout`, 10 s by default in 1.21.2, which CI measured 9 978-9 983 ms after the plant,
    inside the backstop's window. Case 18 alone (`compose.output-hung.yml`) sets `MTX_READTIMEOUT` to
    four of output_gap_budget_ms (60 s), and this guard pins `hung_swap`'s ceiling under it on
    every run;
  - `armed` and `planted` (preconditions: the arming line once, the variant's stall and ms; one
    plant, on sink 1), and `planted_in_drive` (after the drive's start, with room before the end:
    the window's ceiling, `beat_playing_again`'s wait and a heartbeat, longer than
    output_gap_budget_ms);
  - `backstop_swap` (`hung_swap_judged`, lib.jq): the first swap after the plant reads
    `egress_stalled`, the reason lines pair the swap lines, sink 1 logged no error before it, and the
    plant to the swap on the lines' own `ts=` is from output_egress_flat_ms + output_backoff_base_ms −
    output_progress_poll_ms − tick_widest_ms to output_egress_flat_ms + output_backoff_base_ms + 2 ×
    output_progress_poll_ms + 2 × tick_widest_ms;
  - `beat_stalled`: at least one beat from the firing (a back-off before the swap) until sink 2
    could earn Connected (output_connected_after_ms after the swap), a tick in from each end, and
    every one reads `stalled`;
  - `gap_in_budget` (one outage between the plant and the swap, at most output_gap_budget_ms, the
    owner's 15 s target), `no_outage_outside_hang`, `one_swap`;
  - `beat_playing_again` (the review's m-3, the plan's "and `playing` again"): from the swap plus
    output_connect_timeout_ms, output_connected_after_ms and two ticks until the end request, at least
    one beat, all `playing`;
  - the false-positive sweep, `no_restarts` and `ended_beat`; measures `gap_ms` (the key) and
    `lost_ms`.
- **b-1 (the PR #73 confirm pass):** `stall_swap`'s `error_to_swap_ms` is read from the two lines'
  own `ts=` (logfacts `GenAt.ts_ms`), with the floor exactly output_backoff_base_ms: an interval
  inside one process belongs on its clock. The cut-anchored bounds stay on docker's stamps (two
  clocks, a frame of slack). Sims: `stall-err-early` (base − 1) reds, `stall-err-base` passes,
  `stall-ci-499` (docker 499, ts 500) passes, `stall-ts-499` (docker 500, ts 499) reds alone.
- **c-2:** `rule-ceiling` passes at exactly output_sink_timeout_ms + budget_output_lost_max_ms.
- **r-1 (the #71 confirm pass):** the RELAY_IMAGE sweep's `soak/` exemption excused no file and is
  dropped; the sweep now allows the image's variable, each hook's keys and overlay, and a
  `RELAY_IMAGE` assignment only in the two hook cases' files.
- **Cases 5, 12 and 17 keep `reason == "error"`** (the PR #73 review's P-3 (b), the implementer's
  choice): the backstop beats the sink's error only in the multi-wake trickle, which CI's 720p never
  reached (false premises, 2026-10-25); a backstop swap there is a red to read.
- thresholds.json gains `output_egress_flat_ms`, `output_progress_poll_ms` (back, for the window)
  and `tick_widest_ms`.
- **Host mutants**, 29, all KILLED, each with a named FAIL in a `case_verdicts.rs` sim: lib.jq's
  `hung_swap` (the reason, the pairing, the error-first, the floor, the ceiling, `ts=` against
  docker, the floor's tick term), case 18's checks (the beat's state and its count, the gap's budget
  and its one outage, the drive's two bounds, one swap, the arming's stall and ms, the plant's count,
  flag and stall, the outage before and after), and b-1's floor, `ts=` and c-2's `<=` (H1-H24); fix
  round 1's H25-H29: `beat_playing_again`'s state, its count and its connect term, the destination's
  deadline (MTX_READ_TIMEOUT_MS at 0 reds `destination_outwaits` in the good worlds), and the room's
  heartbeat term.
- **CI mutants** (a throwaway `mut/owed94` PR, the `mut/c14-case16` workflow's shape: the target check
  PASSes unmutated, then FAILs mutated with its controls still PASS): (a) the case on the plain relay
  image reds `armed`; (b) the backstop's Connected arm removed reds `backstop_swap` on `hold`; (c)
  progress readings without the encoded counts red `backstop_swap` on `hold`; (d) the threshold
  typed in as 15 000 reds `backstop_swap` on `hang`; (e) the hung sink torn down on the swapping
  thread, not a reaper, reds `gap_in_budget` on `hang`. All five KILLED (run 40149017532), but b, c
  and d by MediaMTX's idle close at about 9.98 s (reason `error`, errors_before non-empty), not by the
  backstop's absence (the PR #76 review's I-1). Fix round 1 re-runs b, c and d under the raised
  deadline, each leg also asserting the mechanism on its verdict: b and c no swap after the plant and
  no sink 1 error; d the swap `egress_stalled`, no error first, past the window's ceiling.

## Amendment 2026-10-25, case 6's honest beat (branch `feat/c14-case6-honest-beat`)

Owed 86 under the owner's ruling of 2026-10-24 (decision log "Owed 86: the honest beat", relayed by
the coordinator): the beat's `fps` is the encoder's real output rate, with a starvation alarm. There
is no ladder change, and performance-4x is kept. Case 6 required `ladder_moved`, which that ruling
rules out, so it could never pass. The PR #69 review's m1 is answered.

- **The honest beat, judged.** logfacts reads the driver's three lines with their rates:
  `output fps beat=<fps> floor=<floor>`, `output starvation alarm raised fps=<fps> floor=<floor>`
  and `output starvation alarm cleared fps=<fps> recovered=<level>`. A rate is digits, a point and
  digits, so `NaN`, `inf` and `2.5e1` are not read. The test pins each line against its producer's
  format string in `app/src/driver.rs`. The case judges five checks:
  - `fps_beats_read`, the anti-vacuity: at least one beat is judged. A judged beat is one whose
    window lies inside the quota's, `[stress + heartbeat_interval_ms, stress_done]`. When none is,
    the shown measure carries the reason: the report has no `output_fps`, or the log has no beat
    line, or no beat falls inside the window. It also carries `parsed` and `checked`.
  - `alarm_raised` (a): the run's first raise comes inside the quota's window, `0 ≤
    raise_from_stress_ms ≤ window_ms`. A raise on the healthy clock before the quota is a red.
  - `beats_below_floor` (b): every judged beat reads under `output_fps_floor`, and at least one is
    judged.
  - `alarm_cleared` (c): the first clear after that raise comes once the quota is lifted, within
    `budget_ladder_realtime_ms` + `heartbeat_interval_ms` (realtime_after's window). A beat in that
    window reads `output_fps_recovered`.
  - `ladder_unmoved` (d): every beat that carries a rung reads `full`. A step is now a red. This
    replaces `ladder_moved`.
- **Levels from the product.** thresholds.json gains `output_fps_floor` (28.5,
  `OutputProfile::P720.fps_floor()`) and `output_fps_recovered` (29.5, `fps_recovered()`). No
  harness file types either number. `starvation_applied` now reads `output_fps_floor` where it read
  `budget_fps_mean_min`. thresholds.rs counts `pairs + 4 + retired + levels`.
  - **Deviation:** dump-thresholds needs the `media` feature (GStreamer), which does not link on the
    implementer's Mac (no `gstreamer-1.0.pc`). thresholds.json was regenerated through
    `harness::thresholds::render`, the same function the bin calls, by a scratch crate. It was never
    edited by hand. `the_committed_thresholds_are_what_the_domain_and_the_budgets_say` pins the
    result.
- **The clock, still judged; false premise.** The dispatch said `ladder_moved` was the case's only
  blocker. CI's run 40143263604 failed four checks: `ladder_moved`, and the three that check the
  clock (`window_fps` 17.8 against 29.5, 5 PTS gaps, 6 of 25 score changes never shown). That run
  was re-judged locally by this verdict, over its own compositor.log, under jq 1.7.1 and 1.8.2. The
  five honest-beat checks pass:
  - 20 beats were parsed and 7 judged, at 17.6 to 20.5 fps.
  - The alarm was raised 4.6 s into the quota at 20.46 fps.
  - It cleared 4.7 s after the quota ended, at 83.67 fps.
  - The ladder never moved.

  The case stays red on the clock alone. Those three checks are spec §9.4's "clock holds", and they
  are kept: owed to 86, with a new signature. That signature is the honest beat's evidence: the
  quota felt, `stepped` 0, the alarm raised inside the window, and at least one beat judged with
  none at or over the floor. A clock lost behind a beat that lied, or behind a ladder that stepped,
  reads NOT OWED. Whether the clock is still owed at this share now that the ladder is ruled out is
  **owed to the owner, via the coordinator**. If it is not, the three become measures and case 6
  goes green.
- **Tests.** case_verdicts.rs folds the domain's own `StarvationAlarm` through the simulation's
  readings. It cross-checks the simulated alarm lines against `domain::starvation` and the levels
  against thresholds.json, so the expected values come from the product's declarations. The three
  case 6 tests cover:
  - an honest pass, and the alarm never raised, never cleared, raised falsely before the quota,
    raised late, cleared at the floor and cleared late;
  - a beat that lies (fps never dropped), no beat line, no beat in the window, a report from before
    the honest beat (each with `checked` 0 and its reason), a ladder step, an uncounted step and no
    rung at all;
  - the drive's own reds, a window just above the floor (29.25 fps: not felt; m1's differential),
    and CI's lost clock read as Owed 86.

  chaos_table.rs pins the new signature on CI's evidence, and on each of its clauses absent.
- **Mutants, each predicate once (cp backup, restored byte-identical):** 18 of 18 KILLED (jq 1.8.2; the scoped case 6 tests). `fps_beats_read` `>= 1` to `>= 0`:
  cpu-no-beats, cpu-nostart. `alarm_raised` without its lower bound: cpu-lying, cpu-never-raised;
  without its upper: cpu-raised-late. `beats_below_floor` without its count: cpu-no-beats; without
  `all(< floor)`: cpu-lying. `alarm_cleared` without its lower bound: cpu-never-cleared; without its
  upper: cpu-clears-late; without the recovered beat: cpu-clears-at-floor. `ladder_unmoved` without
  `all(== "full")`: cpu-step, cpu-climbs; without its count: cpu-no-rung. `starvation_applied` back
  on `budget_fps_mean_min`: cpu-fps-near. The judged window from `stress` (the straddling beat
  judged): cpu-good. Each of the signature's five clauses deleted:
  `each_owed_signature_holds_on_its_causes_evidence_and_not_without_it`. The dump's floor key fed
  `fps_recovered()`: `each_threshold_is_dumped_under_its_own_name` and the committed-thresholds
  pin. logfacts' parse, 4 more, each killed by `the_honest_beat_lines_are_read_with_their_rates`:
  the integer digits or the fraction digits not required, `f64` parsing in place of `decimal` (a
  `floor=NaN` near miss), and the raised line's separator loosened.
- **Expected reds of the next run:** cpu-starved, on `window_fps`, `pts_gaps` and
  `scheduled_changes_shown` only, each read "Owed 86" by its signature. The five honest-beat checks
  must pass. Anything else is NOT OWED. This supersedes the case 16 amendment's line.

## Files

- Create: `harness/cases/{camera-stop,camera-flap,reader-kicked,dest-disconnect,dest-stall,cpu-starved,score-down,cef-killed,process-killed,input-impaired}.{sh,jq}` (amended 2026-10-24: all 16, cases 11–16 too, as `.sh` files with `require` lines over `harness/cases/lib.jq`; and `harness/chaos.json`, the legs)
- Create: `harness/run-chaos.sh` (~~runs all 10 in sequence~~ amended 2026-10-24: tables all 16 cases' 31 legs from chaos.yml's parallel runs, prints a table, exits non-zero on any red)
- Create: `crates/harness/src/bin/dump-thresholds.rs`
- Modify: `crates/domain/src/config.rs` (the budget consts and their test only)
- Create: `.github/workflows/chaos.yml`: `workflow_dispatch` plus nightly `schedule`, never on PRs (it
  takes about 25 min). The matrix is one job per case, with `fail-fast: false`. Each job uploads its
  report.

## Steps

- [ ] **Step 1: Add the budget consts and the consistency test.** Run `cargo test -p domain`, and get
  a pass.
- [ ] **Step 2: Write `dump-thresholds`, and commit `harness/thresholds.json`.** Add a check to
  `check.yml`: regenerate it and `git diff --exit-code harness/thresholds.json`. That's the drift
  gate.
- [ ] **Step 3: Write each case's `.sh` and `.jq`.** Every `.jq` ends with an anti-vacuity clause:
  - the window it judges contains ≥ 1 strip frame
  - the event it waits for was observed (for example, case 3 must see 6 kicks in fake-control's
    reader-connection count)

  Without that, a case that never fired passes.
- [ ] **Step 4: Run each case once locally.** Make it green. Where a case is red, the fix goes in the
  compositor, through the owning task's code. Record each fix as a commit referencing the case.
  **Never loosen a `.jq` to go green.** If a budget is physically unreachable, take the measurement
  to the owner as a finding, and do not edit the budget.
- [ ] **Step 5: Mutate once each.**
  1. Set `resume_after_ms` to 0 in a scratch build. Expected red: case 2 (a phase change faster than
     hysteresis). The verdict reads the **committed** `harness/thresholds.json`, never one regenerated
     from the scratch build, or the mutant moves its own oracle.
  2. Disable the held overlay frame (C11 mutant). Expected red: case 8.
  3. Comment out the fault-injection line of case 1 (the `/pause`), so the run happens and the
     report exists with zero slates. The `.jq` must fail **on its anti-vacuity clause**, and the
     clause's own message is echoed in the table. A failure anywhere else, or a missing report,
     does not count.
- [ ] **Step 6: Run `run-chaos.sh` three times in a row**, all green (spec §9.7 item 1). Paste the
  three tables.
- [ ] **Step 7: Commit and open the PR.** `test(chaos): ~~twelve~~ sixteen automated failure cases (C14)` (amended 2026-10-18; was "ten"; amended 2026-10-24: sixteen, and the dispatch's PR title `feat(harness): the chaos suite (C14)`). Then
  trigger `chaos.yml` once by dispatch, and paste the run URL.

## Acceptance

- ~~10~~ ~~12~~ 16 cases, each with an anti-vacuity clause (amended 2026-10-18: case 11 `cef-hung`, Owed 18; case 12 `dest-trickle`, Owed 16; amended 2026-10-24: cases 13–16 of the 2026-10-20 amendment).
- Every number comes from `thresholds.json`.
- 3 consecutive local green runs, plus 1 green CI dispatch.
- ~~3~~ 5 mutants went red (amended 2026-10-18: mutants 4 and 5).
- (Amended 2026-10-18, Owed 19) Case 8 asserts restart evidence (generations in the compositor
  log) and accepts `ready`/`restarting`/`starting` before the new ready. Case 9 reads
  `RestartCount` + 1 and a non-zero `die` exit code.
- ~~(Amended 2026-10-18, Owed 13/15) Case 5's bound is `output_stall_after_ms + poll + beat`, read
  from `thresholds.json`.~~ (Amended 2026-10-24, Owed 87) In cases 5, 12 and 17, `stall_swap` passes
  only on the sink's own error: `reason=error`, the error line before the swap, the swap
  output_backoff_base_ms − frame_period_ms to budget_output_reconnect_ms after it and within
  output_sink_timeout_ms + output_backoff_base_ms + frame_period_ms of the last growth, and the error
  no sooner than output_sink_timeout_ms − frame_period_ms after the cut and no later than
  output_sink_timeout_ms + budget_output_lost_max_ms, each read from `thresholds.json`. m9
  (`OUTPUT_SINK_TIMEOUT_MS` 20 s) is KILLED on case 17. (Amended 2026-10-25, b-1: the error to the
  swap is read from the lines' own `ts=`, and its floor is output_backoff_base_ms itself.)
- (Amended 2026-10-25, Owed 94) Case 18 (`output-hung`, `hold` and `hang`) runs on the
  `relay-test-hooks` image; its arming line and plant are preconditions; the first swap after the
  plant reads `egress_stalled` inside the domain's window on the process's own clock, the beats
  read `stalled` until sink 2 could earn Connected, and the viewer's gap is one outage inside
  output_gap_budget_ms. Product mutants without the backstop, without the encoded counts, or with
  the threshold typed in red `backstop_swap`; one tearing the hung sink down on the swapping thread reds
  `gap_in_budget`. (Fix round 1, the PR #76 review) The destination outwaits the window
  (`destination_outwaits`: MediaMTX's readTimeout raised for case 18 alone), and the beats read
  `playing` again once sink 2 could have earned Connected (`beat_playing_again`).
- (Amended 2026-10-18, Owed 20) Case 6 asserts the score stays on air through the stress and
  after it, and every beat's `rung` is `full`/`overlay_half_rate`, never back to `full` after a step.
- (Amended 2026-10-25, Owed 86, the honest beat) Case 6 judges the beat and its alarm against
  `output_fps_floor` and `output_fps_recovered` from `thresholds.json`, never typed. (a) The alarm
  is raised inside the quota's window. (b) Every judged beat reads under the floor, and at least one
  is judged; zero judged is a red with its reason. (c) The alarm clears after the quota, within
  realtime_after's window, and a beat there reads the recovered level. (d) No beat steps off
  `full`. `ladder_moved` is gone. The clock's three checks stay judged, owed to 86 (its amendment).
- (Amended 2026-10-20, C11b; Owed 62, 68, 69) Case 8 takes case 9's shape, with a non-zero, non-75 exit and no core
  file. Cases 13–16 prove the in-place restart (exit 75, the final beat's cause, no abort, no core), each with its
  negative run where the brief has one, and case 15's fast loop; cases 1–7 and 9–12 carry the false-positive sweep
  (no verdict, no GPU death, no exit 75); the outage of every restart is measured and judged against
  `BUDGET_PROCESS_RESTART_OUTPUT_MS` in 3 runs; mutants 6 and 7 red. A case that cannot run (16 without the hook) is
  reported, never faked (amended 2026-10-24, case 16 wired: no case is NOT RUNNABLE now, and the
  mechanism is probed on a case of its own).
- (Amended 2026-10-24, Owed 82, PR #62) Case 16 runs on the `relay-test-hooks` image (the amendment
  "Owed 82's handoff"). The arming line is a precondition, and a missing one is a red. The driver is
  `kill-renderer`, with exactly 1 killed per drive, each after the current gen's `cef ready`. The
  held lines number N, counted by retirement. Each held teardown's reaper line lists at least 1
  killed pid. `three` exits 75 with its detail, and `two` stays on air at `leaked_teardowns=2` with
  its CEF CPU printed. Mutant 8 is red. (Amended 2026-10-24, case 16 wired: as built, its own
  amendment; mutants a, b and c red too, each on its named check. Amended again 2026-10-24, the
  PR #71 review's I1: mutants a–f, f being mutant 8, all KILLED on mutant run 40135507971, and
  both legs green on chaos run 40134655269.)

## Do not touch

The budgets, except by owner ruling. And `.jq` files are never loosened to get green.

## Output cap

Under 15 lines: the three run tables (case → pass/fail, key measured number), fixes made, findings
for the owner.
