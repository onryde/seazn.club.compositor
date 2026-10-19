# R2006: Organiser UI (composed-mode option and score-delay override)

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/r2006-composed-ui -b feat/stream-panel-composed origin/main`.
**Depends on:** R2004 and R2005 merged. **Amended 2026-10-15:** also R2007 PR A (`templatesForSport`,
`resolveTemplate`, `OverlayPreview`, `OVERLAY_SCENES`) and R2007 PR B (`PROGRAMME_PHASES` with `final`)
merged. See "Amendment 2026-10-15 (overlay templates spec)" at the end. **Amended 2026-10-19:** the
owner picked **Option C** (neither A nor B). It supersedes the product shape, the default selection
and several tests below; see "Amendment 2026-10-19 (owner ruling: Option C)" at the end, which wins
wherever the two disagree. **Use the `frontend-design` skill.** It's a house rule for
every UI task; put the invocation in the dispatch.

## Context

Pinned 2026-10-14 at `09290de3c`. **Re-pin everything.**

- `components/v2/fixture-stream-panel.tsx` ~~`:1140`~~ `:1205` (re-pinned 2026-10-15 at `b01d07cd5`)
  starts a session with a **hardcoded** `mode: "passthrough"`, body `{ mode, targetId }` and no
  `themeId` (testid `stream-go-live`, `:2347`).
- (Added 2026-10-15) The settings write route is `PUT /api/v1/fixtures/[id]/stream-settings`
  (`app/api/v1/fixtures/[id]/stream-settings/route.ts`), body schema `PutStreamSettings`
  (`server/api-v1/schemas.ts:1488`, `.strict()` plus a "`targetId` or `autoStream` is required"
  refine), answer `StreamSettings` (`:1494`, strict), usecase `saveStreamSettings`
  (`server/usecases/stream-codes.ts:281`). Each field has its own single writer (`writeStreamSettings`,
  `writeAutoStream`); follow that shape.
- `relayOffer` lives in `apps/web/src/server/stream-panel-context.ts:37` (server, not a component). It
  returns `{ entitled, relayEntitled, relayDisabled }`, where
  `relayDisabled = relayEntitled && relayUnavailable()` (`server/relay/drivers.ts:135`). This task adds
  `composedAvailable: relayEntitled && !composedUnavailable()` to that return. `composedUnavailable()`
  is R2001's predicate in `server/relay/drivers.ts`, the same one its usecase refusal reads. Import it;
  never restate it.
- R2001: the API refuses composed with `composed_unavailable` (HTTP 503) while the image is
  unconfigured. **The UI must not offer an option the API will refuse.** It reads availability from
  the same authority, never a second copy of the rule (memory: one authority per fact).
- R2004: `fixture_stream_settings.overlay_delay_ms` is nullable. `null` means default
  (`RELAY_OVERLAY_DELAY_DEFAULT_MS`), and `0` is a real override. Re-pin the settings write route
  (the settings PUT/PATCH and its zod schema). Extend it with
  `overlayDelayMs: z.number().int().min(0).max(30000).nullable().optional()`, where absent means
  unchanged and `null` means reset to default. Re-run the OpenAPI drift check (`ci.yml` step; re-pin
  the generate command).
- Fixture-page stream spec §2 (AGENTS.md "phone composition" section): *Remote scoring* and *Stream*
  are the only duplicated phone/desktop controls. **Don't add a third duplicate.** The mode choice
  lives inside the existing stream panel.

## Product shape

> **Superseded 2026-10-19.** The owner picked Option C (amendment at the end): the score is ON by
> default, there is no segmented control and no second Go live. Options A and B are kept below as the
> record of what was presented.

The owner chooses between the options in step 1. These are the two to present:

- **Option A: segmented control.** In the panel, above *Go live*: `Camera only | Camera + score`.
  The second is disabled with an inline reason when composed is unavailable or not entitled. A
  collapsed "Score timing" row under it shows the effective delay and a number input
  (seconds, 0–30, step 0.1) with *Reset to default*.
- **Option B: two go-live buttons.** *Go live* and *Go live with score*. The delay sits in the
  panel's existing settings disclosure.

**Recommendation: A.** It's one primary action, the choice is visible before committing, and it
reuses the panel's existing pattern. B doubles the most consequential button on a phone.

**Copy** (all 4 locales ~~`apps/web/dictionaries/{en,es,fr,nl}/ui.json`~~
`apps/web/src/dictionaries/{en,es,fr,nl}/ui.json` (path corrected 2026-10-15), then
`pnpm i18n:gen-keys`, which is a generated file, never hand-edited):

| Purpose | English |
|---|---|
| Mode label | "Stream" |
| Option | "Camera only" |
| Option | "Camera + score" |
| Unavailable reason | "Score overlay streaming isn't available yet" |
| Not-entitled reason | re-pin the existing paywall copy pattern (`feature-reasons`); reuse it, don't fork it |
| Delay row | "Score timing" |
| Delay help | "Delay the score to match your camera. Default {seconds}s." |
| Reset | "Reset to default" |

The non-English strings are translated properly, not copied English. Follow the dictionary's
existing tone.

## Files

- Modify: `components/v2/fixture-stream-panel.tsx`, `server/stream-panel-context.ts`
- Modify: `lib/stream-session-view.ts` (the composed camera chip, read from the session's
  `last_heartbeat.programmePhase`; plan re-review N2)
- Modify: the settings route and its schema (re-pin), plus the generated OpenAPI artifacts
- Modify: ~~`apps/web/dictionaries/{en,es,fr,nl}/ui.json`~~ `apps/web/src/dictionaries/{en,es,fr,nl}/ui.json`
  (path corrected 2026-10-15), plus the regenerated keys file
- (Amendment 2026-10-15) Create: `apps/web/src/components/v2/stream-template-picker.tsx`. Modify:
  `server/usecases/stream-codes.ts` (a `writeOverlayTemplate` writer), `PutStreamSettings` /
  `StreamSettings`, and the panel's OBS URL and preview
- Test: the panel's unit tests (re-pin), the settings route test, and the e2e spec. Re-pin the
  existing stream-panel e2e **by testid `stream-go-live`** (grep the selector), and extend it.

## Steps

- [ ] **Step 1: Show the options first** (house rule: 2+ UI options before building, mobile first).
  Render Options A and B as quick static mocks at 320 and 1280, with screenshots cropped to the
  panel. Post them to the owner **in chat**, with the recommendation. **Wait for the pick.**
- [ ] **Step 2: Write the failing tests.**
  1. **Route.** PUT `overlayDelayMs: 2500` persists. `null` resets to null. Absent leaves it
     unchanged. `-1` and `30001` give a 400. `0` persists as 0 (not null). Use the real route.
  2. **Panel.**
     - with composed available and entitled, choosing *Camera + score* sends `mode: "composed"`
       through the real start call (fake drivers)
     - *Camera only* sends `passthrough`
     - when composed is unavailable, the option is disabled and the reason is shown
     - ~~the default selection is *Camera only*, so the existing behaviour holds~~ (superseded
       2026-10-19: the default is the score whenever it can stream; *Camera only* is the opt-out)
  3. **Effective-delay display.** With null it shows the default from the server, never a hardcoded
     number. With 0 it shows 0.0 s.
  4. **Composed camera chip** (ruling A: the session stays `live` while the camera is out, so the
     panel is the only place the organiser learns of it). Fold R2002's real
     `heartbeat-full.json` fixture, with `programmePhase` set to each wire value, through the
     view builder in `lib/stream-session-view.ts`: `live` → no camera chip; `frozen` and
     `reconnecting` → a *camera reconnecting* chip; `waiting` → *waiting for camera*; (added
     2026-10-15) `final` → no camera chip (the stream is in its final hold; that isn't a camera
     problem). Enumerate every
     `programmePhase` value from R2002's schema enum (never a list typed in the test) (amended
     2026-10-15: that enum is `PROGRAMME_PHASES` in `lib/relay-phase.ts`, R2007 PR B, 5 values), and print
     `phases checked: N`, failing on `N == 0`. A passthrough session never shows the chip. The copy
     goes in all 4 locales.
  5. **(Amendment 2026-10-15)** tests P1–P4 from the amendment section.
- [ ] **Step 3: Write the failing e2e,** extending the stream-panel spec.
  - At all **seven** widths (`mobile.spec.ts` projects 320/360/375/390/430/768/834), plus desktop
    1280:
    - pick *Camera + score* → Go live
    - assert the session row's mode is `composed` (read back through the API, not the DOM)
    - set the delay 2.5, reload, see 2.5
    - reset → default
  - Run it with the panel's disclosure **opened at phone widths**: gate on the toggle being visible,
    and wait on `toBeAttached` (AGENTS.md class 22).
  - No horizontal scroll at any width.
  - **(Amendment 2026-10-15)** e2e cases PE1–PE3 from the amendment section, at the same widths.
- [ ] **Step 4: Run, expect a FAIL.**
- [ ] **Step 5: Implement the chosen option.**
- [ ] **Step 6: Run, expect a PASS.**
  - Run the **whole** e2e spec file, never a `-g` slice (AGENTS.md class 21).
  - `mobile.spec.ts` is serial: if it goes red, re-run after each fix until a full pass completes.
  - Screenshots at 320, 768 and 1280, cropped to the panel. Post them in the PR comment, **never
    committed**.
- [ ] **Step 7: Mutate once each.**
  1. Hardcode `passthrough` again. Expected red: panel test 2.
  2. Send `0` as null. Expected red: route test 1 and the e2e.
  3. Map `reconnecting` to no chip. Expected red: panel test 4.
  4–7. **(Amendment 2026-10-15)** mutants PM4–PM7 from the amendment section.
- [ ] **Step 8: Run the scoped gate:** touched unit files plus the e2e spec, lint (`rtk proxy`),
  the typecheck, and the OpenAPI drift check. **Commit and open the PR:**
  `feat(stream): organisers can go live with the score composited, and tune its timing`

## Acceptance

- The owner picked an option.
- Unit, route and e2e tests pass at 7 widths plus 1280.
- The 4 locales are filled and keys regenerated.
- ~~3 mutants~~ 7 mutants went red (amended 2026-10-15).
- Screenshots are posted, with per-screen owner verdicts.
- (Amended 2026-10-15) The picker is shown with live previews at 320, 768 and 1280. The stored choice
  reaches both the OBS URL and the composed `overlayUrl` (PE2 drives the whole seam).

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.4 (D5, D6, D7), §4.7 and §6, R2006 row (verbatim, `29fa9f59c`): "Adds the per-fixture template
picker with live previews, on mobile and desktop, plus persisting the choice and using it in the OBS
URL and the composed `overlayUrl`."

**This amends the 2026-09-05 owner ruling "style in the URL, not the DB"** (spec §4.4: the composed
path needs the choice server-side, because no human pastes the compositor's URL). It was half a false
premise: `fixture_stream_sessions.theme_id` already existed server-side (R2004's amendment records it).
R2004 adds `fixture_stream_settings.overlay_template` and its reader. **This task is its only writer.**

### The picker

- `stream-template-picker.tsx` renders `templatesForSport(stream.sportKey)` (retired templates are
  hidden by R2007), in registry order, as a `role="radiogroup"` with one `role="radio"` option per
  template (`aria-checked`, testid `stream-template-<id>`). Don't reuse `stream-tab-<id>`: the panel's
  own tabs already use `stream-tab-phone` and `stream-tab-obs`, so a template id could collide with
  them.
- **Each option's thumbnail is `<OverlayPreview>`** (R2007, the one render function; D6). It is
  rendered from **this fixture's real data**: the panel's `live ?? seed` `OverlayLiveData`
  (`fixture-stream-panel.tsx:341`, `:417`), its real sides and start label, and `stream.overlayDict`.
  It is never a second rendering path and never a picture.
- A **scene toggle** above the options, one segment per `OVERLAY_SCENES` entry (Live / Starting soon /
  Reconnecting / Final, labels from R2005). It changes the thumbnails only and is never persisted.
- Every option and every segment has a tap target of at least 44 × 44 px, proven by hit-test
  (`elementFromPoint` at the box centre returns the control), not by `boundingBox()` alone (AGENTS.md
  class 2). No horizontal page scroll at 320, 768 or 1280.
- **Placement:** it replaces the OBS tab's style strip (`fixture-stream-panel.tsx:548-563`), and the
  same single component shows under *Camera + score* for composed. It is **one DOM instance**, not a
  phone/desktop duplicate (AGENTS.md phone composition: exactly two controls are duplicated).
- **Step 1's option mocks** (house rule: 2+ options, mobile first) gain the picker layout:
  - **Grid:** a 2-up grid at ≥ 375 and 1-up at 320
  - **Rail:** a horizontal scroll rail of thumbnails

  **Recommendation (not an owner ruling): the grid.** A rail is a scrolling region, which owes
  `tabindex="0"`, a role and a name or axe reds `scrollable-region-focusable`, and the `mobile.spec.ts`
  clipping scan has to exempt it (AGENTS.md class 23).

### Persistence

- `PutStreamSettings` gains `overlayTemplate: z.string().min(1).max(40).nullable().optional()`: absent
  leaves it unchanged, `null` resets to the sport default, and 40 matches `CreateStreamSession.themeId`
  (`schemas.ts:1387`). The refine becomes "at least one of `targetId`, `autoStream`, `overlayDelayMs`,
  `overlayTemplate`". `StreamSettings` gains `overlayTemplate: z.string().nullable()`. Regenerate
  OpenAPI.
- `writeOverlayTemplate(tx, { orgId, fixtureId, overlayTemplate, updatedBy })` in `stream-codes.ts`,
  modelled on `writeAutoStream`: an upsert that sets only `overlay_template`, `updated_by` and
  `updated_at`, never `target_id`/`target_chosen`.
- **Refusal before the write:** an id that `templatesForSport(sport)` doesn't offer (unknown, retired,
  or unsuitable for the sport) → `HttpError(400, "overlay template not available for this fixture")`.
  The rule is `isPickableTemplate(id, sportKey, registry = OVERLAY_TEMPLATES)`, exported so a unit test
  can inject a registry that has a retired and a sport-restricted entry (the shipped one has neither).
  This is the `resolveThemeFrom` precedent.
- The panel opens on `resolveTemplate(saved, sport).id`, replacing today's local
  `useState(defaultThemeFor…)` (`:335`). A pick is saved immediately, optimistically, and reverts with
  the copy below if the PUT fails.
- **The OBS URL** (`:408`) becomes `?template=${selected}`. `?style=` stays readable forever by the
  page (R2007), but the panel no longer writes it. `fixture-stream-panel.test.tsx:404` and `:417` move
  from `searchParams.get("style")` to `get("template")`, and assert that `style` is absent.
- The composed `overlayUrl` reads the stored choice through R2004's copy at session create. This task
  only has to prove the seam end to end (PE2).

Copy (`ui.json`, all 4 locales, then `pnpm i18n:gen-keys`):

| Key | English |
|---|---|
| `stream.template.title` | "Score style" |
| `stream.template.scenes` | "Preview" |
| `stream.template.saveFailed` | "Couldn't save the score style. Try again." |

### Tests

- **P1 Route** (the real route): `overlayTemplate: "bar"` persists, and the answer says `"bar"`;
  `null` resets to `null`; absent leaves it unchanged (send `{ autoStream }` alone); `"nope"` → 400 and
  nothing written; `{ overlayTemplate: "bar" }` alone passes the refine; `targetId` and
  `target_chosen` are untouched by a template write.
- **P2 `isPickableTemplate`** with an injected registry: retired → false; restricted to badminton →
  false for cricket and true for badminton; every `OVERLAY_SPORT_KEYS` sport's default → true. Print
  `sports checked: 11`; fail on 0.
- **P3 Panel** (unit): one option per `templatesForSport(sport)` entry, the count taken from the
  registry declaration; each thumbnail is an `ovl-preview` whose markup contains this fixture's entrant
  names; picking `bar` sends a PUT with body `{ overlayTemplate: "bar" }`; the OBS URL carries
  `template=bar` and no `style`.
- **P4 Scene toggle** (unit): selecting each `OVERLAY_SCENES` entry sets `data-scene` on every
  thumbnail. Print `scenes checked: N`, where N is `OVERLAY_SCENES.length` (4 once R2005 has merged);
  fail on 0.

**E2E** (the panel spec; all seven widths plus 1280; disclosure opened at phone widths per class 22):
- **PE1:** pick the template that is **not** the sport's default, reload, and it's still selected.
  Read the value back through `GET`/`PUT` on `stream-settings` via the API, not the DOM. The copied
  OBS URL carries `template=<picked>`.
- **PE2 The seam:** with composed available (`RELAY_DRIVERS=fake`, a dummy `RELAY_IMAGE`), pick a
  template, choose *Camera + score*, and go live. Then read the session's job facts through the
  internal route with its job token (R2004's precedent): `overlayUrl` carries `template=<picked>`.
- **PE3:** each scene segment changes every thumbnail's `data-scene`. Every option and segment passes
  the 44 px hit-test, and there's no horizontal scroll.

**Mutants:**

| # | Mutant | Expected red |
|---|---|---|
| PM4 | the scene toggle is ignored (thumbnails always render `live`) | P4, PE3 |
| PM5 | the OBS URL keeps `?style=` | P3 |
| PM6 | a pick isn't persisted (local state only) | PE1's reload |
| PM7 | the refusal is skipped | P1's `"nope"` case |

## Amendment 2026-10-19 (owner ruling: Option C)

The owner approved **Option C** on 2026-10-19 ("Ok approved"), from the mocks posted in chat. It is
neither A nor B. Built on `onryde/seazn.club` branch `feat/r2006-organiser-composed-ui` (PR #989).

### Product shape (replaces A/B)

- **The score is on by default.** On the Phone tab's Ready card the style grid (default preselected)
  and a collapsed *Score timing* row sit above ONE *Go live*. Go live sends `mode: "composed"` with the
  shown `themeId`.
- **Camera only** is a small link under the card ("Camera only (no score)"); it stores
  `streamMode: "passthrough"` and swaps the card for a one-line camera-only note with *Add the score
  back*, which stores `streamMode: "composed"` (corrected 2026-10-19, PR #989 review m5: it does not
  clear the field; a stored `composed` and a null both read as the score whenever it can stream).
- **Composed unavailable** (no compositor image, or the relay not entitled): a one-line reason and a
  camera-only Go live. Never blocked, never a disabled option.
- **The OBS tab:** the style grid replaces the style tabs and the big preview.
- **A live composed session** shows its style and timing read-only (the session's own copy, "Set
  before going live") and the line "YouTube delivery isn't monitored for score streams yet"; the
  signal chain's YouTube node is NEUTRAL, never green or red (Owed 26).
- **Switch heights:** 36 px at ≥768, 44 px on phones.

### The stored mode, read by all three starts

- `fixture_stream_settings.stream_mode` (`'composed' | 'passthrough' | null`), migration **V465**
  (renumbered from V464 on 2026-10-19, PR #989 review C1). W2c (`feat/format-matrix-w2c`) holds V463
  and `seazn.club` PR #991 (`feat/claim-email-signin`) holds V464, both unmerged. Flyway runs without
  `outOfOrder`, so the required merge order is **V463 (W2c) → V464 (#991) → V465 (#989)**, with no prod
  tag cut between them (the coordinator sequences it).
- `effectiveStreamMode(stored, available)` in `lib/stream-settings.ts` is the one rule: a stored
  `passthrough` is Camera only; anything else is the score when it can stream, else the camera.
- The organiser's Go live, the phone's own start and the automatic start all read it
  (`startModeFor`). The automatic-stream line says what it will start with.
- `PutStreamSettings` gains `streamMode`; the answer `StreamSettings` carries `streamMode`,
  `overlayTemplate` and `overlayDelayMs` (all nullable).

### Score timing

- 0–30 s, step 0.1, *Reset to default*; null = the server default (handed in), 0 a real override.
- Editable before going live only; read-only while live with "Set before going live".
- No delay cap (Owed 36).

### Out of scope (follow-ups)

- The camera phone's own timing control (capture app).
- Adjusting the delay while live.
- The composed camera chip (`programmePhase`) — Step 2.4 and mutant 3 above are deferred, not built.
  It is owed to a web follow-up before C17 (`_INDEX.md` Owed 52), and C17 Step 3 row 3 no longer
  expects it.

### Added during the build

- **I3 (PR #987 review, routed here):** the style-grid thumbnails render the AIRED sides — read through
  the overlay page's own loader (`relayOverlayPageFacts`), org-checked — so a person pair previews the
  codes it airs (Lee Chong Wei v Viktor Axelsen: WEI/AXE, not LEE/VIK).
- **CI e2e:** e2e-parallel sets a dummy `RELAY_IMAGE`, so the score is every start's default there; a
  walkthrough that follows the camera's lifecycle stores Camera only on its fixture first.
- **The owner's per-screen sign-off** (2026-10-19, relayed by the coordinator: "approve"): all 6 screens,
  as built.
- **PR #989 review fixes (2026-10-19), as built:**
  - Step 3's width e2e is built: `e2e/overlay-compositor.spec.ts`, one test per width (320, 360, 375,
    390, 430, 768, 834 and 1280), on the e2e-parallel leg, which has the image. `mobile.spec.ts` runs
    without the image, so the score card never renders there. Each test checks two things: reachability
    (`elementFromPoint`, 44 px below 768 and 36 px from 768), and no clipping (`overflowingIn`). It then
    runs the sequence: Camera only and back, a style and 2.5 s that survive a reload, Reset, and Go live,
    with the mode read back through the API.
  - Go live sends `overlayDelayMs`, the timing the card showed, in `CreateStreamSession`. Without it the
    request raced the timing's own save, which fires on blur. If the field is absent, the stored timing
    applies; if it is null, the server's default applies. That covers the phone's start and the
    automatic start, which send none.
  - While a composed session is open, the OBS tab's grid says "Applies to the next stream". It is still
    editable, and a running session never moves.
  - The stored preferences re-seed from a newer page read for every field with no save in flight.
  - Both radiogroups use arrow keys with a roving tabindex.
  - A number the browser rejects shows the invalid line.

## Do not touch

The overlay page (R2005), relay usecases (R2004), and `/admin`. (Amended 2026-10-15) The template
registry and `OverlayPreview` (R2007): use them, don't fork them.

## Output cap

Under 15 lines.
