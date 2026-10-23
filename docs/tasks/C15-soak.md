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
- (Amended 2026-10-23, C13 CI round 2) The cost legs' camera ran once with `--pattern snow` and
  once with `--pattern noise` on the production guest. Both numbers are pasted, and the heavier
  pattern is the one the legs used.
- The 30-min rehearsal is green.
- The 3-hour run is green, with numbers pasted.
- Both Machines are destroyed afterwards. Run `flyctl machine list -a seazn-relay-soak` and show it's
  empty.

## Amendment 2026-10-18 (owner ruling on the ladder, `docs/_INDEX.md` Owed 20)

Owner, 2026-10-18, in chat, relayed by the coordinator: "no lite mode; if CPU impacts, measure and
raise the CPU size". The owner approved measuring the **real** overlay cost here, with a size rule.
F-005 measured only a test page: 0.83 of 4 cores at 720p, and 1.27 average (p95 1.37) on its 720p
snow diagnostic. This closes that gap. A ladder step-down in production is a sizing alarm (C17
runbook), so the size must leave headroom for the real page.

### Cost legs (run before the 3 h soak, on the same split: compositor Machine on the production guest, harness Machine beside it)

| Leg | Output | Overlay | Camera |
|---|---|---|---|
| A | 720p30 | template `bar` on a **cricket** fixture, moments back to back | snow |
| B | 720p30 | template `bug` (the corner bug) on a **badminton** fixture, rallies back to back (the score driver records a rally every 3 s) | snow |
| C | 1080p30 | as A | snow |
| D | 1080p30 | as B | snow |

**Why these two sports** (amended 2026-10-18, PR #21 review; the house rule "one sample is not a
sweep").
- **Cricket's bar is the expected heaviest page.** It carries the most live elements at once: the
  crease band with two batters, the bowler's analysis and this over's ball glyphs. A delivery
  repaints it and can fire a moment on nearly every ball.
- **Badminton's corner bug is the most common production pairing.** So its cost is the one most
  Machines actually pay.
- "Heaviest" is a hypothesis, not a measurement. If leg B or D measures above A or C, that is a
  finding. Then add a leg for a clocked sport, football, whose running clock repaints every second,
  before choosing a size.

- **The page is the production overlay page.** `…/overlay/fixtures/<id>?template=bar|bug&host=compositor&…`,
  from a `seazn.club` build containing R2007 PR A (`#974`, the registry: `TemplateId = "bar" | "bug"`)
  and R2005. Never `overlay-test.html`. Where it is served from is this task's choice: stg, or a
  standalone build on the harness Machine. Record which, with the web commit.
- **Frequent moments.** A score driver records cricket deliveries on the fixture so that a moment
  fires as soon as the previous moment's slab has cleared. That is the worst case the page allows.
  Record the moment rate achieved, and fail the leg if fewer than one moment a minute was seen.
- **Snow.** The camera is `videotestsrc pattern=snow`, F-005's worst-case all-real-bits encode, at
  the session's bitrate (720p 4 000 kbps, 1080p 6 000 kbps: `OutputProfile::P720`/`P1080` in `crates/domain/src/config.rs`; F-005 ran its snow diagnostic at only 2 500 kbps). The fake-camera gains a
  `--pattern snow` mode for this. It keeps the strip if the checker reads it.
  **Amended 2026-10-23 (C13 CI round 2; see the amendment below):** ~~`videotestsrc pattern=snow`,
  F-005's worst-case all-real-bits encode~~. C13's `--pattern snow` is 16×16 block noise above a
  black guard cell, because per-pixel noise crushes the strip at x264's I-frames. Per-pixel noise
  is `--pattern noise`. Block snow is not F-005's picture, and it is not cheaper: the reviewer's
  host libx264 took longer on it. So F-005's figure is not comparable to either. Run both once on
  the production guest and use the heavier.
- **Each leg runs at least 10 minutes** (brief default; F-005 ran 290 s), sampled every 1 s from
  +15 s, by F-005's method. The sample covers the whole Machine (`/proc/stat`) and the process tree,
  plus the heartbeats' `encoderSpeed`, `droppedFrames` and `rung`.
- **One page reload per leg** (amended 2026-10-18).
  - At the leg's midpoint, kill the CEF renderer once: `pkill -KILL -f cef`, C14 case 8's
    mechanism. The page reloads under load, which is the same work as the 2 h recycle and as a
    hang's restart.
  - A reload under load is the likeliest false sizing alarm: 3 drops in 1 s step the ladder down
    for the rest of the match. So it is measured, not assumed.
- **Size rule (acceptance).** A leg passes only if all three hold over its whole run, the reload
  included:
  1. The p95 of Machine CPU is at most 50 % of the guest's cores, which is 2.0 cores on
     performance-4x.
  2. **No step down:** every beat's `rung` is `full`.
  3. **No pre-encoder drops:** the final `droppedFrames` is 0.

  CPU alone can pass a leg that steps. A single-thread bottleneck, such as the blend or CEF's
  renderer at `nice 10`, can drop frames at 45 % p95 (PR #21 review, I4).
  - If any leg fails any of the three, move to the next performance size (performance-8x after 4x),
    and re-run all four legs there. Repeat until all four pass. The 3 h soak then runs on the
    passing size.
  - The move is a `seazn.club` PR that sets R1's `RUNNER_DEFAULT_GUEST` (Owed 25), because web picks
    the guest. This task records the finding and opens or requests that PR. It never edits web's
    config itself.
- **Record `F-0xx: real overlay cost`** in `_FINDINGS.md`: F-005's table shape per leg (Machine
  cores avg / p95 / max, tree, encoder speed, drops, rung), the URLs, the moment rate and the size
  verdict. Screenshots of each leg's programme are looked at, and never committed.

### The 3 h soak runs the real templates (amended 2026-10-18)

The soak's overlay is the production page, never `overlay-test.html`. Run the soak on the heaviest
pairing: `bar` on the cricket fixture, with the same score driver. The `overlayUrl` that
fake-control hands out points at the real page (served as for the cost legs) for a **public**
fixture, so it needs no `sid`/`st`. That is R2005's public path, with `host=compositor`.

The real page has no machine-readable strip. So criterion 5's "9 match restarts seen as an overlay
index drop to 0" is replaced by four checks:
- (a) The compositor log has `cef alive` lines for at least 95 % of `3 h / cef_alive_every_ms`.
- (b) (Amended again 2026-10-18, C05b PR #22 review I2.)
  - `graphics` never reads `failed`.
  - Every `overlay hidden` line names its reason: `reason=hang`, `reason=crash` or `reason=recycle`
    (`recycle` amended 2026-10-18 in the review's second round: a 3 h soak crosses one 2 h recycle,
    and a reload not ready within `cef_hang_after_ms` of the recycled page's last beat hides as
    `recycle`; it is printed, not failed, like a crash).
    - A `reason=failed` line fails, as the first clause already does.
    - So does a bare `overlay hidden` with no reason. That line comes from a build older than this
      amendment.
  - Print the hides by reason.
  - Nothing is joined across lines. So a lost `cef alive` line (check (a) tolerates 5 %) cannot turn a
    real hang into a false red.
  - The superseded text: ~~no `overlay hidden` appears without a matching hang (amended 2026-10-18,
    C05b; `docs/_INDEX.md` false-premise row "C15's soak check (b) has a hang line to match"; before
    that it read "hang or crash line"). No line names a hang: C05b's hang returns the same `Hide` as
    Failed, so the log has only C12's lines (its amendment 2026-10-18). A matching hang is an
    `overlay hidden` whose current generation `g` (the last `graphics restart gen=g`) logged
    `cef ready gen=g` and no `cef crashed gen=g` before the hide, `cef_hang_after_ms` after its last
    `cef alive gen=g` (or its ready, if it never beat), within one tick. "Or crash" was vacuous: below
    the count a crash never hides, and at the count `graphics` reads `failed`, which the first clause
    already fails. Print the hides matched, and fail on a hide that matches nothing.~~
  - "Below the count a crash never hides" has also stopped being true: a crashed page's held frame
    now ends in `reason=crash`, and a recycled page's in `reason=recycle`, `cef_hang_after_ms` after
    that page's last beat (C05b's one bound).
- (c) The score driver has recorded ≥ 1 delivery a minute.
- (d) Once a minute, a sample of the programme recording's overlay region differs from the
  camera-only reference, so the overlay is on air.

The strip-based overlay checks stay in C13/C14, on the test page.

### Soak verdict, criterion 6 (added)

| # | Criterion | How it's measured |
|---|---|---|
| 6 | No step down | Every heartbeat's `rung` is `full`. A stepped beat fails with the reason "sizing alarm: the guest is short of CPU" (C17 runbook). In production the same beat raises web's sizing alarm. |

`test-verdict.sh` gains a fifth red fixture, a beat with `rung: "overlay_half_rate"`, which fails
on criterion 6. That makes the suite 1 green / 5 red. Step 6 gains a second mutant: drop criterion 6 from `verdict.jq`. Expected red: the new fixture
reads green.

### Acceptance (added)

- The four cost legs, each with its one reload, meet all three parts of the size rule (CPU p95
  ≤ 50 %, 0 step-downs, 0 dropped frames) on the size the soak then uses. The F-entry is pasted,
  with the sport reason, or the football leg if the hypothesis failed.
- The 3 h soak ran on the real `bar`/cricket page, and criterion 5's replacement checks (a)–(d)
  all passed.
- `test-verdict.sh` is 1 green / 5 red, with reasons.

## Amendment 2026-10-23 (C13 CI round 2: the camera's snow is not F-005's)

- **What changed.** C13's per-pixel snow crushed the strip at x264's I-frames at 4 Mbps (mutant
  run 39984287910, frame 1 020, the first snow keyframe). So `--pattern snow` is now one noise value
  per 16×16 block on the macroblock grid, above a one-cell black guard over the strip, and the strip
  reads on it. The per-pixel picture (`videotestsrc pattern=snow`'s, F-005's) is kept as
  `--pattern noise`. The strip does not survive noise's I-frames, so a leg on `noise` cannot gate on
  `camera_strips`.
- **Why the cost legs must measure both.** The two pictures do not cost the same. On the PR #48
  reviewer's host, libx264 was slower on block snow than on per-pixel noise: 6.5 s against 4.0 s at
  720p and 4 000 kbps, and 9.2 s against 5.2 s at 1080p and 6 000 kbps. So F-005's snow figure is
  comparable to neither. Neither is the worst case by assumption either: the host is not the
  production guest.
- **The leg.** Run one cost leg per profile with each pattern on the production guest, and record
  both numbers in the F-entry. The legs and the size rule then use the heavier pattern. On the
  `noise` run, the strip facts (`camera_strips`, latency) are reported, not judged.

## Do not touch

The compositor crates. A soak red is fixed in the owning task's code, with the fix referencing this
run.

## Output cap

Under 15 lines: the verdict table, memory numbers, run URLs, cost estimate (Machine-hours × rate).
