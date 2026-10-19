# C14: Chaos suite (~~10~~ 12 cases, automated pass/fail; amended 2026-10-18)

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

### The 10 cases (12 with the 2026-10-18 amendment's cases 11 and 12)

The PDF's kill-switch case is replaced by "CEF killed".

| # | File | Script | Verdict (`.jq`) |
|---|---|---|---|
| 1 | `camera-stop` | at 20 s `POST camera /pause`; at 30 s `/resume`; run 50 s | a slate span begins ≤ `BUDGET_SLATE_AFTER_LOSS_MS` after the last pre-pause frame; first strip frame after resume ≤ `BUDGET_RETURN_TO_LIVE_MS` after the camera's first post-resume `emit` wall time; `output_outages == []`; `counter_regressions == 0` |
| 2 | `camera-flap` | `/pause` then `/resume` every 5 s for 120 s | `output_outages == []`; `counter_regressions == 0`; **hysteresis from the strip, not the beats** (5 s heartbeats can't see a 1 s rule): every `camera.resumes[].suppressed_ms ≥ resume_after_ms − one frame period` and `≤ BUDGET_RETURN_TO_LIVE_MS`; anti-vacuity: `resumes` has ≥ 10 entries (120 s of 5 s flaps) |
| 3 | `reader-kicked` | kick the SRT reader every 20 s, 6 times | after each kick, strip frames return ≤ `BUDGET_RETURN_TO_LIVE_MS + input_backoff_cap_ms`; heartbeats show `inputKickAlert: true` once `kick_alert_count` kicks fall in `kick_window_ms`, and **not before** (assert the first `true` beat comes after kick #`kick_alert_count`); `output_outages == []` |
| 4 | `dest-disconnect` | at 20 s `netem out loss 100%`; at 50 s `clear`; run 80 s | output resumes ≤ `BUDGET_OUTPUT_RECONNECT_MS` after clear; the programme lost per outage, measured as the counter gap across the outage minus the outage wall span, is ≤ `BUDGET_OUTPUT_LOST_MAX_MS` worth of frames; heartbeats show `input: connected` throughout (input untouched); `pipelineRebuilds` unchanged |
| 5 | `dest-stall` | at 20 s `netem out rate 1kbit` (accepts, but no acks); at 50 s `clear` | a `sinkSwaps` increase appears in a beat ≤ `output_ack_stall_ms + heartbeat_interval_ms` after 20 s; output recovers after clear as in case 4 |
| 6 | `cpu-starved` | start a `stress-ng --cpu 4` sidecar in the compositor netns/cgroup at 20 s, for 40 s | `fps_mean` over the window stays ≥ 29.5 (the clock holds); `pts_gaps == []`; heartbeats show `droppedFrames` increase **or** `rung != "full"`, and at least one of them changes; after stress ends, `encoderSpeed ≥ $t.encoder_speed_realtime_min` within `BUDGET_LADDER_REALTIME_MS` |
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

## Files

- Create: `harness/cases/{camera-stop,camera-flap,reader-kicked,dest-disconnect,dest-stall,cpu-starved,score-down,cef-killed,process-killed,input-impaired}.{sh,jq}`
- Create: `harness/run-chaos.sh` (runs all 10 in sequence, prints a table, exits non-zero on any red)
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
- [ ] **Step 7: Commit and open the PR.** `test(chaos): twelve automated failure cases (C14)` (amended 2026-10-18; was "ten"). Then
  trigger `chaos.yml` once by dispatch, and paste the run URL.

## Acceptance

- ~~10~~ 12 cases, each with an anti-vacuity clause (amended 2026-10-18: case 11 `cef-hung`, Owed 18; case 12 `dest-trickle`, Owed 16).
- Every number comes from `thresholds.json`.
- 3 consecutive local green runs, plus 1 green CI dispatch.
- ~~3~~ 5 mutants went red (amended 2026-10-18: mutants 4 and 5).
- (Amended 2026-10-18, Owed 19) Case 8 asserts restart evidence (generations in the compositor
  log) and accepts `ready`/`restarting`/`starting` before the new ready. Case 9 reads
  `RestartCount` + 1 and a non-zero `die` exit code.
- (Amended 2026-10-18, Owed 13/15) Case 5's bound is `output_stall_after_ms + poll + beat`, read
  from `thresholds.json`.
- (Amended 2026-10-18, Owed 20) Case 6 asserts the score stays on air through the stress and
  after it, and every beat's `rung` is `full`/`overlay_half_rate`, never back to `full` after a step.

## Do not touch

The budgets, except by owner ruling. And `.jq` files are never loosened to get green.

## Output cap

Under 15 lines: the three run tables (case → pass/fail, key measured number), fixes made, findings
for the owner.
