# W04: Composed provisioning (CF Input B), composed session facts, and the delay override column

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/w04-input-b -b feat/relay-composed-input-b origin/main`.
**Depends on:** W03 merged (runs sequentially in `server/relay/*`), and **C00 F-002** (the measured
camera→compositor delay, which becomes the default).

## Context

Pinned 2026-10-14 at `09290de3c`. **Re-pin everything.**

### Today's state

- `sessionFactsForJob` (`server/usecases/stream-sessions.ts:2180`) returns `target` = the org
  destination's own secret (`readTargetSecret`, `:2183`). R1's composed design had **the Machine
  push straight to YouTube**.
  - **Spec §5.3 changes this.** A composed Machine pushes to **CF Input B** over RTMPS, and the
    destinations are CF Outputs on **B**. The session keeps its CF recording and Output fan-out, and
    the destination key never reaches the Machine.
- `overlayDelayMs: row.overlay_delay_ms`: that session column is **never written** (a false premise,
  recorded in `docs/_INDEX.md`).
- `ingest-cf.ts`:
  - `createLiveInput` (`:232`) returns `{ rtmps, srt }`, which are **ingest** (publish) credentials.
    The CF response's `srtPlayback` (`url`, `streamId`, `passphrase`) is not read at all.
  - The `:15-19` comment explains that secret-bearing fields are split and sealed through
    `secret-columns.ts`.
  - `addOutput` (`:326`) is "passthrough only, exactly once" (`ports.ts:78`).
- `fixture_stream_inputs` has no role column, and is unique on `(session_id, slot)`.
- Migrations live in `db/migration/` (Flyway). **Take the next free `V` at build time. Never pin a
  version number in a doc.**

### What composed needs

Input **A** (the camera, as today) plus input **B** (the programme):
- the compositor **reads A** over SRT playback (`input`)
- the compositor **publishes B** over RTMPS (`target`)
- the destination Outputs attach to **B**

## Decisions this brief makes

The orchestrator puts these to the owner if any re-pin contradicts them:

1. **`fixture_stream_inputs.role`:** `text not null default 'camera' check (role in ('camera','programme'))`.
   Unique becomes `(session_id, role, slot)`. Greenfield stance: amend the constraint in the same
   migration.
2. **Recording.**
   - Input B records (`automatic`, same settings as A today), because the VOD with the score bug is
     the product.
   - Input A's recording mode for composed sessions is `off`. **Re-pin first:**
     `grep -arn "recording\|listVideos\|video_id" apps/web/src` for any consumer reading the
     camera's recording. If one exists (replay link, highlights), **stop and report**, because it's
     an owner question.
   - `fillReplayUrl` (`:2193`) is unaffected: it reads the destination watch URL.
3. **Output profile flag.** `RELAY_OUTPUT_PROFILE` = `720p` (default) or `1080p`, read in `config.ts`
   by `relayOutputProfile(env)`, which returns:
   - `720p`: `{ width: 1280, height: 720, fps: 30, videoKbps: 4000 }`
   - `1080p`: `{ width: 1920, height: 1080, fps: 30, videoKbps: 6000 }`

   Doc line: compositor ADR 0005. **These must equal compositor `OutputProfile`.** The contract
   fixtures `session-composed-{720p,1080p}.json` carry the same numbers. A test here asserts
   `relayOutputProfile` output deep-equals the matching fixture's `output`, read from
   `docs/contracts/fixtures/relay-job.v1/`.
4. **Delay.**
   - `RELAY_OVERLAY_DELAY_DEFAULT_MS = <F-002 median>`, with a doc line citing F-002 and its date.
   - New column `fixture_stream_settings.overlay_delay_ms int null check (overlay_delay_ms between 0 and 30000)`.
     `null` means "use the default", and `0` is a real override.
   - At session create, write the **effective** value
     (`settings.overlay_delay_ms ?? RELAY_OVERLAY_DELAY_DEFAULT_MS`) into
     `fixture_stream_sessions.overlay_delay_ms`. A running session therefore never changes when the
     default or the setting changes.
   - Re-pin the session column's nullability. If it's nullable, keep it nullable for passthrough
     (null), and write it for composed.
5. **`overlayUrl`** =
   `${APP_URL}/overlay/fixtures/${fixtureId}?sid=${sessionId}&st=${pageToken}&host=compositor&delay=${overlayDelayMs}`.
   `sid` is there because `verifyRelayToken` (`server/relay/tokens.ts:80`) needs the expected sid; the
   page must never trust an unverified claim to find it. The token is in a query string, and
   compositor `redact()` strips every query string from logs (C07).
6. **`heartbeatIntervalMs`** = `RELAY_HEARTBEAT_INTERVAL_MS` (W02).

## Changes

1. **Migration:** `role` + constraint, plus `fixture_stream_settings.overlay_delay_ms`. Sealed
   columns for B's RTMPS stream key and A's SRT playback stream id and passphrase follow
   `secret-columns.ts`'s existing pattern. Re-pin how A's SRT ingest secrets are sealed today, and
   mirror it exactly.
2. **`ingest-cf.ts`:** `createLiveInput` also returns `srtPlayback: { url, streamId, passphrase }`
   (the url bare, the secrets as separate fields), and accepts `recording: "automatic" | "off"` in
   `IngestCreateSpec`. Extend `IngestCredentials` in `ports.ts`, and `fakes.ts` to match. **Fakes
   first** (house rule).
3. **Provisioning usecase** (re-pin where passthrough provisioning creates A and calls `addOutput`).
   For composed:
   - create A (recording off) and B (recording automatic)
   - store B with `role='programme'`
   - `addOutput(B.id, target)` **instead of** A
   - the `ports.ts:78` comment becomes "the session's output input, exactly once"

   Passthrough is byte-for-byte unchanged, and the existing passthrough tests must stay green
   **unmodified**.
4. **Teardown:** the session end, the sweep and `deleteInput` handle B exactly as A: videos first,
   then the input (C2). Re-pin every place that iterates a session's inputs, and make it iterate
   both roles. `grep -arn "fixture_stream_inputs" apps/web/src` gives the full list in the PR body.
5. **`sessionFactsForJob`:** for composed, return
   `{ …today, target: { url: B.rtmps.url, streamKey: B.rtmps.streamKey }, input: { protocol: "srt", url: A.srtPlayback.url, streamId, passphrase }, output: relayOutputProfile(), heartbeatIntervalMs, overlayUrl, overlayDelayMs: row.overlay_delay_ms }`,
   validated by `RelayJobSession.parse` before returning. For passthrough, return today's shape.
   Decide whether passthrough even has a Machine (re-pin); if it doesn't, the passthrough branch
   stays for contract completeness only.

## Files

- Create: `db/migration/V<next>__relay_composed_input_b.sql`
- Modify:
  - `apps/web/src/server/relay/{ingest-cf.ts,ports.ts,fakes.ts,config.ts}`
  - `apps/web/src/server/usecases/stream-sessions.ts`
  - the provisioning usecase (re-pin)
  - `secret-columns.ts` (re-pin its path)
- Test:
  - `server/relay/__tests__/ingest-cf.test.ts`
  - `server/usecases/__tests__/relay-internal-routes.test.ts`
  - the provisioning usecase's test file
  - `server/relay/__tests__/config.test.ts`
  - `enc-boundary.test.ts` (it must still pass: the new sealed columns are named only in
    `secret-columns.ts`)

## Steps

- [ ] **Step 1: Fakes first.** Extend `fakes.ts` so a fake `createLiveInput` returns `srtPlayback`
  and honours `recording`. Write a fake-level test.
- [ ] **Step 2: Write the failing tests.**
  1. `composed_creates_two_inputs_and_attaches_output_to_b`: through the real provisioning usecase
     with fakes. Assert two `createLiveInput` calls (A with `recording: "off"`, B with
     `"automatic"`), one `addOutput` whose inputId is **B's**, and two `fixture_stream_inputs` rows
     with roles `camera` and `programme`.
  2. `passthrough_provisioning_is_unchanged`: the existing test, untouched. If none asserts the
     `addOutput` input id, add one asserting it's A.
  3. `composed_job_facts_point_input_at_a_playback_and_target_at_b`: GET through the **real route**
     with a job token. `RelayJobSession.parse(body)` succeeds, `input.streamId` equals A's playback
     stream id, `target.streamKey` equals B's key, and the destination's own key appears **nowhere**
     in the body (scan it).
  4. `effective_delay_is_copied_at_create`:
     - settings null: the session gets `RELAY_OVERLAY_DELAY_DEFAULT_MS`
     - settings 0: the session gets 0, and the body says `overlayDelayMs: 0`
     - settings 2500, then changed to 4000 after create: the running session still serves 2500
  5. `delay_column_rejects_out_of_range`: −1 and 30001 fail at the DB check.
  6. `teardown_deletes_both_inputs_videos_first`: through the real end/sweep path. Assert the delete
     order per input.
  7. `output_profile_matches_contract_fixtures`: as decision 3.
  8. `overlay_url_carries_st_host_and_delay`: parse the URL and assert the four params. The `st`
     value verifies with `verifyRelayToken` scope `relay-page`.
- [ ] **Step 3: Run, expect a FAIL** (JSON reporter, scoped files).
- [ ] **Step 4: Implement changes 1–5.**
- [ ] **Step 5: Run, expect a PASS.** Paste the counts. Apply the migration to a **fresh** local DB per
  the `seazn-local-env` skill (`db:apply` + `sync:sports`), and confirm `show data_directory` is yours.
- [ ] **Step 6: Mutate once each.**
  1. `addOutput(A.id…)` for composed. Expected red: test 1.
  2. Write the setting rather than the effective value. Expected red: test 4.
  3. Return the destination secret as `target`. Expected red: test 3.
- [ ] **Step 7: Smoke and e2e.** Run the relay smoke spec (selected by behaviour) against a prod
  build with `RELAY_DRIVERS=fake` for passthrough **and** composed (W01's refusal is lifted only
  where `RELAY_IMAGE` is set, so set a dummy for this run).
- [ ] **Step 8: Scoped gate.** Run every touched test file, lint (`rtk proxy`), and the typecheck.
  Then open the PR: `feat(relay): composed sessions publish to their own Cloudflare input`. The PR
  body lists every `fixture_stream_inputs` reader and how each was handled.

## Acceptance

- 8 tests pass, with passthrough tests unchanged.
- 3 mutants went red.
- The migration applies on a fresh DB.
- The destination key never reaches the Machine.
- The PR is open.

## Do not touch

The overlay page (W05), the UI (W06), and the runner (W03).

## Output cap

Under 15 lines: counts, the PR URL, the recording-consumer re-pin result, deviations.
