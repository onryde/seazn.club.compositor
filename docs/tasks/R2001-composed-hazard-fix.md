# R2001: Refuse composed mode until the compositor image exists, and repoint the docs

**Repo:** `onryde/seazn.club`. Work in a worktree:
```bash
cd /Users/ashokhein/github/seazn.club && git worktree add ../seazn.club-wt/w01-composed-hazard -b fix/composed-mode-refusal origin/main
```
Follow the `seazn-local-env` skill for the DB and worktree setup (`~/.claude/skills/seazn-local-env/SKILL.md`).

**Depends on:** nothing. Ships first, on its own.

## Context: a live hazard

Pinned 2026-10-14 on main `09290de3c`. **Re-pin every line before editing.** The brief is a
hypothesis, not a source of truth.

- `apps/web/src/server/api-v1/schemas.ts:1358` `StreamMode` and `:1386` `CreateStreamSession` accept
  `mode: "composed"` with no gate.
- `apps/web/src/server/usecases/stream-sessions.ts:1219` `createSession` → `:1255`
  `startBroadcast(actor, fixtureId, opts, deps)` passes `body.mode` straight through at `:1235`.
- `admit()` (`apps/web/src/server/relay/domain/session.ts:94`) checks 9 refusals in this order:
  1. `overlay_required`
  2. `plan_lacks_overlay`
  3. `plan_lacks_relay`
  4. `active_session`
  5. `phone_not_paired`
  6. `phone_not_responding`
  7. `no_credits`
  8. `target_not_found`
  9. `storage_exhausted`

  None of them looks at the mode. (Order corrected by the plan review; re-pin it anyway. The table
  test below builds an input with **all** other refusals true, so it doesn't depend on the order.)
- The composed branch (`stream-sessions.ts:1493`/`:1515` `provisionSession`) applies runner
  `create_started`. Only then does `FlyRunner`'s constructor (`server/relay/runner-fly.ts:199-215`)
  throw on missing `RELAY_IMAGE`. `drivers.ts:90-112` `lazyRunner()` wraps that throw in
  `createRefusedBeforeCall`.
- **By the time the throw happens, the session row, the Cloudflare input and provisioning already
  exist.** A comment at `drivers.ts:84` says "composed is disabled this wave", but no code enforces it.
- `refuse()` (`stream-sessions.ts:1120`) maps refusals to HTTP. 503 is already used for
  `storage_exhausted` and `ingest_unavailable`.
- `relayUnavailable()` is already read by `server/stream-panel-context.ts:37`. Use the same predicate,
  so that the UI and the API agree on "composed is unavailable".

## Files

- Modify: `apps/web/src/server/relay/domain/session.ts` (the `AdmitRefusal` union and `admit()`)
- Modify: `apps/web/src/server/usecases/stream-sessions.ts` (pass the availability fact into `admit`;
  map the refusal in `refuse()`)
- Modify: the OpenAPI/zod error enum, if refusal codes are enumerated there. Grep for
  `storage_exhausted` with `git grep -a`.
- Test: `apps/web/src/server/relay/domain/__tests__/session.test.ts`,
  `apps/web/src/app/api/v1/fixtures/[id]/stream-sessions/__tests__/routes.test.ts`,
  `apps/web/src/server/usecases/__tests__/stream-sessions.test.ts`
- Docs: none. The pointer note and the superseded header shipped separately in `seazn.club` PR #957.

## Behaviour

- A new refusal `composed_unavailable` is checked **first** in `admit()`, when `mode === "composed"`
  and the composed runner is unavailable. Unavailable means `relayUnavailable()` is true, or
  `RELAY_IMAGE` is unset. Put that in **one** exported predicate, `composedUnavailable()`, beside
  `relayUnavailable()` in `server/relay/drivers.ts`. The usecase reads it, and R2006 (the organiser UI)
  imports it for `relayOffer`. `admit()` stays pure and receives a boolean.
- It maps to **HTTP 503** with body code `composed_unavailable`.
- **No write happens before it.** There's no session row, no `createLiveInput`, and no runner call.
- Passthrough is unchanged. The phone path (`capture-phone.ts:709`) and auto path
  (`stream-auto.ts:126`) both call `startBroadcast`. Confirm they never pass `composed`, and keep their
  tests green.

## Steps

- [ ] **Step 1: Failing domain test** (`session.test.ts`). Add these four cases:
  - composed + unavailable → `composed_unavailable`
  - composed + unavailable + every other refusal also true → still `composed_unavailable`, because it
    comes first in the order
  - composed + available → the existing refusal order is unchanged. Pick one existing refusal case
    and run it with `mode: "composed"`.
  - passthrough + unavailable → no `composed_unavailable`
- [ ] **Step 2: Run it and watch it fail.**
  ```bash
  cd apps/web && pnpm vitest run src/server/relay/domain/__tests__/session.test.ts --reporter=json --outputFile=/tmp/w01-a.json
  ```
  Read `numFailedTests`. Use your own scratch subdir for the output file, not a shared path.
- [ ] **Step 3: Implement it in `admit()`.**
- [ ] **Step 4: Failing route test** (`routes.test.ts`, next to the existing composed cases at about
  `:219`/`:407`/`:453`/`:494`). POST `mode: "composed"` with the runner unavailable, then assert:
  - status 503 and code `composed_unavailable`
  - **zero** `fixture_stream_sessions` rows for the fixture
  - `FakeIngest`'s create-call count is **0**
  - `FakeRunner`'s create-call count is **0**

  Positive pair: the same POST with the runner available still returns 201. Without this pair, the
  negative assertion could pass vacuously.
- [ ] **Step 5: Implement it in the usecase.** Read availability once at `startBroadcast` entry,
  before any DB transaction. Map the refusal in `refuse()`.
- [ ] **Step 6: Run the scoped tests and paste the counts from the JSON reporter.**
  - `session.test.ts`
  - `routes.test.ts`
  - `stream-sessions.test.ts`
  - `stream-go-live-phone.test.ts` (phone path regression)
  - `drivers.test.ts`
- [ ] **Step 7: Mutate once.** Move `composed_unavailable` to the last position in the `admit` order.
  The "first in order" domain test must go red. Revert, and note it in the commit body.
- [ ] **Step 8: Docs.** Nothing to do (PR #957). Confirm it merged: the note is at the top of
  `docs/superpowers/specs/2026-09-05-stream-overlay-prompts/_INDEX.md`.
- [ ] **Step 9: Lint and typecheck the changed files.** Run `rtk proxy pnpm --filter web lint` and read
  `✖ N problems`. Run the repo's typecheck command for `apps/web`.
- [ ] **Step 10: Commit, push and open the PR.** Smoke runs on PRs only, and this is a behaviour
  change, so it needs a PR.
  ```bash
  git commit -m "fix(stream): refuse composed mode before any write while the compositor image is unconfigured"
  ```
  The PR body names the hazard, the order, the four asserted zero counts, and the mutation.

## Acceptance

- A composed POST with the runner unavailable returns 503 `composed_unavailable`, with 0 rows, 0
  ingest calls and 0 runner calls.
- Passthrough and the phone path stay green.
- The mutant went red.
- PR #957's docs pointer is on `main`.

## Do not touch

`runner-fly.ts` (that's R2003), the panel UI (R2006), and the composed provisioning flow (R2004).

## Output cap

Final message under 15 lines: the PR URL, the scoped test counts (from JSON), the mutant, and any
deviations.
