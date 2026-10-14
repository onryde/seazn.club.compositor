# R2003: Runner restarts in place (`on-failure`, ≤2), same-attempt accounting, 15 s stale for composed

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/r2003-runner-restart -b feat/relay-runner-restart origin/main`.
**Depends on:** R2002 merged. **Runs sequentially** with R2004 and R2005, because all three touch
`apps/web/src/server/relay/*`.

## Context

Pinned 2026-10-14 at main `09290de3c`. **Re-pin every line before editing.**

- `runner-fly.ts:237` creates with `restart: { policy: "no" }`. The header comment (`:11-12`) gives
  the reason: *"D4: the lazy expiry path is the only retry authority; any other policy puts two
  encoders on one stream key."*
- `:235`: the guest comes from `spec.guest`, which comes from `config.ts:39`
  `RUNNER_DEFAULT_GUEST = { cpus: 4, memoryMb: 8192, cpuClass: "dedicated" }`. That's already from
  config. **Verify** the create call's caller passes it (`grep -a "RUNNER_DEFAULT_GUEST"`), and only
  fix it if it doesn't. The spec line "guest from config" may already be true; if so, record it as a
  false premise.
- `domain/runner.ts`:
  - `RUNNER_TABLE` (`:185`). `playing.stale_beat` → `toLost` (FORCE_DESTROY). `observedFrom`
    (`:159`) maps an observed `pending`/`running` to "stay, no claim", and `destroyed` →
    `afterLostDestroyed`, which is the retry or the fail.
  - **Invariant 1:** `create_started` (the retry) is legal **only from `destroyed`**, and only when
    `attempt === r.attempt + 1 ≤ RUNNER_MAX_ATTEMPTS` (`:181`, `config.ts:145` = 2).
- `FLY_STATE_MAP` (`runner-fly.ts:61`) maps Fly `restarting` (and `replacing`/`updating`) →
  `pending`.
- `domain/expiry.ts` `evaluate` (`:68`): the stale-beat rule uses
  `limits.staleHeartbeatSeconds = STALE_HEARTBEAT_SECONDS` (`config.ts:138` = 90) for every mode.

**Why `on-failure` is now safe.** D4's hazard was two encoders on one key. Invariant 1 closes it:
- web retries only after the old Machine is **confirmed destroyed**
- a Machine Fly is restarting in place is `pending`, never `destroyed`
- a stale beat during a restart goes to `lost` with FORCE_DESTROY, so the restarting Machine is
  killed before any `r<n+1>`

**This task's tests must prove that, not assume it** (AGENTS.md class 3).

## Changes

1. **`runner-fly.ts`:**
   - `restart: { policy: "on-failure", max_retries: RUNNER_INPLACE_RESTARTS }`, with
     `RUNNER_INPLACE_RESTARTS = 2` in `config.ts` and a doc line citing compositor ADR 0004.
   - Rewrite the `:11-12` comment to state the new reason (invariant 1), and reference
     `runner.test.ts`'s proof.
   - Re-pin Fly's Machines API field name (`max_retries`) against the Fly docs and record it.
2. **`config.ts`:** `STALE_HEARTBEAT_SECONDS_COMPOSED = 15`. Doc line: compositor spec §8 item 6, "3
   missed beats at the 5 s cadence of `heartbeatIntervalMs`". It must equal
   `3 × RELAY_HEARTBEAT_INTERVAL_MS / 1000`, so **define it as that product**, using R2002's
   `RELAY_HEARTBEAT_INTERVAL_MS` (re-pin its name; it's the value R2002 serves as
   `heartbeatIntervalMs`). Then the beat cadence and the stale rule can't drift apart.
3. **`domain/expiry.ts`:** the stale-beat rule picks the composed threshold when `s.mode === "composed"`.
   Passthrough keeps 90 s. Today's passthrough tests must stay green **unchanged**: that's the
   regression proof.
4. **Exit 78 never retries** (a spec gap found while writing this brief; record it in compositor
   `docs/_INDEX.md`). The compositor exits 78 for a refusal (bad token, 404, wrong mode).
   - `on-failure` will restart it twice, which is harmless, since each run refuses in about 1 s.
   - But then `auto_destroy` → `destroyed` → `afterLostDestroyed` would spend the **web** retry on a
     run that can't succeed.
   - Re-pin how `withExit` records the exit and what `afterLostDestroyed` does with it. Add: an
     observed exit code `78` → signal `failed` with reason `refused`, and no `create_started`.
   - If the `RunnerEndReason`/failure-reason union lacks `refused`, add it and grep **every**
     consumer: the UI copy for a failed session reason must exist in all 4 locales, or this task
     owes the strings.

## Files

- Modify:
  - `apps/web/src/server/relay/runner-fly.ts`
  - `apps/web/src/server/relay/config.ts`
  - `apps/web/src/server/relay/domain/expiry.ts`
  - `apps/web/src/server/relay/domain/runner.ts` (exit-78 arm only)
- Test:
  - `apps/web/src/server/relay/__tests__/runner.test.ts` (or the file holding `RUNNER_TABLE` tests; re-pin)
  - `__tests__/expiry.test.ts`
  - `__tests__/runner-fly.test.ts`
  - `__tests__/config.test.ts`
  - `apps/web/src/server/usecases/__tests__/relay-internal-routes.test.ts` (test 7; the heartbeat
    route `app/api/internal/relay/sessions/[sid]/heartbeat/route.ts`)
- Possibly modify: the 4 locale dictionaries, plus `pnpm i18n:gen-keys`, if a new failure reason
  needs copy.

## Steps

- [ ] **Step 1: Write the failing tests.** Drive them through `decide`/the runner table, with the real
  `evaluate`. Never hand-build the expiry. Use the existing test helpers; re-pin their names.
  1. `in_place_restart_keeps_attempt` (review focus 5):
     - a composed session `playing` at attempt 1, last beat at T
     - observed `pending` (Fly `restarting`) at T+5 s
     - a beat at T+12 s
     - assert no `stale_beat` fired, the runner is still attempt 1, and no `create_machine` effect
       exists anywhere in the sequence
  2. `crash_with_no_beat_within_15s_is_lost_then_one_retry_after_destroyed`:
     - same start; no beat; `evaluate` at T+15 s yields the stale beat → `lost` + FORCE_DESTROY
     - observed `pending` at T+16 s: still `lost`, no create
     - `destroy_ok` → `destroyed` → one `create_started` at attempt 2
     - assert exactly **one** create across the whole sequence
  3. `stale_beat_during_restart_never_runs_two_machines`: across a sweep of observation orders
     (pending/running/destroyed interleaved with stale beats and `destroy_ok`), assert the
     invariant: no step emits `create_machine` while the previous Machine's last observed state is
     not `destroyed`. Print `orders checked: N` and fail on `N == 0`.
  4. `passthrough_keeps_90s_stale`: a passthrough session at T+15 s and T+89 s is not stale; at
     T+90 s it is. Derive 90 from `STALE_HEARTBEAT_SECONDS`.
  5. `composed_stale_is_three_beats`: assert
     `STALE_HEARTBEAT_SECONDS_COMPOSED * 1000 === 3 * RELAY_HEARTBEAT_INTERVAL_MS`.
  6. `exit_78_fails_refused_without_retry`: playing → observed `destroyed` with exit code 78 →
     signal `failed(refused)`, no `create_started`. Positive pair: exit code 1 → the one retry still
     happens.
  7. `slate_on_air_takes_a_composed_session_live` (owner ruling 2026-10-14, plan review F5 option A).
     **Green on base, a regression pin:** web already promotes on `state === "playing"`
     (`stream-sessions.ts` `callback_playing`, `runner.ts` `went_live`; re-pin). It pins that the new
     `programmePhase`/`input` keys never gate going live. Preconditions: `RELAY_IMAGE` set (or R2001's
     `composedUnavailable()` refuses the start), and the runner in `booting` (`callback_playing` is
     handled only from `booting`/`playing`).
     - a composed session in `warming`
     - a beat `{state: "playing", programmePhase: "waiting", input: "connecting"}` through the
       **real** heartbeat route
     - assert the session is `live`, exactly one credit is consumed, and a later beat with
       `programmePhase: "reconnecting"` leaves it `live`, with no second consume
     - positive pair: `{state: "stalled"}` (output down) never takes it live
  8. `create_body_uses_on_failure_restart`: `runner-fly.test.ts` captures the POST body and asserts
     `restart` deep-equals `{ policy: "on-failure", max_retries: 2 }`, with `2` taken from
     `RUNNER_INPLACE_RESTARTS`.
- [ ] **Step 2: Run, expect a FAIL** (tests 1–6 and 8; test 7 is green on base).
  `cd apps/web && pnpm vitest run src/server/relay/__tests__/{runner,expiry,runner-fly,config}.test.ts src/server/usecases/__tests__/relay-internal-routes.test.ts --reporter=json --outputFile=$SCRATCH/r2003.json`.
  Read `numFailedTests`, and confirm `.testResults[].name` resolved to the worktree.
- [ ] **Step 3: Implement changes 1–4.**
- [ ] **Step 4: Run, expect a PASS.** Paste `numPassedTests`/`numTotalTests`.
- [ ] **Step 5: Mutate once each, one at a time.**
  1. In `expiry.ts`, use 90 s for composed. Expected red: test 1 or 2.
  2. Allow `create_started` from `lost`. Expected red: test 3.
  3. Drop the exit-78 arm. Expected red: test 6.
  4. Gate `callback_playing` on `programmePhase === "live"`. Expected red: test 7.
- [ ] **Step 6: Smoke.** Run the relay smoke spec that covers session start/stop. Re-pin it with
  `grep -arl "relay" apps/web/e2e`, choosing **by behaviour, not filename** (AGENTS.md class 16).
  Run it against a local prod build with `RELAY_DRIVERS=fake`.
- [ ] **Step 7: Scoped gate.** Run the five test files (including `relay-internal-routes.test.ts`), plus `stream-sessions` tests touching
  `retryRunner` (re-pin). Lint with `rtk proxy` and typecheck.
- [ ] **Step 8: Commit and open the PR.**
  `feat(relay): in-place restart for the compositor, a 15 s composed stale rule, and refused exits never retry`

## Acceptance

- 8 new tests pass, and existing passthrough tests are unchanged and green.
- 4 mutants went red.
- The D4 comment is rewritten with its new proof.
- The PR is open.

## Do not touch

Provisioning (R2004), the overlay page (R2005), and the UI (R2006), except the locale strings this task
owes.

## Output cap

Under 15 lines: counts, the PR URL, false premises found (for example "guest already from config").
