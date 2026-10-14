# C14: Chaos suite (11 cases, automated pass/fail)

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
with doc lines. **No number is typed into a `.jq` file.** If a `.jq` needs a number, it must come from
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

### The 11 cases

The PDF's kill-switch case is replaced by "CEF killed".

| # | File | Script | Verdict (`.jq`) |
|---|---|---|---|
| 1 | `camera-stop` | at 20 s `POST camera /pause`; at 30 s `/resume`; run 50 s | a slate span begins ≤ `BUDGET_SLATE_AFTER_LOSS_MS` after the last pre-pause frame; first strip frame after resume ≤ `BUDGET_RETURN_TO_LIVE_MS` after the camera's first post-resume `emit` wall time; `output_outages == []`; `counter_regressions == 0` |
| 2 | `camera-flap` | `/pause` then `/resume` every 5 s for 120 s | `output_outages == []`; across heartbeats, no `programmePhase` change < `resume_after_ms` after the previous one; `counter_regressions == 0` |
| 3 | `reader-kicked` | kick the SRT reader every 20 s, 6 times | after each kick, strip frames return ≤ `BUDGET_RETURN_TO_LIVE_MS + input_backoff_cap_ms`; heartbeats show `inputKickAlert: true` once `kick_alert_count` kicks fall in `kick_window_ms`, and **not before** (assert the first `true` beat comes after kick #`kick_alert_count`); `output_outages == []` |
| 4 | `dest-disconnect` | at 20 s `netem out loss 100%`; at 50 s `clear`; run 80 s | output resumes ≤ `BUDGET_OUTPUT_RECONNECT_MS` after clear; the programme lost per outage, measured as the counter gap across the outage minus the outage wall span, is ≤ `BUDGET_OUTPUT_LOST_MAX_MS` worth of frames; heartbeats show `input: connected` throughout (input untouched); `pipelineRebuilds` unchanged |
| 5 | `dest-stall` | at 20 s `netem out rate 1kbit` (accepts, but no acks); at 50 s `clear` | a `sinkSwaps` increase appears in a beat ≤ `output_ack_stall_ms + heartbeat_interval_ms` after 20 s; output recovers after clear as in case 4 |
| 6 | `cpu-starved` | start a `stress-ng --cpu 4` sidecar in the compositor netns/cgroup at 20 s, for 40 s | `fps_mean` over the window stays ≥ 29.5 (the clock holds); `pts_gaps == []`; heartbeats show `droppedFrames` increase **or** `rung != "full"`, and at least one of them changes; after stress ends, `encoderSpeed ≥ 1.0` within `BUDGET_LADDER_REALTIME_MS` |
| 7 | `score-down` | `POST fake-control /control/score-down?ms=60000` at 20 s | heartbeats show `scoreStale: true` within 10 s (spec §6, page side) + one beat interval; the overlay index never changes during the outage and never regresses; `scoreStale: false` after the first poll following recovery |
| 8 | `score-void` | the timeline's event 12 is a void that retracts event 11 | the overlay index drops from 11 to 10 at event 12's `due_wall_ms`, within `late_frames_max ≤ 1`; `future_dated == 0` |
| 9 | `cef-killed` | at 20 s `docker exec compositor pkill -KILL -f cef` | the overlay strip shows the **same** index (held frame) with no transparent gap; camera strip frames continue (no freeze > 2 frames); heartbeats go `graphics: restarting` → `ready`; `output_outages == []` |
| 10 | `process-killed` | at 20 s `docker exec compositor pkill -KILL -x compositor` (tini exits non-zero, compose's `on-failure:2` restarts it) | output resumes ≤ `BUDGET_PROCESS_RESTART_OUTPUT_MS` after the kill; heartbeats resume; fake-control sees the **same** `SESSION_ID`; `counter_regressions == 0` |
| 11 | `clock-skew` | `POST fake-control /control/skew?ms=5000`: fake-control serves every timeline event stamped 5 s ahead (the scorer's clock runs fast) | the checker is given the **unskewed** timeline, so `due_wall_ms` is the true due time. Each index change appears no earlier than its true due time, and no later than true due + 2 000 ms (the page's clamp, spec §6) + 1 000 ms (one poll) + one frame. Without the clamp, an event would show 5 s late, so the upper bound is the one that fails |

**Cases 8 and 11: who owns what.** They exercise **page** logic: void re-derivation and clock
clamping. Here they run against the harness test page, which implements the same rule the real page
does, so they prove that CEF renders a page change within one frame. The real page's rule is owned
and tested by **W05** (vitest on the derive function, plus an e2e). C17 proves the real page live.
This split is recorded in `docs/_INDEX.md` as a decision.

## Files

- Create: `harness/cases/{camera-stop,camera-flap,reader-kicked,dest-disconnect,dest-stall,cpu-starved,score-down,score-void,cef-killed,process-killed,clock-skew}.{sh,jq}`
- Create: `harness/run-chaos.sh` (runs all 11 in sequence, prints a table, exits non-zero on any red)
- Create: `crates/harness/src/bin/dump-thresholds.rs`
- Modify: `crates/domain/src/config.rs` (the budget consts and their test only)
- Create: `.github/workflows/chaos.yml`: `workflow_dispatch` plus nightly `schedule`, never on PRs (it
  takes about 25 min). The matrix is one job per case, with `fail-fast: false`. Each job uploads its
  report.
- Modify: `harness/pages/overlay-test.html` (void handling and the 2 s clamp, for cases 8 and 11)

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
     hysteresis).
  2. Disable the held overlay frame (C11 mutant). Expected red: case 9.
  3. Remove a case's anti-vacuity clause and make its script a no-op. It must still go red; if it
     stays green, the clause is missing.
- [ ] **Step 6: Run `run-chaos.sh` three times in a row**, all green (spec §9.7 item 1). Paste the
  three tables.
- [ ] **Step 7: Commit and open the PR.** `test(chaos): eleven automated failure cases (C14)`. Then
  trigger `chaos.yml` once by dispatch, and paste the run URL.

## Acceptance

- 11 cases, each with an anti-vacuity clause.
- Every number comes from `thresholds.json`.
- 3 consecutive local green runs, plus 1 green CI dispatch.
- 3 mutants went red.

## Do not touch

The budgets, except by owner ruling. And `.jq` files are never loosened to get green.

## Output cap

Under 15 lines: the three run tables (case → pass/fail, key measured number), fixes made, findings
for the owner.
