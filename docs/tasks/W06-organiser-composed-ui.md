# W06: Organiser UI (composed-mode option and score-delay override)

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/w06-composed-ui -b feat/stream-panel-composed origin/main`.
**Depends on:** W04 and W05 merged. **Use the `frontend-design` skill.** It's a house rule for
every UI task; put the invocation in the dispatch.

## Context

Pinned 2026-10-14 at `09290de3c`. **Re-pin everything.**

- `components/v2/fixture-stream-panel.tsx:1140` starts a session with a **hardcoded**
  `mode: "passthrough"` (testid `stream-go-live`).
- `relayOffer` lives in `components/v2/stream-panel-context.ts:37`. Re-pin what it carries:
  entitlement, availability, and whether W01's `composed_unavailable` refusal is surfaced.
- W01: the API refuses composed with `composed_unavailable` (HTTP 503) while the image is
  unconfigured. **The UI must not offer an option the API will refuse.** It reads availability from
  the same authority, never a second copy of the rule (memory: one authority per fact).
- W04: `fixture_stream_settings.overlay_delay_ms` is nullable. `null` means default
  (`RELAY_OVERLAY_DELAY_DEFAULT_MS`), and `0` is a real override. Re-pin the settings write route
  (the settings PUT/PATCH and its zod schema). Extend it with
  `overlayDelayMs: z.number().int().min(0).max(30000).nullable().optional()`, where absent means
  unchanged and `null` means reset to default. Re-run the OpenAPI drift check (`ci.yml` step; re-pin
  the generate command).
- Fixture-page stream spec §2 (AGENTS.md "phone composition" section): *Remote scoring* and *Stream*
  are the only duplicated phone/desktop controls. **Don't add a third duplicate.** The mode choice
  lives inside the existing stream panel.

## Product shape

The owner chooses between the options in step 1. These are the two to present:

- **Option A: segmented control.** In the panel, above *Go live*: `Camera only | Camera + score`.
  The second is disabled with an inline reason when composed is unavailable or not entitled. A
  collapsed "Score timing" row under it shows the effective delay and a number input
  (seconds, 0–30, step 0.1) with *Reset to default*.
- **Option B: two go-live buttons.** *Go live* and *Go live with score*. The delay sits in the
  panel's existing settings disclosure.

**Recommendation: A.** It's one primary action, the choice is visible before committing, and it
reuses the panel's existing pattern. B doubles the most consequential button on a phone.

**Copy** (all 4 locales `apps/web/dictionaries/{en,es,fr,nl}/ui.json`, then
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

- Modify: `components/v2/fixture-stream-panel.tsx`, `components/v2/stream-panel-context.ts`
- Modify: the settings route and its schema (re-pin), plus the generated OpenAPI artifacts
- Modify: `apps/web/dictionaries/{en,es,fr,nl}/ui.json`, plus the regenerated keys file
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
     - the default selection is *Camera only*, so the existing behaviour holds
  3. **Effective-delay display.** With null it shows the default from the server, never a hardcoded
     number. With 0 it shows 0.0 s.
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
- [ ] **Step 8: Run the scoped gate:** touched unit files plus the e2e spec, lint (`rtk proxy`),
  the typecheck, and the OpenAPI drift check. **Commit and open the PR:**
  `feat(stream): organisers can go live with the score composited, and tune its timing`

## Acceptance

- The owner picked an option.
- Unit, route and e2e tests pass at 7 widths plus 1280.
- The 4 locales are filled and keys regenerated.
- 2 mutants went red.
- Screenshots are posted, with per-screen owner verdicts.

## Do not touch

The overlay page (W05), relay usecases (W04), and `/admin`.

## Output cap

Under 15 lines.
