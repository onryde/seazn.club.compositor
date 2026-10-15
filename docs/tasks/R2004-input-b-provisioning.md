# R2004: Composed provisioning (CF Input B), composed session facts, and the delay override column

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/r2004-input-b -b feat/relay-composed-input-b origin/main`.
**Depends on:** R2003 merged (runs sequentially in `server/relay/*`), and **C00 F-002** (the measured
camera→compositor delay, which becomes the default). **Amended 2026-10-15:** also **R2007 PR A merged**
(`resolveTemplate` in `components/overlay/template-registry.ts`). See "Amendment 2026-10-15 (overlay
templates spec)" at the end. **The stop contract (stop signals and the grace) is not in this PR**: it
ships as its own small PR 0, described in that amendment, ahead of C12 and not gated on R2007
(review m9).

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
   - `fixture_stream_sessions.overlay_delay_ms` is `integer not null default 0`
     (`db/migration/deltas/V410__stream_sessions.sql:120`, pinned by the plan review; re-pin). So
     passthrough keeps writing 0, the column default (the field is unused in that mode), and composed
     writes the effective value. No ALTER to that column.

5. **`overlayUrl`** =
   ~~`${APP_URL}/overlay/fixtures/${fixtureId}?sid=${sessionId}&st=${pageToken}&host=compositor&delay=${overlayDelayMs}`~~
   **(superseded 2026-10-15, decision 7)**
   `${APP_URL}/overlay/fixtures/${fixtureId}?sid=${sessionId}&st=${pageToken}&host=compositor&delay=${overlayDelayMs}&template=${templateId}`.
   `sid` is there because `verifyRelayToken` (`server/relay/tokens.ts:80`) needs the expected sid; the
   page must never trust an unverified claim to find it. The token is in a query string, and
   compositor `redact()` strips every query string from logs (C07).
6. **`heartbeatIntervalMs`** = `RELAY_HEARTBEAT_INTERVAL_MS` (R2002).
7. **Template (amendment 2026-10-15, overlay spec §4.4 / §6).** New column
   `fixture_stream_settings.overlay_template text null`, with **no** check constraint: templates live in
   code (`OVERLAY_TEMPLATES`), and a stored id that is later retired or removed must still read, then
   resolve. `null` means the sport default. At session create, copy the **choice**:
   `theme_id = body.themeId ?? settings.overlay_template ?? null` (the existing column,
   `V410__stream_sessions.sql:119`, already inserted at `stream-sessions.ts:1452-1457`). The running
   session never changes when the setting changes, the same rule as the delay. `overlayUrl` carries
   `template=${resolveTemplate(row.theme_id, row.sport_key).id}`: resolved, so the compositor's URL
   always names a live, non-retired id. `row.sport_key` is the session's admission snapshot (same
   insert). R2006 is the writer of `overlay_template`; this task adds the column and ~~its one reader~~
   its two readers (corrected 2026-10-15, review m10: the capture descriptor too; see change 7).

## Changes

1. **Migration:** `role` + constraint, plus `fixture_stream_settings.overlay_delay_ms`, plus
   `fixture_stream_settings.overlay_template text null` (decision 7, amendment 2026-10-15). Sealed
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
   (Amended 2026-10-15: `overlayUrl` per decision 5 as superseded, with `template=`; `sessionFactsForJob`
   is now at `stream-sessions.ts:2202`.)
6. **Session create copies the template choice** (decision 7): the insert at `stream-sessions.ts:1452-1457`
   reads `settings.overlay_template` in the same statement it already uses for the destination.
   Decide whether passthrough even has a Machine (re-pin); if it doesn't, the passthrough branch
   stays for contract completeness only.
7. **(Amended 2026-10-15, review m10)** The capture descriptor's `overlayUrl` resolves the stored
   choice. See "Second reader" in the amendment.

## Files

- Create: `db/migration/V<next>__relay_composed_input_b.sql`
- Modify:
  - `apps/web/src/server/relay/{ingest-cf.ts,ports.ts,fakes.ts,config.ts}` (`config.ts` for
    `RELAY_OUTPUT_PROFILE`; the stop grace is PR 0's)
  - `apps/web/src/server/usecases/stream-sessions.ts`
  - the provisioning usecase (re-pin)
  - `secret-columns.ts` (re-pin its path)
  - (Amended 2026-10-15, review m10) `apps/web/src/server/usecases/capture-phone.ts` (change 7)
- Test:
  - `server/relay/__tests__/ingest-cf.test.ts`
  - `server/usecases/__tests__/relay-internal-routes.test.ts`
  - the provisioning usecase's test file
  - `server/relay/__tests__/config.test.ts` (the output profile; the grace pin is PR 0's)
  - (Amended 2026-10-15) the capture-GET test file (re-pin; test 11)
  - `enc-boundary.test.ts` (it must still pass: the new sealed columns are named only in
    `secret-columns.ts`)
- Read only (amendment 2026-10-15): `apps/web/src/components/overlay/template-registry.ts`
  (`resolveTemplate`, from R2007). The usecase imports it the way `capture-phone.ts:19` already imports
  the registry today.

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
  4. `effective_delay_is_copied_at_create` (plus: a passthrough create leaves the session column at 0):
     - settings null: the session gets `RELAY_OVERLAY_DELAY_DEFAULT_MS`
     - settings 0: the session gets 0, and the body says `overlayDelayMs: 0`
     - settings 2500, then changed to 4000 after create: the running session still serves 2500
  5. `delay_column_rejects_out_of_range`: −1 and 30001 fail at the DB check.
  6. `teardown_deletes_both_inputs_videos_first`: through the real end/sweep path. Assert the delete
     order per input.
  7. `output_profile_matches_contract_fixtures`: as decision 3.
  8. ~~`overlay_url_carries_st_host_and_delay`: parse the URL and assert the four params.~~
     **(Superseded 2026-10-15.)** `overlay_url_carries_st_host_delay_and_template`: parse the URL and
     assert the **five** params (`sid`, `st`, `host`, `delay`, `template`). The `st` value verifies with
     `verifyRelayToken` scope `relay-page`.
  9. `overlay_url_template_follows_the_stored_choice` (amendment 2026-10-15), through the real route.
     Expected values come from spec §3's default rule (cricket → `bar`, every other sport → `bug`),
     typed in the test, never read from `defaultTemplateFor`:
     - settings `null`, cricket fixture → `template=bar`
     - settings `null`, badminton fixture → `template=bug`
     - settings `'bar'`, football fixture → `template=bar`
     - settings `'gone'` (written by SQL, an id no registry holds), football → `template=bug`, and the
       create does not throw
  10. `template_choice_is_copied_at_create` (amendment 2026-10-15): settings `'bar'` → create → change
      the setting to `'bug'` → the running session's facts still say `template=bar`, and
      `fixture_stream_sessions.theme_id` is `'bar'`. A passthrough create copies it too (`capture-phone.ts:176`
      reads `themeId` for the phone overlay), and a `body.themeId` given at create wins over the setting.
- [ ] **Step 3: Run, expect a FAIL** (JSON reporter, scoped files).
- [ ] **Step 4: Implement changes 1–5.**
- [ ] **Step 5: Run, expect a PASS.** Paste the counts. Apply the migration to a **fresh** local DB per
  the `seazn-local-env` skill (`db:apply` + `sync:sports`), and confirm `show data_directory` is yours.
- [ ] **Step 6: Mutate once each.**
  1. `addOutput(A.id…)` for composed. Expected red: test 1.
  2. Write the setting rather than the effective value. Expected red: test 4.
  3. Return the destination secret as `target`. Expected red: test 3.
  4. (Amendment 2026-10-15) Build `template=` from the live `fixture_stream_settings` row at GET time
     instead of the session's copy. Expected red: test 10.
  5. (Amendment 2026-10-15) Drop `template=` from `overlayUrl`. Expected red: tests 8 and 9.
- [ ] **Step 7: Smoke and e2e.** Run the relay smoke spec (selected by behaviour) against a prod
  build with `RELAY_DRIVERS=fake` for passthrough **and** composed (R2001's refusal is lifted only
  where `RELAY_IMAGE` is set, so set a dummy for this run).
- [ ] **Step 8: Scoped gate.** Run every touched test file, lint (`rtk proxy`), and the typecheck.
  Then open the PR: `feat(relay): composed sessions publish to their own Cloudflare input`. The PR
  body lists every `fixture_stream_inputs` reader and how each was handled.

## Acceptance

- ~~8 tests pass~~ ~~10~~ 11 tests pass (amended 2026-10-15), with passthrough tests unchanged.
- ~~3 mutants~~ ~~5~~ 6 mutants went red (amended 2026-10-15).
- (Amended 2026-10-15) PR 0 (the stop contract) is a **separate** PR; this PR doesn't carry it.
- The migration applies on a fresh DB.
- The destination key never reaches the Machine.
- The PR is open.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.4 and §6, R2004 row (verbatim, `29fa9f59c`): "The composed `overlayUrl` carries `template=` from the
fixture's stored choice." The edits above are marked in place: decision 5 (superseded), decision 7, change 1, change 6,
tests 8–10, mutants 4–5, Acceptance.

**False premise recorded:** the 2026-09-05 ruling "style in the URL, not the DB" read as if nothing
server-side held a style. In fact `fixture_stream_sessions.theme_id` has existed since
`V410__stream_sessions.sql:119`. `CreateStreamSession.themeId` accepts it (`schemas.ts:1387`), the job
facts serve it (`stream-sessions.ts:2207`), and `capture-phone.ts:176` reads it. Only the panel never
writes it (its go-live posts `{ mode, targetId }`, `fixture-stream-panel.tsx:1205`). This task stores
the choice on the fixture's settings and copies it into that existing column. It adds no second column
on the session.

**Second reader: the capture descriptor's `overlayUrl` (review m10; coordinator decision).**
`capture-phone.ts:176` builds the capture API's `overlayUrl` as `s.themeId ?? defaultThemeFor(ctx.sport_key)`.
`s.themeId` is the latest session's `theme_id`, passed at `:226` and `:631`. So before any session, or
after a re-pick, the field names the sport default or the old template, not the stored choice.
- **Change 7:** add `st.overlay_template` to the context select. That query already joins
  `fixture_stream_settings st` at `:159`. Then
  `theme = resolveTemplate(s.themeId ?? ctx.overlay_template, ctx.sport_key).id`. That is the
  coordinator's `s.themeId ?? resolveTemplate(settings.overlay_template, sport).id`, except that a
  retired session id also resolves. The URL keeps `?style=` (the page reads both, R2007 decision 2).
- **Test 11:** in the capture-GET tests (re-pin the file), with no session and
  `overlay_template = 'bug'` on a cricket fixture, the descriptor's `overlayUrl` carries `style=bug`.
  With no stored choice, it carries `style=bar`, cricket's default from the registry entry.
- **Mutant 6:** read `defaultThemeFor` again. Expected red: test 11.
- The 'one reader' in decision 7 becomes **two readers**: `sessionFactsForJob` and `captureCommon`.
- **Recorded fact, not a reason to skip:** today no phone reads this field. The score preview
  WebView was removed by owner ruling 2026-10-08 (`seazn.club.capture` `AGENTS.md` §7: "The
  descriptor still sends `overlayUrl`; nothing on the phone reads it"). The field is still public
  API v1 (`capture-schemas.ts:62`, `:169`), so it must not name a stale template. No e2e can witness
  it, so test 11 is the only witness.

### PR 0: the relay stop contract (its own small PR; replaces the old Step 0)

**Why it's separate (review m9).** C12's grace test can't go green until this ships. Inside R2004 it
would wait on R2007 PR A, and so on the end-card-undo fix, which puts the compositor on the overlay
critical path. As its own PR, it waits only on R2003.

- **Worktree:** `git worktree add ../seazn.club-wt/relay-stop-contract -b fix/relay-stop-contract origin/main`,
  **after R2003 (#967) merges**: #967 edits `runner-fly.ts`, `fly-client.ts`, `config.ts` and
  `domain/runner.ts`.
- **The facts.** These are re-pinned on R2003's tree (`origin/feat/relay-runner-restart` `f9034f575`),
  with the main `b01d07cd5` lines in brackets. The file is `server/relay/domain/runner.ts`, not
  `server/relay/runner.ts` (corrected, review):
  - `STOP` = `{ stop_machine, signal: RUNNER_STOP_SIGNAL, timeoutSeconds: RUNNER_STOP_GRACE_SECONDS }`
    is at `:143` (`:101`), and `toStopping` at `:160-161` (`:117-118`).
  - The playing row's `deadline` and `session_stop` are at `:324-325` (`:251-252`).
  - In `config.ts`, `RUNNER_STOP_GRACE_SECONDS = 10` is at `:146` (`:141`) and
    `RUNNER_STOP_SIGNAL = "SIGINT"` at `:161` (`:147`).
  - `fly-client.ts:464-468` `stopMachine` POSTs `{ signal, timeout: "<n>s" }` and doesn't wait.
  - `runner-fly.ts:230-238` creates the Machine with no `stop_config`.
- **Fly's API (docs.fly.io, fetched 2026-10-15; cite these in the code comments):**
  - `docs.fly.io/machines/api/machines-resource`, "Stop a Machine": the body takes `signal` ("Signal
    to stop the Machine with. (default: SIGINT)") and `timeout` ("Seconds to wait before sending
    SIGKILL to the Machine"). So web's explicit `signal` on its own stop is supported.
  - The same page, "Machine `config` object properties", defines `stop_config`. Its `signal` is "The
    name of the signal to send to the entrypoint process on the Machine to initiate shutdown", and
    its `timeout` is "How long in nanoseconds to wait, after signaling the entrypoint process, before
    hard-shutdown of the Machine". Both default to nil.
  - `docs.fly.io/reference/configuration`: `kill_signal` defaults to SIGINT, and `kill_timeout`
    defaults to **5 s** (max 300). They apply "during controlled shutdowns such as" `fly deploy`,
    `fly machine stop` and `auto_stop_machines`.

  So **today a platform-initiated stop arrives as SIGINT with a 5 s timeout** (review I4). C06 would
  show "Stream ended" for a host stop and be killed mid-hold.
- **Part 1: the signal split (coordinator decision I4, 2026-10-15).**
  - Composed Machines are created with `stop_config: { signal: "SIGTERM", timeout: <RUNNER_STOP_GRACE_SECONDS in the docs' unit> }`.
    Re-pin whether `runner-fly.ts` creates anything but composed Machines; if it doesn't, that's every
    create. `MachineCreateInput` (`fly-client.ts`) gains the optional field.
  - Web's own stop keeps passing `signal: RUNNER_STOP_SIGNAL` (SIGINT) explicitly.
  - Result: **our stop arrives as SIGINT and C06 holds the Final scene**. A Fly-initiated controlled
    stop arrives as **SIGTERM, and C06 drains at once** (no Final card), within our grace instead of
    Fly's 5 s.
  - Fly literals stay in `runner-fly.ts` and `fly-client.ts` (ADR 0007).
  - **The timeout's unit is checked, not assumed.** The docs type it as an int in nanoseconds; send
    that. The live leg reads the Machine back and asserts `config.stop_config` equals what was sent.
    If Fly normalises it to another form, use the form that round-trips and record it.
  - **Precedence is measured, not assumed.** Does the stop request's explicit `signal` win over
    `stop_config.signal`? If it didn't, our own stop would arrive as SIGTERM and the Final scene would
    never show.
    - Measure once on the stg Fly app with a throwaway probe Machine created with
      `stop_config.signal = SIGTERM`. Its entrypoint is
      `sh -c 'trap "exit 3" INT; trap "exit 4" TERM; while :; do sleep 1; done'`, a public `busybox`
      image, and it has no `auto_destroy`.
    - Send `POST /stop { "signal": "SIGINT" }`, then read the exit code from the Machine's events.
      Expected: **3**. Also send a stop with no `signal`. Expected: **4**.
    - Destroy the probe and confirm it's gone.
    - Record the result as `F-0xx: Fly stop signal precedence` in compositor `_FINDINGS.md`.
    - **If the explicit signal loses, halt** and route to the coordinator. Don't ship Part 1.
- **Part 2: the grace. OWNER DECISION PENDING (option A).**
  - **Recommendation, not a ruling:** raise `RUNNER_STOP_GRACE_SECONDS` from 10 to 20, which fits
    hold 10 + drain 7 + 3 headroom. Option B, shortening the final hold to fit 10 s, leaves 0 s for
    the hold.
  - **Part 2 ships only on the owner's yes.** If the owner picks B, Part 2 doesn't ship; C06's
    `final_hold_ms` and its spec-number test change by owner ruling instead. Part 1 ships either way.
  - With A, update the arithmetic comment above `ENDING_TIMEOUT_SECONDS` (`config.ts:125-135` on main;
    re-pin on R2003's tree):
    - ordinary teardown: 20 + 20 + 55 + 55 = **150 s** < 300
    - with a destroy wait: 40 + 55 + 55 + 55 = **205 s** < 300

    `ENDING_TIMEOUT_SECONDS` stays at 300. The doc comment on `RUNNER_STOP_GRACE_SECONDS` itself
    (today "the supervisor's flush budget is ≤ 10 s", from R0's ffmpeg) gains the compositor's budget:
    hold 10 + drain 7 + headroom 3. Also confirm:
    - `grace_expired` (grace + `RUNNER_OBSERVE_SLACK_SECONDS` = 40 s) stays below it
    - `MACHINE_MINUTES_BOUND` is unchanged (`ceil(20 / 60)` = 1; `domain/runner.ts:93-94` on main)
    - `runner-fly.test.ts`'s composed-teardown term moves with the constant
- **Tests (failing first; JSON reporter):**
  - `runner-fly.test.ts`: the create body carries `stop_config: { signal: "SIGTERM", timeout: … }`, with
    the timeout derived from `RUNNER_STOP_GRACE_SECONDS`; the stop still posts `signal: "SIGINT"`.
  - `runner-fly.live.test.ts` (opt-in, `RELAY_LIVE_FLY=1`, the file's existing residue rule): a
    composed create reads back the same `stop_config`.
  - With A, `config.test.ts` pins `RUNNER_STOP_GRACE_SECONDS * 1000 >= 10_000 + 7_000 + 3_000`, each
    number commented from its source: spec §4.2, compositor `config.rs` `drain_timeout_ms`, and the
    3 s headroom in that field's doc line.
- **Mutants:**
  - `stop_config.signal` set to `"SIGINT"`. Expected red: the runner-fly test.
  - `stop_config` dropped. Expected red: the same test.
  - With A, the grace back to 10. Expected red: `config.test.ts`.
- **Commit:** `fix(relay): composed Machines stop on SIGTERM from the platform and SIGINT from us`,
  plus `, with a 20 s grace` when A ships. Record the parts that shipped in compositor `docs/_INDEX.md`.
  C12's `final_hold_and_drain_fit_inside_r1_stop_grace` reads the same value.

## Do not touch

The overlay page (R2005), the UI (R2006), and the runner (R2003). (Amended 2026-10-15) The stop
contract is PR 0's, not this PR's.

## Output cap

Under 15 lines: counts, the PR URL, the recording-consumer re-pin result, deviations.
