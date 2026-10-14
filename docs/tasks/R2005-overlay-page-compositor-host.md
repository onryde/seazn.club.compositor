# R2005: Overlay page for the compositor (`sid`+`st` access, `host=compositor` signals, stale, clamp)

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/w05-overlay-host -b feat/overlay-compositor-host origin/main`.
**Depends on:** R2004 merged (sequential with R2003/R2004), and **C00 F-001** (the readiness mechanism the
CEF adapter observes). **Use the `frontend-design` skill** (house rule for every UI task), even
though the page is transparent: the compositor output is what viewers see.

## Context

Pinned 2026-10-14 at `09290de3c`. **Re-pin everything.**

`app/overlay/fixtures/[fixtureId]/page.tsx` (172 lines) has:

| Lines | What |
|---|---|
| `:3-7` | two server-side `notFound()` gates: public visibility (`publicFixtureSlugs` + `getPublicFixture`) and the `streaming.overlay` entitlement |
| `:55` | the query is `{ style, lang, delay, <OVERLAY_KEY_PARAM> }` |
| `:70` | `resolveDelayMs(delay)`: unparseable or out of range ⇒ 0, never throws; max 300 000 ms (`lib/overlay-delay.ts`) |

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
     C14 case 11 then runs against the harness page only. Add a test that pins the server-assigned
     fact, so a future device-sourced timestamp reds it.
4. **Void re-derivation.** Re-pin that a void yields a snapshot the page re-derives from (no
   append-only display state). Add a test only if no existing one covers "void after delay window".

## Files

- Modify: `apps/web/src/app/overlay/fixtures/[fixtureId]/page.tsx`
- Create: `apps/web/src/lib/overlay-compositor.ts` (thresholds, the signal helper, the clamp)
- Modify: the live-fixture hook / `overlay-stage.tsx` (re-pin) to call the signal helper when
  `host=compositor`
- Possibly create: a bypass fixture read in `server/public-site/data.ts` or `server/overlay/` (re-pin)
- Test: `app/overlay/__tests__/` (existing dir), `lib/__tests__/overlay-compositor.test.ts`, and an
  e2e spec `apps/web/e2e/overlay-compositor.spec.ts`

## Steps

- [ ] **Step 1: Write the failing unit tests** (`overlay-compositor.test.ts`, pure):
  - stale flips at exactly `SCORE_STALE_AFTER_MS` after the last success, not before
  - stale clears on the next success
  - the signal is emitted on change only (a sequence of 5 identical states ⇒ 1 emit)
  - the clamp: an event 2 001 ms past `now + delay` is clamped; 1 999 ms is not
  - the clamp is not applied when the delay is 0 and the event is in the past
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
- [ ] **Step 3: Write the failing e2e** (`overlay-compositor.spec.ts`). Against a local prod build,
  seed a composed session with fakes (`RELAY_DRIVERS=fake`) and open the overlay URL from
  `sessionFactsForJob` (the **real producer**, not a hand-built URL).
  - Assert the F-001 ready signal is observed exactly once.
  - Stop the live-data route (route interception), and assert the stale signal `true` within
    `SCORE_STALE_AFTER_MS` + one poll, then `false` after restoring it.
  - Open the same URL without `host=compositor`, and assert no signal is ever emitted.
  - This page is a 1920×1080 render target, not a user-facing layout, so the seven-width bar does not
    apply. **Record that reasoning in the PR body for the owner.** Screenshot once at 1920×1080 over a
    checkerboard background, to prove transparency.
- [ ] **Step 4: Run, expect a FAIL** (vitest JSON reporter; the Playwright spec run alone from
  `apps/web` with `BASE=http://localhost:<port>`).
- [ ] **Step 5: Implement changes 1–4.**
- [ ] **Step 6: Run, expect a PASS.** Paste the counts.
- [ ] **Step 7: Mutate once each.**
  1. Skip the `session.fixture_id === fixtureId` check. Expected red: page test 6.
  2. Share the cache key. Expected red: page test 7.
  3. Emit ready on mount. The e2e "exactly once, after data" assertion must go red. Make it assert
     ready comes **after** the first snapshot's DOM is present.
- [ ] **Step 8: Run the existing overlay e2e specs** (grep by route `/overlay/fixtures`, not by
  filename). All green and unchanged. **Commit and open the PR:**
  `feat(overlay): compositor access by relay page token, readiness and score-stale signals`

## Acceptance

- Unit, page and e2e tests pass.
- Existing overlay e2e is unchanged and green.
- 3 mutants went red.
- Cache isolation is proven.
- The false premises (transparent body, timestamps) are recorded.

## Do not touch

The organiser UI (R2006), and the relay usecases (R2004).

## Output cap

Under 15 lines.
