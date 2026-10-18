# R2005: Overlay page for the compositor (`sid`+`st` access, `host=compositor` signals, stale, clamp)

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/r2005-overlay-host -b feat/overlay-compositor-host origin/main`.
**Depends on:** R2004 merged (sequential with R2003/R2004), and **C00 F-001** (the readiness mechanism the
CEF adapter observes). **Amended 2026-10-15:** also **R2007 PR A merged** (template registry,
`OVERLAY_SCENES`, `OverlayPreview`, the gallery and its sweeps), **R2007 PR B merged** (`final` and
`phaseChannel` on the heartbeat; `lib/relay-phase.ts`), and **C11 Step 0's result in `_FINDINGS.md`**
(whether CEF 139 lets the page reach loopback; it decides which channel is primary). See "Amendment
2026-10-15 (overlay templates spec)" at the end. **Amended 2026-10-16:** Step 0 recorded **F-009,
outcome (a)**, so the hook starts in `loopback`. Its review adds changes 8 and 9 (the page's CSP and a
CORS-simple request); see "Amendment 2026-10-16 (C11 Step 0, F-009)". **Use the `frontend-design` skill** (house rule for every UI task), even
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
     is not ended, and `mode === "composed"`. **(Defined 2026-10-15, review I5)** "Not ended" means
     `!isTerminal(session.state)` (`server/relay/domain/session.ts:20`; `TERMINAL_STATES` =
     `completed`, `failed` at `:19`). So `ending`, the state web holds during the compositor's final
     hold, counts as active: a CEF reload during the hold must load, and fallback B must still be able
     to deliver `final`.
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
8. **CSP `connect-src` allows the loopback endpoint on `/overlay` only** (amendment 2026-10-16; F-009,
   coordinator decision). See the amendment.
9. **The phase request stays CORS-simple** (amendment 2026-10-16; F-009 review M5). See the amendment.

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
  - Modify: `components/overlay/overlay-scenes.ts` (append three scenes, each with its `momentsFire`:
    `starting` true, `reconnecting` true, `final` false; R2007 decision 8), `overlay-slate.tsx` (two card
    variants), `overlay-stage.tsx` (`host`, `sceneOverride`, moment discard),
    `components/overlay/overlay-preview.tsx` (R2007's render function: pass `sceneOverride={scene}`;
    review I6), `moment-queue.ts` (a
    `discard` action), `app/api/v1/public/fixtures/[id]/overlay/route.ts` and
    `components/public-site/live-score-data.ts` `fetchOverlayFixture` (fallback B), `app/globals.css`
  - Modify: `apps/web/src/dictionaries/{en,es,fr,nl}/public.json` and `ui.json`, then
    `pnpm i18n:gen-keys`
  - Test: `components/overlay/__tests__/overlay-scene.test.ts`,
    `components/overlay/__tests__/moment-queue.test.ts` (extend),
    `app/api/v1/public/fixtures/[id]/overlay/__tests__/route.test.ts` (extend), and the e2e above
- **Amendment 2026-10-16 (F-009):**
  - Modify: `apps/web/src/proxy.ts` (change 8) and `components/overlay/use-programme-phase.ts`
    (`phaseRequest`, change 9)
  - Test: `src/__tests__/proxy-csp.test.ts` (extend, A5), the A6 test, and E8 in the e2e above

## Steps

- [ ] **Step 1: Write the failing unit tests** (`overlay-compositor.test.ts`, pure):
  - stale flips at exactly `SCORE_STALE_AFTER_MS` after the last success, not before
  - stale clears on the next success
  - the signal is emitted on change only (a sequence of 5 identical states ⇒ 1 emit)
  - the clamp: an event 2 001 ms past `now + delay` is clamped; 1 999 ms is not
  - the clamp is not applied when the delay is 0 and the event is in the past
  - **(amendment 2026-10-15)** tests A1–A3 from the amendment section
  - **(amendment 2026-10-16)** tests A5 (the `/overlay`-only `connect-src`, through `proxy()`) and A6
    (`phaseRequest` stays CORS-simple)
- [ ] **Step 2: Write the failing page tests.** Extend the existing `app/overlay/__tests__`; re-pin its
  render harness.
  1. A private fixture plus a valid `sid`/`st` for a composed session on it: 200.
  2. The same without `st`: 404.
  3. `st` for another session's sid: 404.
  4. `st` with scope `relay-job`: 404.
  5. An ended session (`completed`, and `failed`): 404.
  5b. **(Added 2026-10-15, review I5)** An `ending` session (the final hold): 200, so a CEF reload
     during the hold loads.
  6. A session on a different fixture: 404.
  7. **Cache isolation:** a bypass read of a private fixture, then the public read of it, is still
     404.
  8. Public fixture, no params: byte-identical to before. Assert on `="` anchors, not bare `data-*`
     (AGENTS.md trap).
  9. **(Amendment 2026-10-15)** test A4 from the amendment section (the overlay route's fallback-B
     field and its cache isolation).
  10. **(Review I6)** The page never sets `sceneOverride`. With `host=compositor` and without it, the
     stage element `page.tsx` renders has no `sceneOverride` prop: inspect the returned element tree,
     because the markup alone can't show an absent prop.
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
  - **(Amendment 2026-10-16)** E8: E1 once against a prod build started with `CSP_MODE=enforce`.
- [ ] **Step 4: Run, expect a FAIL** (vitest JSON reporter; the Playwright spec run alone from
  `apps/web` with `BASE=http://localhost:<port>`).
- [ ] **Step 5: Implement changes 1–4, then changes 5–7** (the amendment: scene layer, phase channel,
  moments), then changes 8–9 (amendment 2026-10-16: the CSP and the CORS-simple request). Run each change's unit tests (A1–A3) before moving to the next, then the page tests (A4,
  5b) and the e2e.
- [ ] **Step 6: Run, expect a PASS.** Paste the counts.
- [ ] **Step 7: Mutate once each.**
  1. Skip the `session.fixture_id === fixtureId` check. Expected red: page test 6.
  2. Share the cache key. Expected red: page test 7.
  3. Emit ready on mount. The e2e "exactly once, after data" assertion must go red. Make it assert
     ready comes **after** the first snapshot's DOM is present.
  4–12. **(Amendment 2026-10-15)** mutants M4–M12 from the amendment section.
  13–15. **(Amendment 2026-10-16)** mutants M13–M15 from that amendment.
- [ ] **Step 8: Run the existing overlay e2e specs** (grep by route `/overlay/fixtures`, not by
  filename). All green and unchanged. **Commit and open the PR:**
  `feat(overlay): compositor access by relay page token, readiness and score-stale signals`

## Acceptance

- Unit, page and e2e tests pass.
- Existing overlay e2e is unchanged and green.
- ~~3 mutants~~ ~~9~~ ~~11~~ ~~12~~ 15 mutants went red (amended 2026-10-15; M10 and M11 from the review
  fixes; M7 rewritten and M12 added for owner ruling D14, 2026-10-16; M13–M15 added 2026-10-16 for F-009).
- Cache isolation is proven, for the page read **and** for the overlay route's `programmePhase`
  (amended 2026-10-15).
- (Amended 2026-10-16, F-009) Under `CSP_MODE=enforce`, the `/overlay` policy's `connect-src` carries
  `http://127.0.0.1:7419`, and every other route's policy doesn't (A5, with its printed route count).
  E8 passed, or its absence is recorded with the reason and G1 named as the first witness. The
  request is CORS-simple (A6). The PR body records the stg `overlayUrl` origin.
- The false premises (transparent body, timestamps) are recorded.
- (Amended 2026-10-15) The R2007 gallery shows all four scenes with **no gallery edit**: its sweeps
  enumerate `OVERLAY_SCENES`, so their printed counts grow by ×4 and stay green. Per-screen owner
  verdicts for every scene × template.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.1–4.3, §4.8 and §5. Spec §6, R2005 row (verbatim, `29fa9f59c`): "Adds the scene layer (§4.1–4.2),
the Reconnecting and Stream-ended card variants (4 locales), the loopback client with fallback B
(§4.3). Depends on R2007."

### Scene layer (change 5)

**(Corrected 2026-10-15, review I6)** The scene layer mounts in exactly two cases:
- the page with `host=compositor`, where the phase comes from the channel (change 6)
- the preview render function (`OverlayPreview`, R2007) via the stage's `sceneOverride` prop, where
  the scene is the one the gallery cell or picker toggle names

The overlay **page** never sets `sceneOverride` (M4 checks it), so an OBS URL shows no scene; OBS
users have their own scenes.

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
| `final` | `final`, `ended` | `final`, `ended` | `final`, `result` | §4.2: "The existing **Result** card if the match is decided or void. Otherwise a **Stream ended** card variant with the current score." |

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
The stage gains `sceneOverride?: SceneId`, preview only. R2007's `OverlayPreview` takes a `scene` prop
and writes `data-scene`, but doesn't pass anything to the stage; this task makes it pass
`sceneOverride={scene}` (Files). From then on the gallery and R2006's picker show each scene. `live` as
an override means "no scene", whatever the phase.

**The scene card renders inside the card layer's `.ovl-slate-content` wrapper, as an
`[data-testid="ovl-slate-card"]` element** (the existing `OverlayMatchCard`). So R2007's card layout
(cards placed from the template's `sceneCardSafeArea` through `--ovl-card-*`, R2007 step 9b) and its
gallery check 4 (keyed on that element's presence) cover every scene card with **no gallery edit**.
Only the backdrop is a new element (`[data-testid="ovl-scene"]`, below the template).

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
    - **(review m12)** a reply whose `phase` is not in `PROGRAMME_PHASES` (`lib/relay-phase.ts`; the
      client page can't import `schemas.ts`), whose `seq` is not a non-negative integer, or that
      carries any other key, counts as a **`loopbackFailed`**, never as a phase
    - in `loopback`, `webPhase` events are ignored, and vice versa
- `use-programme-phase.ts`: `useProgrammePhase({ enabled, webPhase })`. When `enabled` is false it
  returns `null` and **makes no request**. In loopback it long-polls
  `http://127.0.0.1:${RELAY_PHASE_PORT}${RELAY_PHASE_PATH}?after=${seq}` (`lib/relay-phase.ts`, R2007
  PR B). An `AbortController` counts any reply later than `RELAY_PHASE_LONG_POLL_MAX_MS + 2_000` as a
  failure. The reply is validated as above before it reaches the reducer. Unless C11 Step 0 recorded
  outcome (c), "blocked", **in which case the hook starts in `web`**
  and never fetches loopback; the `_FINDINGS.md` F-id is cited at the constant. **(2026-10-16) Step 0
  recorded F-009, outcome (a): the hook starts in `loopback`. Cite F-009. The request stays CORS-simple
  (change 9), and `connect-src` must allow it on `/overlay` (change 8).**
- **Channel signal:** `console.info("seazn:phase-channel=loopback" | "seazn:phase-channel=web")` through
  F-001's console mechanism, once on the first settled channel and again on each change. C11 relays it,
  C06 stores it, and C07 puts `phaseChannel` on the beat, so the heartbeat records it (spec §4.3).
- **Fallback B data.** The overlay route (`app/api/v1/public/fixtures/[id]/overlay/route.ts`) calls the
  same relay-access check as change 1 (`sid` + `st` + composed + not ended, which is
  `!isTerminal(state)` so `ending` passes, + same fixture). On success
  it reads the fixture through the bypass read, adds `programmePhase` (from
  `fixture_stream_sessions.last_heartbeat->>'programmePhase'`; an absent key stays absent, never
  defaulted) and sends `Cache-Control: private, no-store` in place of `LIVE_CACHE_CONTROL` (`:14`).
  Any failure falls through to today's public path **exactly**: no error, no oracle. The stage's
  fetcher passes `sid`/`st` on its poll only when the page has them (re-pin `fetchOverlayFixture`,
  `live-score-data.ts:122`).
- The page renders the phase it is given. **It debounces nothing** (§4.3): C03 already applied the
  thresholds.

### Moments during scenes (change 7)

**Owner ruling D14 (2026-10-16; spec §4.8 at `seazn.club` `9bf31b38a`, PR #969):** "**Starting soon:**
moments **fire** (owner ruling 2026-10-16). A stream can start after the match has begun, and a goal
scored before the first camera frame must still reach viewers, the same reasoning as Reconnecting.
**Final:** none fire; the deciding moment has already shown, and the Result card carries the outcome."

- `moment-queue.ts` gains a `discard` action: the moment is marked seen and is never enqueued.
- The stage reads the active scene's `momentsFire` (R2007 decision 8; the declaration in
  `overlay-scenes.ts` is the one authority) and never compares scene ids itself:
  - `starting` (`momentsFire: true`): moments fire as usual. With the match in play (slate `live`)
    there's no card, so the slab sits on the night backdrop.
  - `reconnecting` (`true`): moments fire, and the slab sits above the scene card (§4.8: "the slab is
    the only way a viewer learns a goal happened").
  - `final` (`false`): every new moment is discarded, so it can never replay. Final is terminal, so no
    later scene could show it anyway.
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
  - **(review m12)** the hook's reply validator, over 4 replies:
    - `{ phase: "paused", seq: 1 }` → `loopbackFailed`
    - `{ phase: "live", seq: -1 }` → `loopbackFailed`
    - `{ phase: "live", seq: 1, x: 1 }` → `loopbackFailed`
    - `{ phase: "final", seq: 1 }` → `loopbackOk`

    Every `PROGRAMME_PHASES` value is accepted: print `phases checked: 5`; fail on 0.
- **A3** `moment-queue.test.ts`, through the real reducer and the stage's scene rule (amended
  2026-10-16, D14):
  - a goal while `starting` with the match in play (slate `live`) is **enqueued and shows**
  - a goal while `reconnecting` is enqueued and shows
  - a goal while `final` is discarded, and stays unseen through the rest of the hold
  - every `OVERLAY_SCENES` entry's `momentsFire` equals the spec's rule, typed in the test from §4.8
    and D14: `live` true, `starting` true, `reconnecting` true, `final` false. Print
    `scenes checked: 4`; fail on 0. The values come from the spec, never read back from the
    declaration.

**Page / route:**
- **A4**, through the real overlay route:
  - a verified composed `sid`/`st` returns `programmePhase` from the seeded `last_heartbeat`, with
    `Cache-Control: private, no-store`
  - immediately after, the plain public request for the same fixture has **no** `programmePhase` key
    and has `LIVE_CACHE_CONTROL`
  - a wrong `st` returns the public answer
  - a beat without `programmePhase` leaves the key absent
  - **(review I5)** an `ending` session whose `last_heartbeat` carries `programmePhase: "final"` →
    the route serves `programmePhase: "final"`. A `completed` session → the public answer.

**E2E** (`overlay-compositor.spec.ts`, local prod build, `RELAY_DRIVERS=fake`; the phase is served by
`page.route("http://127.0.0.1:7419/phase**", …)` from a scripted list). **(Review m13)** Every fulfilled
response carries `Access-Control-Allow-Origin: <the page's origin>`, and an `OPTIONS` is answered 204
with that header plus `Access-Control-Allow-Private-Network: true`, because the page at `localhost:<port>`
calling `127.0.0.1:7419` is cross-origin. These tests can't witness Private Network Access; only C11
Step 0 and C17 can. Every case asserts the score
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
  - **(review I5) The hold under fallback B:**
    - stop the session through the organiser's real stop API, so it goes `ending`
    - POST a beat with `programmePhase: "final"` with the same job token: the Final scene appears
      within `STREAM_POLL_MS + 3 000`
    - **reload the page**: it loads (200), and shows the Final scene again
- **E7 Moments:** with `delay=3000`, serve `reconnecting` and score a goal through the API at T0. The
  slab is visible no sooner than T0 + 3 000 (the URL's `delay`; the spec's guard) and no later than
  T0 + 3 000 + `STREAM_POLL_MS` + 2 000 (so the test can't pass by never showing it), and it is on top
  of the scene card (`elementFromPoint`).
  - **(Amended 2026-10-16, D14)** On a fixture already in play, serve `waiting` and score a goal at
    T1. The slab appears inside the same `[T1 + 3 000, T1 + 3 000 + STREAM_POLL_MS + 2 000]` window,
    over the Starting soon backdrop. Starting soon shows the moment.
  - Serve `final` and score a goal at T2. Over `FINAL_HOLD_MS`, the slab never appears (poll every
    250 ms and assert none is attached). Final drops the moment.

**Mutants** (spec §5's list, plus this task's guards). Restore each from a `cp` backup.

| # | Mutant | Expected red |
|---|---|---|
| M4 | the scene layer mounts without `host=compositor` (and without a `sceneOverride`); also: the page passes `sceneOverride` | A1's `null` rows, E5; a page test that asserts the stage element rendered by `page.tsx` has no `sceneOverride` prop |
| M5 | fallback never engages (failures never switch to `web`) | A2, E6 |
| M6 | moments suppressed during `reconnecting` (discard in every scene) | A3, E7 |
| M7 | (rewritten 2026-10-16, D14) moments suppressed during Starting soon (`starting` discards, or declared `momentsFire: false`) | A3's `starting` row and its declaration row, E7's Starting-soon leg |
| M8 | the overlay route adds `programmePhase` without the relay check | A4 |
| M9 | `web` not sticky (a later `loopbackOk` switches back) | A2, E6's handler count |
| M10 | "not ended" read as `state === "live"` (refuses `ending`) | page test 5b, A4's `ending` row, E6's hold leg |
| M11 | the reply validator dropped (any JSON is a phase) | A2's validator rows |
| M12 | (added 2026-10-16) no discard in `final` (moments fire in every scene) | A3's `final` row and its declaration row, E7's Final leg |

### Not this task

The Machine side of the channel (C08 publishes, C12 serves, C11 relays the signal) and the `final`
phase itself (C06) are compositor tasks. The organiser's picker is R2006.

## Amendment 2026-10-16 (C11 Step 0, F-009)

Authority: compositor `_FINDINGS.md` F-009 (CEF 139, outcome (a)), the review of compositor PR #20, and the
coordinator's decision on that review's I1. F-009 proved that loopback works for a page **with no CSP**, sending
a **CORS-simple GET**. The real overlay page differs in both ways, and that is the reason for this amendment.

### Change 8: the CSP allows the endpoint on `/overlay` only

- **The gap, re-pin it first** (`seazn.club` `c64e0a217`):
  - `apps/web/src/proxy.ts` `cspHeader` (`:45`) builds one policy for every page request. Its `connect-src`
    (`:58`) has no `http://127.0.0.1:7419`.
  - `/overlay` is outside the `CACHEABLE_PUBLIC` carve-out (`:14`), so `CSP_MODE=enforce` would enforce
    the policy there. API paths get no CSP at all (`:150-151`).
  - On 2026-10-16, `seazn.club`, `stg.seazn.club` and `seazn-club-stg.fly.dev` all sent it as
    `Content-Security-Policy-Report-Only`. `design/v1/DEFERRED.md:36-40` still plans the flip to enforce.
- **Report-only today:** every poll should log a `[Report Only] Refused to connect` console error.
- **Once enforced:** every loopback fetch would be blocked before it reaches the network. The hook would drop
  to fallback B after 3 failures, sticky, with no error anywhere (AGENTS.md class 6).
- **Coordinator decision (2026-10-16): this task owns it.**
  - Add `http://127.0.0.1:${RELAY_PHASE_PORT}` (`lib/relay-phase.ts`; derived, never a literal) to
    `connect-src` for the `/overlay` tree only (`/overlay` or `/overlay/…`), never site-wide.
  - The OBS page carries the source too; it never fetches it, because the hook makes no request without
    `host=compositor`.
  - Thread the pathname from the call site (`proxy.ts:155-157`, re-pin) into `cspHeader`.
  - The other directives are unchanged, and every other route's policy is byte-identical.
- **Verify `upgrade-insecure-requests` under enforcement** (`:65`; review I1, unverified). An enforced policy
  might rewrite `http://127.0.0.1:7419` to `https://127.0.0.1:7419`, which would fail.
  - **E8:** run E1 once against a prod build started with `CSP_MODE=enforce`. The `page.route` handler must
    be called at least once. That proves the policy allowed the URL and nothing rewrote it, because a
    CSP-blocked or rewritten request never reaches `http://127.0.0.1:7419/phase**`. The console must also
    show no `Refused to connect` line.
  - If the harness can't start a second server, record that, and the G1 compatibility step on stg becomes
    the first witness (compositor Owed 21).

### Change 9: the phase request stays CORS-simple (review M5)

- **The rule:** the hook's fetch is `fetch(url, { signal })` and nothing else. No `headers`, no `body`, no
  `method` other than the default GET, no `credentials`, no `mode`.
- **Why:**
  - F-009 proved only that shape.
  - A non-safelisted request header (a `Content-Type`, say) makes the browser send an **enforced** CORS
    preflight. C12 answers with no `Access-Control-Allow-Headers` (C12 brief, amended 2026-10-16), so the
    preflight fails, and so does every poll.
- **The build:** factor the request into a pure `phaseRequest(seq, signal)` that returns `[url, init]`. The hook
  calls exactly that.

### The page's origin (review M6; nothing to build here)

stg serves `/overlay` on two hosts. The compositor's exact-origin check passes only for the host the composed
`overlayUrl` names (C12 brief, amended 2026-10-16). Record in the PR body the origin that `sessionFactsForJob`'s
`overlayUrl` carries on stg, so C12 can pin it.

### Tests

- **A5** (`src/__tests__/proxy-csp.test.ts`, extend). Go through `proxy(new NextRequest(…))`, the real call site,
  not `cspHeader` alone, so the pathname wiring is what gets proven. Read the `connect-src` directive from
  whichever CSP header is present.
  - **These carry `http://127.0.0.1:7419`,** typed from spec §4.3 (port 7419), never read from
    `RELAY_PHASE_PORT`. Check each in report-only mode and under `CSP_MODE=enforce` (as
    `Content-Security-Policy`):
    - `/overlay/fixtures/<uuid>`
    - `/overlay/fixtures/<uuid>?host=compositor`
  - **These don't:** `/`, `/dashboard`, `/shared/<x>`, `/embed/<x>`, `/score/<token>`, and the prefix guard
    `/overlayx/fixtures/<uuid>`.
  - Print `routes checked: N`. Fail on 0, and on any route class missing.
  - Every other directive on `/overlay` equals the same directive on `/dashboard` (split on `; `, compared one by
    one, with each `'nonce-…'` value replaced first, because the nonce is per request).
- **A6** (`lib/__tests__/overlay-compositor.test.ts` or the hook's own test):
  - `Object.keys(phaseRequest(7, signal)[1])` is exactly `["signal"]`.
  - The URL is `http://127.0.0.1:7419/phase?after=7`.
- **E8** as described above.

**Mutants** (restore each from a `cp` backup):

| # | Mutant | Expected red |
|---|---|---|
| M13 | `connect-src` loses the `/overlay` addition | A5's `/overlay` rows (both modes), E8 |
| M14 | the addition goes site-wide (every path) | A5's other-route rows |
| M15 | `phaseRequest` adds `headers: { "Content-Type": "application/json" }` | A6 |

### Files (amendment 2026-10-16)

- Modify: `apps/web/src/proxy.ts`
- Modify: the hook file `components/overlay/use-programme-phase.ts` (`phaseRequest`)
- Test: `src/__tests__/proxy-csp.test.ts`, the A6 test, and E8 in `overlay-compositor.spec.ts`

## Amendment 2026-10-18 (hang detection; owner ruling, compositor `docs/_INDEX.md` Owed 18)

Owner, 2026-10-18, in chat, relayed by the coordinator: "2 Ok", approving hang detection (compositor Owed 18, closed). The compositor cannot see a JS hang once
the page is ready: cefsrc emits a buffer only when the page paints, so frame silence is not a signal
(F-001). So the page beats.

### Change 10: the alive beat

- **With `host=compositor` only**, after the ready signal (change 3's `console.info("seazn:ready")`),
  the page runs `setInterval(() => console.info("seazn:alive"), 2_000)` on F-001's console channel.
- It **never** uses `gstSendMsg`, which accepts only `ready`/`eos`, and whose js-signal mode hangs
  the compositor's `start()` and `set_state(NULL)` (F-001).
- The interval is `OVERLAY_ALIVE_EVERY_MS = 2_000`, and its comment cites the compositor's
  `cef_alive_every_ms`. The compositor counts `cef_hang_after_ms` (10 000, five beats, equal to
  `SCORE_STALE_AFTER_MS`) of silence as a hang, then restarts the page.
- The beat runs on the main thread on purpose: a hung main thread must stop it. It is never a worker
  or a `requestAnimationFrame` loop that could keep beating while the score logic hangs.
- The line is the bare `seazn:alive`. The design's optional `stale=0|1` suffix is **not** built: the
  `ScoreStale` signal stays change 4's own edge-triggered line.
- Without `host=compositor`, nothing is logged (OBS and the public page are unchanged).
- Started once, after ready. The interval is cleared on unmount, so a remount never doubles the beat.

### Tests

- **Unit (fake timers):**
  - with `host=compositor`, there is no `seazn:alive` before ready.
  - After ready, there are exactly `k` lines at `k × OVERLAY_ALIVE_EVERY_MS` and `k − 1` at 1 ms
    before. Check `k` = 1..5, and print the count.
  - Without `host`, there are 0 lines after 10 beats' worth of time.
  - Unmount, then remount: still one line per interval.
- **E9** in `overlay-compositor.spec.ts`: with `host=compositor`, the console shows ≥ 2
  `seazn:alive` lines within 5 s after `seazn:ready`. Without `host`, it shows 0 in the same window.

### Mutants

| # | Mutant | Expected red |
|---|---|---|
| M16 | the beat starts on mount, not after ready | the unit "no `seazn:alive` before ready" |
| M17 | the `host=compositor` gate is dropped | the unit "without `host`", E9's second half |
| M18 | `clearInterval` on unmount is dropped | the remount unit |

### Files (amendment 2026-10-18)

- Modify: the compositor-host hook beside change 3's ready signal (re-pin it; R2005 has not landed).
- Test: its unit file, and E9 in `overlay-compositor.spec.ts`.

## Do not touch

The organiser UI (R2006), and the relay usecases (R2004). (Amended 2026-10-15) The template registry's
resolution rules and the gallery's sweeps (R2007): extend `OVERLAY_SCENES`; don't fork the sweeps.

## Output cap

Under 15 lines.
