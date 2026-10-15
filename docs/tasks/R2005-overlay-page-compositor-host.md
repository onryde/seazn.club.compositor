# R2005: Overlay page for the compositor (`sid`+`st` access, `host=compositor` signals, stale, clamp)

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/r2005-overlay-host -b feat/overlay-compositor-host origin/main`.
**Depends on:** R2004 merged (sequential with R2003/R2004), and **C00 F-001** (the readiness mechanism the
CEF adapter observes). **Amended 2026-10-15:** also **R2007 PR A merged** (template registry,
`OVERLAY_SCENES`, `OverlayPreview`, the gallery and its sweeps), **R2007 PR B merged** (`final` and
`phaseChannel` on the heartbeat; `lib/relay-phase.ts`), and **C11 Step 0's result in `_FINDINGS.md`**
(whether CEF 139 lets the page reach loopback; it decides which channel is primary). See "Amendment
2026-10-15 (overlay templates spec)" at the end. **Use the `frontend-design` skill** (house rule for every UI task), even
though the page is transparent: the compositor output is what viewers see.

## Context

Pinned 2026-10-14 at `09290de3c`. **Re-pin everything.**

`app/overlay/fixtures/[fixtureId]/page.tsx` (172 lines) has:

| Lines | What |
|---|---|
| `:3-7` | two server-side `notFound()` gates: public visibility (`publicFixtureSlugs` + `getPublicFixture`) and the `streaming.overlay` entitlement |
| ~~`:55`~~ `:54` (re-pinned 2026-10-15 at `b01d07cd5`) | the query is `{ style, lang, delay, <OVERLAY_KEY_PARAM> }`; R2007 adds `template` |
| `:65` (added 2026-10-15) | the query destructure |
| ~~`:70`~~ `:75` (re-pinned 2026-10-15) | `resolveDelayMs(delay)`: unparseable or out of range ⇒ 0, never throws; max 300 000 ms (`lib/overlay-delay.ts`) |
| `:157` (added 2026-10-15) | the template resolution (R2007: `resolveTemplate(templateParam(…), sport).id`) |

Also relevant:
- `layout.tsx:26` is already transparent. The spec's "render a transparent body" is **already true**;
  record it as a false premise.
- Live data comes from `loadOverlayLiveData` (`server/overlay/load.ts:235`), threaded through
  `useLiveFixture` / `useOverlayClock` (`presentationNowOffsetMs`).
- **Re-pin the page's poll interval and realtime path** before step 1. The `lib/overlay-delay.ts`
  comment says `POLL_MS` 15 s.
- `verifyRelayToken(token, { sid, scope })` (`server/relay/tokens.ts:80`) has an expected sid and is
  **unconsumed** for `relay-page` today. R2004 builds
  `overlayUrl = …?sid=<sid>&st=<token>&host=compositor&delay=<ms>`.

A composed session can stream a fixture that is **not public**: unlisted, or a private competition.
Today's visibility gate would 404 the compositor's page, and the stream would go out without a
score bug. That's an inert seam the owner would only see live.

## Changes

1. **Relay access path.** If `sid` and `st` are both present:
   - verify with `verifyRelayToken(st, { sid, scope: "relay-page" })`
   - then load the session row and require all of: `session.fixture_id === fixtureId`, the session
     is not ended, and `mode === "composed"`
   - any failure ⇒ `notFound()`, the same opaque 404 as today. Never a 401: the route's contract is
     "indistinguishable from missing"

   On success, read the fixture through a **non-visibility-filtered** read with the same data shape
   (re-pin whether `getPublicFixture` can take a bypass; otherwise add a sibling).
   - **Security:** the bypass read must **never share a cache entry** with the public read. Give it
     its own `unstable_cache` key prefix, or no cache, and add a test that a private fixture read
     through the bypass is still 404 on the public path straight afterwards.
   - The entitlement gate still applies.
   - Without `sid`/`st`, today's behaviour is byte-identical.
2. **`host=compositor`** turns on the signals. The page never sets them otherwise, so OBS users see no
   change.
   - **Ready:** signal through **exactly** F-001's mechanism, once, when all of these hold:
     - the first live-data snapshot has rendered
     - `document.fonts.ready` resolved
     - the theme assets loaded
   - **Score stale:**
     - `scoreStale = true` when the last *successful* live-data update is older than
       `SCORE_STALE_AFTER_MS`
     - `false` on the next success
     - signalled on the same channel as ready, **on change only**

     `SCORE_STALE_AFTER_MS = 10_000` (compositor spec §6). If the re-pinned poll interval is above
     5 s, a 10 s rule fires between healthy polls. In that case set the compositor host's poll
     interval to `5_000`, which is `STREAM_POLL_MS` in `lib/stream-session-view.ts`; reuse that
     constant rather than adding a third. Record the choice.
   - Put both thresholds in `lib/overlay-compositor.ts`, with doc lines.
3. **Clamp** (spec §6, clock skew).
   - **Re-pin first** which timestamps the presentation clock schedules against: server-assigned or
     scorer-device.
   - **If any is device-sourced:** an event whose time is more than 2 000 ms past
     `now + delayMs` is treated as `now + delayMs` (`OVERLAY_FUTURE_CLAMP_MS = 2_000`).
   - **If all are server-assigned:** record that as a false premise in compositor `docs/_INDEX.md`;
     Clock skew is then a non-issue for R2, and no chaos case covers it. Add a test that pins the server-assigned
     fact, so a future device-sourced timestamp reds it.
4. **Void re-derivation.** Re-pin that a void yields a snapshot the page re-derives from (no
   append-only display state). Add a test only if no existing one covers "void after delay window".
5. **Scene layer** (amendment 2026-10-15; spec §4.1–4.2). See the amendment section for the table,
   the z-order rule and the card variants.
6. **Phase channel: loopback primary, fallback B** (amendment 2026-10-15; spec §4.3). See the amendment.
7. **Moments during scenes** (amendment 2026-10-15; spec §4.8). See the amendment.

## Files

- Modify: `apps/web/src/app/overlay/fixtures/[fixtureId]/page.tsx`
- Create: `apps/web/src/lib/overlay-compositor.ts` (thresholds, the signal helper, the clamp)
- Modify: the live-fixture hook / `overlay-stage.tsx` (re-pin) to call the signal helper when
  `host=compositor`
- Possibly create: a bypass fixture read in `server/public-site/data.ts` or `server/overlay/` (re-pin)
- Test: `app/overlay/__tests__/` (existing dir), `lib/__tests__/overlay-compositor.test.ts`, and an
  e2e spec `apps/web/e2e/overlay-compositor.spec.ts`
- **Amendment 2026-10-15:**
  - Create: `apps/web/src/components/overlay/overlay-scene.tsx` (`sceneFor`, the backdrop and the scene
    card) and `apps/web/src/components/overlay/use-programme-phase.ts` (the channel hook)
  - Modify: `components/overlay/overlay-scenes.ts` (append three scenes), `overlay-slate.tsx` (two card
    variants), `overlay-stage.tsx` (`host`, `sceneOverride`, moment discard), `moment-queue.ts` (a
    `discard` action), `app/api/v1/public/fixtures/[id]/overlay/route.ts` and
    `components/public-site/live-score-data.ts` `fetchOverlayFixture` (fallback B), `app/globals.css`
  - Modify: `apps/web/src/dictionaries/{en,es,fr,nl}/public.json` and `ui.json`, then
    `pnpm i18n:gen-keys`
  - Test: `components/overlay/__tests__/overlay-scene.test.ts`,
    `components/overlay/__tests__/moment-queue.test.ts` (extend),
    `app/api/v1/public/fixtures/[id]/overlay/__tests__/route.test.ts` (extend), and the e2e above

## Steps

- [ ] **Step 1: Write the failing unit tests** (`overlay-compositor.test.ts`, pure):
  - stale flips at exactly `SCORE_STALE_AFTER_MS` after the last success, not before
  - stale clears on the next success
  - the signal is emitted on change only (a sequence of 5 identical states ⇒ 1 emit)
  - the clamp: an event 2 001 ms past `now + delay` is clamped; 1 999 ms is not
  - the clamp is not applied when the delay is 0 and the event is in the past
  - **(amendment 2026-10-15)** tests A1–A3 from the amendment section
- [ ] **Step 2: Write the failing page tests.** Extend the existing `app/overlay/__tests__`; re-pin its
  render harness.
  1. A private fixture plus a valid `sid`/`st` for a composed session on it: 200.
  2. The same without `st`: 404.
  3. `st` for another session's sid: 404.
  4. `st` with scope `relay-job`: 404.
  5. An ended session: 404.
  6. A session on a different fixture: 404.
  7. **Cache isolation:** a bypass read of a private fixture, then the public read of it, is still
     404.
  8. Public fixture, no params: byte-identical to before. Assert on `="` anchors, not bare `data-*`
     (AGENTS.md trap).
  9. **(Amendment 2026-10-15)** test A4 from the amendment section (the overlay route's fallback-B
     field and its cache isolation).
- [ ] **Step 3: Write the failing e2e** (`overlay-compositor.spec.ts`). Against a local prod build,
  seed a composed session with fakes (`RELAY_DRIVERS=fake`) and open the overlay URL from
  `sessionFactsForJob` (the **real producer**, not a hand-built URL).
  - Assert the F-001 ready signal is observed exactly once.
  - Stop the live-data route (route interception), and assert the stale signal `true` within
    `SCORE_STALE_AFTER_MS` + one poll, then `false` after restoring it.
  - Open the same URL without `host=compositor`, and assert no signal is ever emitted.
  - This page is a 1920×1080 render target, not a user-facing layout, so the seven-width bar does not
    apply. **Record that reasoning in the PR body for the owner.** Screenshot once at 1920×1080 over a
    checkerboard background, to prove transparency. ~~once~~ **(amended 2026-10-15)** once per scene
    (live, starting, reconnecting, final), for cricket (bar) and badminton (bug).
  - **(Amendment 2026-10-15)** e2e cases E1–E7 from the amendment section.
- [ ] **Step 4: Run, expect a FAIL** (vitest JSON reporter; the Playwright spec run alone from
  `apps/web` with `BASE=http://localhost:<port>`).
- [ ] **Step 5: Implement changes 1–4.**
- [ ] **Step 6: Run, expect a PASS.** Paste the counts.
- [ ] **Step 7: Mutate once each.**
  1. Skip the `session.fixture_id === fixtureId` check. Expected red: page test 6.
  2. Share the cache key. Expected red: page test 7.
  3. Emit ready on mount. The e2e "exactly once, after data" assertion must go red. Make it assert
     ready comes **after** the first snapshot's DOM is present.
  4–9. **(Amendment 2026-10-15)** mutants M4–M9 from the amendment section.
- [ ] **Step 8: Run the existing overlay e2e specs** (grep by route `/overlay/fixtures`, not by
  filename). All green and unchanged. **Commit and open the PR:**
  `feat(overlay): compositor access by relay page token, readiness and score-stale signals`

## Acceptance

- Unit, page and e2e tests pass.
- Existing overlay e2e is unchanged and green.
- ~~3 mutants~~ 9 mutants went red (amended 2026-10-15).
- Cache isolation is proven, for the page read **and** for the overlay route's `programmePhase`
  (amended 2026-10-15).
- The false premises (transparent body, timestamps) are recorded.
- (Amended 2026-10-15) The R2007 gallery shows all four scenes with **no gallery edit**: its sweeps
  enumerate `OVERLAY_SCENES`, so their printed counts grow by ×4 and stay green. Per-screen owner
  verdicts for every scene × template.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.1–4.3, §4.8 and §5. Spec §6: "R2005 (amend): adds the scene layer, the Reconnecting and
Stream-ended card variants (4 locales), the loopback client with fallback B. Depends on R2007."

### Scene layer (change 5)

`host=compositor` is the only way the scene layer mounts. OBS users have their own scenes.

`sceneFor(phase, slate)` in `overlay-scene.tsx` is pure. `phase` is the channel's
`ProgrammePhaseWire | null` (`null` when `host` is absent), and `slate` is `slateStateOf(model)`
(`overlay-slate.tsx:18`). It returns `{ scene: SceneId | null, card: "upcoming" | "result" | "reconnecting" | "ended" | null }`.
The table comes from spec §4.2 (each row's reason in brackets):

| phase \ slate | `warming` | `live` | `ended` | Reason |
|---|---|---|---|---|
| `null` (no host) | no scene, `upcoming` | no scene, no card | no scene, `result` | today's card layer, unchanged |
| `waiting` | `starting`, `upcoming` | `starting`, no card | `starting`, `result` | "the matching existing card, so the card layer's rules still apply" |
| `live` | no scene, `upcoming` | no scene, no card | no scene, `result` | §4.2 last row: the card layer as today |
| `frozen` | no scene, `upcoming` | no scene, no card | no scene, `result` | "a short glitch never flashes a card" |
| `reconnecting` | `reconnecting`, `reconnecting` | `reconnecting`, `reconnecting` | `reconnecting`, `result` | §5: "reconnecting while the match is decided (Result card + backdrop)" |
| `final` | `final`, `ended` | `final`, `ended` | `final`, `result` | "the Result card if decided or void; otherwise a Stream ended card with the current score" |

**Spec ambiguities resolved here** (record them in the PR body and the compositor `_INDEX.md`):
- **Starting soon while the match is already live.** No card exists for `live`, so the scene is the
  night backdrop with the score bug on it and no card. The owner judges it in the gallery.
- **z-order.** §4.1's diagram puts the scene layer (backdrop + card) on top, but §4.1 also says the
  score bug "stays visible over every scene" and is "never covered by a scene card". Both hold only if
  the **backdrop sits below the template** and the **scene card sits above the backdrop, outside the
  template's `sceneCardSafeArea`** (R2007). The slab sits above the scene card (§4.8).
- **One card at a time.** When a scene is active, the scene picks the card and the card layer renders
  nothing, so two cards can never stack.

**Card variants** (`overlay-slate.tsx`, the existing `OverlayMatchCard` component and styles,
spec §4.1 and D8):
- `reconnecting`: entrant names, the line below, and a pulsing status dot. The dot holds still
  under `prefers-reduced-motion` (the stage's `usePrefersReducedMotion` is the precedent).
- `ended`: the title below and the current score.

Every new text is ≥ 30 px. R2007's sweeps enforce that without an edit here.

Copy, all 4 locales, translated properly in each dictionary's tone, then `pnpm i18n:gen-keys`:

| Dictionary | Key | English |
|---|---|---|
| `public.json` | `overlay.scene.reconnecting.line` | "Camera reconnecting, back shortly" (spec §4.2, verbatim) |
| `public.json` | `overlay.scene.ended.title` | "Stream ended" |
| `ui.json` | `stream.scene.starting` | "Starting soon" |
| `ui.json` | `stream.scene.reconnecting` | "Reconnecting" |
| `ui.json` | `stream.scene.final` | "Final" |

Append `starting`, `reconnecting` and `final` to `OVERLAY_SCENES` (R2007's file) with those label keys.
The stage gains `sceneOverride?: SceneId`, preview only: `OverlayPreview` already passes `scene`, and
from this task on the gallery and R2006's picker show each scene. `live` as an override means "no
scene", whatever the phase.

### Phase channel (change 6)

- `lib/overlay-compositor.ts` gains `PHASE_LOOPBACK_FAILURES_TO_FALLBACK = 3` (spec §4.3), with a doc
  line, and a pure reducer `phaseChannelStep(state, event)`:
  - state `{ channel: "loopback" | "web", failures: number, phase: ProgrammePhaseWire | null, seq: number }`
  - events `loopbackOk({ phase, seq })`, `loopbackFailed`, `webPhase(phase | null)`
  - rules:
    - a success resets `failures` to 0
    - `PHASE_LOOPBACK_FAILURES_TO_FALLBACK` **consecutive** failures switch to `web`
    - **`web` is sticky for the page's life.** A CEF restart reloads the page and tries loopback again,
      so there is no in-page flapping. (Spec ambiguity: §4.3 says when to switch, not whether to come
      back. Resolved here; record it.)
    - a reply whose `seq` is ≤ the held `seq` is ignored (a guard, with a test)
    - in `loopback`, `webPhase` events are ignored, and vice versa
- `use-programme-phase.ts`: `useProgrammePhase({ enabled, webPhase })`. When `enabled` is false it
  returns `null` and **makes no request**. In loopback it long-polls
  `http://127.0.0.1:${RELAY_PHASE_PORT}${RELAY_PHASE_PATH}?after=${seq}` (`lib/relay-phase.ts`, R2007
  PR B). An `AbortController` counts any reply later than `RELAY_PHASE_LONG_POLL_MAX_MS + 2_000` as a
  failure. Unless C11 Step 0 recorded outcome (c), "blocked", **in which case the hook starts in `web`**
  and never fetches loopback; the `_FINDINGS.md` F-id is cited at the constant.
- **Channel signal:** `console.info("seazn:phase-channel=loopback" | "seazn:phase-channel=web")` through
  F-001's console mechanism, once on the first settled channel and again on each change. C11 relays it,
  C06 stores it, and C07 puts `phaseChannel` on the beat, so the heartbeat records it (spec §4.3).
- **Fallback B data.** The overlay route (`app/api/v1/public/fixtures/[id]/overlay/route.ts`) calls the
  same relay-access check as change 1 (`sid` + `st` + composed + not ended + same fixture). On success
  it reads the fixture through the bypass read, adds `programmePhase` (from
  `fixture_stream_sessions.last_heartbeat->>'programmePhase'`; an absent key stays absent, never
  defaulted) and sends `Cache-Control: private, no-store` in place of `LIVE_CACHE_CONTROL` (`:14`).
  Any failure falls through to today's public path **exactly**: no error, no oracle. The stage's
  fetcher passes `sid`/`st` on its poll only when the page has them (re-pin `fetchOverlayFixture`,
  `live-score-data.ts:122`).
- The page renders the phase it is given. **It debounces nothing** (§4.3): C03 already applied the
  thresholds.

### Moments during scenes (change 7)

- `moment-queue.ts` gains a `discard` action: the moment is marked seen and is never enqueued.
- While the scene is `starting` or `final`, every new moment is discarded, so it can never replay when
  the scene ends. In `reconnecting`, moments fire as usual and the slab sits above the scene card
  (§4.8: "the slab is the only way a viewer learns a goal happened").
- Timing: at `host=compositor` the slab follows the same `delay` presentation as today.

### Tests

**Unit** (failing first, JSON reporter):
- **A1** `sceneFor`: all 18 cells of the table above, typed from the spec. Print `cells checked: 18`;
  fail on 0. A `null` phase never yields a scene for any slate.
- **A2** `phaseChannelStep`:
  - fail, fail, ok → still `loopback`, `failures` 0
  - fail × 3 → `web`
  - after `web`: `loopbackOk` ignored, `webPhase("reconnecting")` adopted
  - a stale `seq` ignored
  - `PHASE_LOOPBACK_FAILURES_TO_FALLBACK` equals spec §4.3's 3, typed in the test
- **A3** `moment-queue.test.ts`, through the real reducer and the stage's scene rule:
  - a goal while `starting` is discarded; after the scene ends it never shows, and a **new** goal does
  - a goal while `reconnecting` is enqueued and shows
  - a goal while `final` is discarded

**Page / route:**
- **A4**, through the real overlay route:
  - a verified composed `sid`/`st` returns `programmePhase` from the seeded `last_heartbeat`, with
    `Cache-Control: private, no-store`
  - immediately after, the plain public request for the same fixture has **no** `programmePhase` key
    and has `LIVE_CACHE_CONTROL`
  - a wrong `st` returns the public answer
  - a beat without `programmePhase` leaves the key absent

**E2E** (`overlay-compositor.spec.ts`, local prod build, `RELAY_DRIVERS=fake`; the phase is served by
`page.route("http://127.0.0.1:7419/phase**", …)` from a scripted list). Every case asserts the score
bug is **visible and on top**: `document.elementFromPoint` at the template's centre returns a node
inside the template (AGENTS.md class 2), and the scene card's box doesn't intersect the template's box.
- **E1 Sequence** (spec §5): live → frozen → reconnecting → live → final. Per step, the expected
  `data-scene` and card variant come from the A1 table. No scene shows at `frozen`.
- **E2 Second camera loss:** live → reconnecting → live → reconnecting → live. The card appears and
  clears twice.
- **E3 Reconnecting while decided:** decide the fixture through the real API, then serve
  `reconnecting`: the Result card on the backdrop.
- **E4 Another sport:** E1 for cricket (`template=bar`) and for badminton (`template=bug`).
- **E5 OBS:** the same URL without `host=compositor`, with the route ready to serve `reconnecting`. No
  `[data-testid="ovl-scene"]` ever appears, and the route handler is called **0** times.
- **E6 Fallback B:**
  - the route aborts (`connectionrefused`)
  - after exactly 3 calls the console shows `seazn:phase-channel=web`, and the handler count stays 3
    for the next 15 s (sticky)
  - POST a real heartbeat with `programmePhase: "reconnecting"` through the internal route with the
    seeded session's job token: the Reconnecting scene appears within `STREAM_POLL_MS + 3 000`
  - a run where the route succeeds logs `seazn:phase-channel=loopback` exactly once
- **E7 Moments:** with `delay=3000`, serve `reconnecting` and score a goal through the API at T0. The
  slab is visible no sooner than T0 + 3 000 (the URL's `delay`; the spec's guard) and no later than
  T0 + 3 000 + `STREAM_POLL_MS` + 2 000 (so the test can't pass by never showing it), and it is on top
  of the scene card (`elementFromPoint`). Serve `waiting`, score a goal, switch to `live`: the slab
  never appears.

**Mutants** (spec §5's list, plus this task's guards). Restore each from a `cp` backup.

| # | Mutant | Expected red |
|---|---|---|
| M4 | the scene layer mounts without `host=compositor` | A1's `null` rows, E5 |
| M5 | fallback never engages (failures never switch to `web`) | A2, E6 |
| M6 | moments suppressed during `reconnecting` (discard in every scene) | A3, E7 |
| M7 | no discard in `starting` | A3, E7's last line |
| M8 | the overlay route adds `programmePhase` without the relay check | A4 |
| M9 | `web` not sticky (a later `loopbackOk` switches back) | A2, E6's handler count |

### Not this task

The Machine side of the channel (C08 publishes, C12 serves, C11 relays the signal) and the `final`
phase itself (C06) are compositor tasks. The organiser's picker is R2006.

## Do not touch

The organiser UI (R2006), and the relay usecases (R2004). (Amended 2026-10-15) The template registry's
resolution rules and the gallery's sweeps (R2007): extend `OVERLAY_SCENES`; don't fork the sweeps.

## Output cap

Under 15 lines.
