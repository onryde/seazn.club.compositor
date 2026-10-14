# C15: 3-hour soak on Fly Machines (owner ruling 2026-10-14: 3 h)

**Repo:** `seazn.club.compositor`. **Depends on:** C14. Read spec §9.5.

## Context

The owner set the soak at **3 h** (2026-10-14). It fits inside GitHub's 6 h job cap, so **one**
workflow job drives it end to end. The media still runs on Fly, because private-repo runners have 2
vCPU and the numbers must come from the production guest. It runs as **two Machines** in a dedicated app
`seazn-relay-soak` (region `lhr`, as R1's `RUNNER_DEFAULT_REGION`):

1. **compositor Machine:** the **real** `seazn-relay:<sha>` image, on the **same guest as production**
   (perf, 4 CPU, 8192 MB: R1's `RUNNER_DEFAULT_GUEST`, re-pinned from `seazn.club`
   `apps/web/src/server/relay/config.ts:39`), with `COMPOSITOR_DRIVERS=live`. It talks to the harness
   Machine over Fly's private network (`<id>.vm.seazn-relay-soak.internal`).
2. **harness Machine:** a `seazn-relay-soak:<sha>` image built from `harness/soak/Dockerfile`. It
   holds mediamtx, fake-camera, fake-control, the checker and the soak driver, as plain processes
   under tini. It runs as **root** only because the dest-disconnect step uses `iptables`. It holds no
   secrets beyond the per-run random job token.

Splitting them keeps the fakes from stealing CPU from the compositor, so the resource numbers mean
what they would in production.

**Schedule** (spec §9.5):

| What | When |
|---|---|
| Duration | 3 h at 720p30 |
| Scripted match | every 20 min: fake-control replays `score.json` from the start, so the overlay index restarts at 0 |
| Camera dropout | every 5 min: `POST camera /pause`, then `/resume` 10 s later |
| Dest disconnect | once per hour, at minute 30: `iptables -I INPUT -p tcp --dport 1935 -j DROP` for 30 s, then `-D` |

The driver writes every scheduled action to `actions.jsonl`, with its wall time.

**Pass**, computed by `harness/soak/verdict.jq` from the checker's `report.json` and `actions.jsonl`:

| # | Criterion | How it's measured |
|---|---|---|
| 1 | Memory | median `memMb` of hour 3 within ±10 % of the hour-1 median (from heartbeats) |
| 2 | Zero unexplained output gaps | every `output_outages` entry overlaps a dest-disconnect action, and ends ≤ `BUDGET_OUTPUT_RECONNECT_MS` after that action's end; `pts_gaps == []` |
| 3 | No programme rebuild | fake-control saw **exactly one** session GET, so the process never restarted, and C09 guarantees the programme is built once per process (`start()` refuses a second build, and C09's mutant pins it) |
| 4 | Dropped frames | final `droppedFrames` < 0.1 % of `3 h × fps` frames (from thresholds) |
| 5 | Anti-vacuity | `heartbeats.count ≥ 3 h / heartbeat_interval_ms × 0.95`; 36 dropout actions and 3 disconnect actions present; 9 match restarts seen as an overlay index drop to 0 |

## Workflow

**`soak.yml`** (`workflow_dispatch`; inputs `sha`, and `minutes` defaulting to 185, i.e. 3 h + 5 min):

1. If `secrets.FLY_API_TOKEN_SOAK` is empty: `echo "::warning::FLY_API_TOKEN_SOAK unset — soak skipped"`,
   then exit 0.
2. Build and push both images to `registry.fly.io/seazn-relay-soak`.
3. `flyctl machine run` the harness Machine, then the compositor Machine, with:
   - `--restart no` (a restart would invalidate criterion 3; the soak reports it rather than hides it)
   - `SESSION_ID`, plus `JOB_TOKEN`: a random 32-byte hex generated in the job, passed as a Machine env
     and masked with `::add-mask::`, never echoed
   - `APP_URL=http://<harness>.vm.seazn-relay-soak.internal:7000`
   - `RELAY_DEADLINE_AT` = now + `minutes`
4. Poll the harness Machine every 5 min until its driver writes `/out/done`, or until `minutes + 10`
   elapse.
5. `flyctl ssh sftp get` `/out/report.json`, `/out/actions.jsonl` and `/out/heartbeats.jsonl`. Run
   `verdict.jq`, and upload the artifacts.
6. **In an `if: always()` step:** destroy both Machines, and list the app to prove it's empty. A
   cancelled or failed run must never leave Machines billing.
7. Fail on a red verdict.

The job's `timeout-minutes` is `minutes + 40`, well under GitHub's 360.

## Files

- Create: `harness/soak/{Dockerfile,driver.sh,verdict.jq,README.md}`
- Create: `.github/workflows/soak.yml`
- Create: `harness/soak/fixtures/` (a 30-min miniature run's `report.json`/`actions.jsonl`, used to
  test `verdict.jq`)
- Test: `harness/soak/test-verdict.sh` runs `verdict.jq` against the fixtures with the duration env
  set to 30 min. One fixture passes. Four fail, each for one criterion: memory drift, an unexplained
  gap, two session GETs, and a missing dropout action. `check.yml` runs it.

## Steps

- [ ] **Step 1: Write the `verdict.jq` fixtures and `test-verdict.sh`,** and get a FAIL (no
  `verdict.jq` yet).
- [ ] **Step 2: Implement `verdict.jq`.** Every number comes from `harness/thresholds.json` or the
  run's duration env, never a literal. Get a PASS: 1 green, 4 red, each for the expected criterion.
  Assert the failure **reason**, not just the exit code.
- [ ] **Step 3: Implement the soak Dockerfile, `driver.sh` and both workflows.**
- [ ] **Step 4: Do a 30-minute rehearsal.** Run `soak.yml` with `minutes: 30`. The verdict must be green with the scaled anti-vacuity
  counts. Paste the run URLs.
- [ ] **Step 5: Do the full 3-hour run.** Paste the verdict table, plus memory hour-1 vs hour-3 and
  the dropped-frames %.
- [ ] **Step 6: Mutate once.** Run the verdict against the fixture with two session GETs. It must
  fail on criterion 3. This is already covered by step 2, so confirm it's in the fixture set.
- [ ] **Step 7: Commit and open the PR.** `test(soak): three-hour Fly soak with a split harness (C15)`

## Acceptance

- `test-verdict.sh` is 1 green / 4 red, with reasons.
- The 30-min rehearsal is green.
- The 3-hour run is green, with numbers pasted.
- Both Machines are destroyed afterwards. Run `flyctl machine list -a seazn-relay-soak` and show it's
  empty.

## Do not touch

The compositor crates. A soak red is fixed in the owning task's code, with the fix referencing this
run.

## Output cap

Under 15 lines: the verdict table, memory numbers, run URLs, cost estimate (Machine-hours × rate).
