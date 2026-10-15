# R2007: Overlay template system, readability floor, and the staff gallery

**Repo:** `onryde/seazn.club`. Two PRs from one brief:

- **PR A (the template system):** worktree
  `git worktree add ../seazn.club-wt/r2007-overlay-templates -b feat/overlay-template-system origin/main`.
  **Depends on:** the end-card-undo fix (branch `fix/overlay-end-card-undo`) **merged to `main`**. It
  edits the overlay stage and the live-fixture hook this task also edits, so start PR A only after it
  lands, and re-pin the stage from the merged `main`.
- **PR B (the owed R2002 contract follow-up, §"PR B" below):** worktree
  `git worktree add ../seazn.club-wt/r2007-relay-final-phase -b feat/relay-contract-final-phase origin/main`.
  **Does not wait for the undo fix**: it touches no `components/overlay/**` file. It must merge
  **before C07 vendors the contract**.

**Use the `frontend-design` skill** (house rule for every UI task; put the invocation in the dispatch).
**Spec (authority):** `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
(owner-approved 2026-10-15; branch `docs/overlay-templates-and-scenes` @ `29fa9f59c`). This task covers
§4.4–4.7 and the gallery and render-function part of §4.8. Scenes themselves (§4.1–4.3, the rest of
§4.8) are R2005. The picker is R2006. Also read `docs/superpowers/specs/2026-09-05-stream-overlay-prompts/_THEMES.md`.

## Context

Pinned 2026-10-15 at `seazn.club` `origin/main` `b01d07cd5`. **Re-pin everything after the undo fix
merges**: it moves lines in `overlay-stage.tsx`.

`apps/web/src/components/overlay/theme-registry.ts` (the module this task turns into the template
registry):

| Lines | What |
|---|---|
| `:36-45` | `OverlayThemeProps { model, tick, msg, sportKey }`. The spec keeps this name for the component props (§4.7). |
| `:53` | `ThemeId = "bar" \| "bug"`, an explicit union (its doc says why; keep that reasoning for `TemplateId`) |
| `:55-66` | `OverlayThemeDef { id, labelKey, component, sports }` |
| `:68-71` | `OVERLAY_THEMES`: `bar` and `bug`, label keys `stream.tab.bar` / `stream.tab.bug` (`ui.json`), both `sports: "all"` |
| `:75-77` | `defaultThemeFor`: cricket → `bar`, every other sport → `bug` |
| `:84-86` | `slabPlacementFor(style)`: the spec replaces it with the per-template `slabPlacement` field |
| `:93` | `themesForSport` |
| `:102-116` | `resolveThemeFrom(themes, styleParam, sportKey, fallback)`: hardcoded retired `slate` → fallback (`:109`), `Object.hasOwn` against `?style=toString` |
| `:123` | `resolveTheme` |

Callers (grep `-a` again, by symbol, before editing):

| File | Lines | Use |
|---|---|---|
| `app/overlay/fixtures/[fixtureId]/page.tsx` | `:19`, `:54` (query type `{ style, lang, delay, key }`), `:65` (destructure), `:157` (`style={resolveTheme(style, division.sport_key).id}`) | the OBS / compositor page |
| `components/overlay/overlay-stage.tsx` | `:33` import, `:48` prop `style: ThemeId`, `:155` `slabPlacementFor(props.style)`, `:243` `OVERLAY_THEMES[props.style].component`, `:304-321` the moment slot `ovl-moment-slot--${placement}` | the stage |
| `components/overlay/overlay-slate.tsx` | `:14` (type import) | match card layer |
| `components/v2/fixture-stream-panel.tsx` | `:38`, `:291` `themesForSport`, `:335` `useState(defaultThemeFor…)`, `:408` OBS URL `?style=`, `:548-563` style strip | R2006 owns behaviour here; this task only renames imports |
| `server/usecases/capture-phone.ts` | `:19`, `:176` `s.themeId ?? defaultThemeFor(…)` | the phone overlay URL keeps `?style=` (the alias) |
| tests | `theme-registry.test.ts`, `moment-queue.test.ts:12,:155-166`, `overlay-stage-hold.test.tsx:27`, `overlay-stage-theme-props.test.tsx:22,:162-189`, `fixture-stream-panel.test.tsx:42,:304-417` (asserts `searchParams.get("style")` at `:404`, `:417`), `capture-get.test.ts:18,:100,:298,:313` | renamed imports only; their assertions stay |
| e2e | `apps/web/e2e/stream-overlay.spec.ts:341`, `overlay-moments.capture.ts`, `e2e/visual/manifest.json` | by route `/overlay/fixtures` |

Other pins:

| Where | What |
|---|---|
| `components/overlay/overlay-stage.tsx:117` | `useLiveFixture(props.fixtureId, props.initial, props.realtime, { fetcher: fetchOverlayFixture, … })`; the hook polls every `POLL_MS = 15_000` (`components/public-site/match-centre/use-live-fixture.ts:10`) **whatever `realtime` says**, so a preview over sample data would poll a fixture id that doesn't exist |
| `overlay-stage.tsx:153` | `momentBaseline` (lazy `useState(maxSeq(initial.recent))`): a static render never raises a slab, so a gallery moment cell needs a pinned moment |
| `overlay-stage.tsx:290-293` | match card rendered before the template component |
| `components/v2/fixture-stream-panel.tsx:566-610` | the console preview: a `CANVAS_W × CANVAS_H` canvas scaled into an `aspect-ratio` strip. The shared render function lifts exactly this pattern. |
| `app/globals.css:1369` | `.ovl-canvas` 1920×1080 |
| `app/globals.css:1393` | `.ovl-canvas { --ovl-bar-main-h: 126px; --ovl-bar-band-h: 51px; --ovl-bar-bottom: 54px; }` |
| `app/globals.css:1384`, `:1571`, `:1702-1707`, `:1852` | `.ovl-bar` (left/right 72), `.ovl-bug` (left 60, top 54, width 480), `.ovl-slate-content` / `.ovl-slate-card` (max-width 1100), `.ovl-moment-slot--bug` |
| `app/globals.css` | **26 `.ovl-` rules set a font-size below 30 px** at pin (line: selector px): 1408 `.ovl-live-row` 24; 1428 `.ovl-context` 21; 1436 `.ovl-innings-strip` 21; 1441 `.ovl-bug-innings-strip` 18; 1492 `.ovl-detail-band` 24; 1553 `.ovl-ball-glyph` 15; 1579 `.ovl-bug-header` 21; 1586 `.ovl-bug-status` 21; 1596 `.ovl-bug-context` 19.5; 1599 `.ovl-bug-brand` 24; 1646 `.ovl-bug-footer` 21; 1731 `.ovl-slate-pill` 18; 1739 `.ovl-slate-line` 24; 1756 `.ovl-slate-tile-score` 28; 1767 `.ovl-slate-name-pill` 20; 1782 `.ovl-slate-highlight-role` 14; 1785 `-name` 26; 1788 `-line` 28; 1792 `-detail` 18; 1969 `.ovl-slab__line` 21; 2048 `.ovl-end-of-over-title` 23; 2056 `-line` 20; 2064 `-bowler` 17; 2067 `-side-label` 14; 2083 `-glyph` 22; 2087 `-runs` 28 |
| `components/v2/scorepad/v3/__tests__/_globals-css.ts` | `parseCss`, `readGlobalsCss`, `declaredValue`: the repo's CSS reader. **Reuse it**; don't write a second parser. |
| `components/overlay/overlay-tokens.ts:100` | `OVERLAY_SPORT_KEYS`: 11 sports (badminton, boardgame, carrom, cricket, football, generic, hockey, icehockey, tabletennis, tennis, volleyball) |
| `lib/overlay-moments.ts:337-353` | `MOMENT_RULES: Record<sport, Record<ruleKey, MomentRule>>`: **17 (sport, rule key) entries** at pin: cricket 2, football 2, hockey 2, icehockey 2, tennis 3, badminton 2, tabletennis 2, volleyball 2; boardgame, carrom and generic `{}` |
| `lib/overlay-moments.ts:397` | `momentsFor(sportKey, recent, sinceSeq, msg, sides)` |
| `server/engine-db/fold.ts:171` | `foldFrom(fixtureId, { module, cfg, lineups, envelopes })`: the pure fold |
| `server/overlay/project.ts:170` | `projectOverlayLiveData({ row, folded, venueTz, recent, … })`: the pure projection the overlay route serves |
| `app/admin/layout.tsx:31` | the admin nav; `capture-records` is the localised precedent (`t(dict, "admin.captureRecords.nav")`, `ui.json`) |
| `apps/web/e2e/capture-records-admin.spec.ts` | the admin e2e precedent: a fresh org whose owner is made staff, 1280 only |
| `scripts/smoke.ts:918` | `streamOverlaySuite()`, the overlay route over real HTTP |

**Hypothesis to settle in step 9, not to assume:** CSS arithmetic puts the match card at about
x 410–1510 and the bug box at x 60–540, so they may overlap in x. Whether they overlap on screen is
for the e2e containment check to say.

## Decisions this brief makes

The orchestrator puts these to the owner if a re-pin contradicts them.

1. **Rename, don't wrap.** `git mv theme-registry.ts template-registry.ts` (and its test), and rename
   the exports to the spec's names. The component props keep the name `OverlayThemeProps` (spec
   §4.7). The stage's `style` prop and the DOM attribute `data-style` **keep their names**: `style`
   is the alias forever (§4.4), and e2e specs and OBS-facing tests read `data-style`. Renaming them is
   churn with no reader.
2. **`template` wins when present.** `?template=<id>` is read first. `?style=` is read only when
   `template` is absent or empty. An unknown `template` resolves to the sport default (never to
   `style`'s value): one rule, no fall-through chain.
3. **The `scenes` field is not added.** §4.4 says the registry carries "the scenes it supports"; §4.7's
   contract table (the later, more exact section) does not list it. Scenes are a page layer above every
   template, so every template supports every scene by construction, and the readability and safe-area
   sweeps enforce that for each scene R2005 adds. Record this in the PR body as a resolved spec
   ambiguity.
4. **`slate` needs no special case.** It isn't a registry key, so it already resolves as unknown →
   sport default. The test pins `?style=slate` → default.
5. **Bar lift: `--ovl-bar-bottom: 162px`** (15 % of 1080). That is clear of the ~14 % controls zone
   (≈ 151 px) with 11 px to spare. The tests pin the spec's floor (150), never the chosen value.
6. **Gallery snapshots are folded, not typed.** For each sport, scripted engine events go through the
   real `foldFrom` and `projectOverlayLiveData` at request time: real producers, no DB, no hand-built
   `OverlayLiveData` (AGENTS.md class 1).
7. **Moment samples:** at least one per `(sport, rule key)` in `MOMENT_RULES`, and three for cricket's
   `cricket.ball` (six, four, wicket: spec §4.8 names all three), so 19 samples at pin. Each sample is
   a `RecentEvent`; the gallery derives the slab from it through the real `momentsFor`.

## Files (PR A)

- Rename: `apps/web/src/components/overlay/theme-registry.ts` → `template-registry.ts`, and
  `__tests__/theme-registry.test.ts` → `__tests__/template-registry.test.ts`
- Create:
  - `apps/web/src/components/overlay/overlay-scenes.ts` (the scene list; R2005 extends it)
  - `apps/web/src/components/overlay/overlay-preview.tsx` (the shared render function, `"use client"`)
  - `apps/web/src/components/overlay/gallery-matrix.ts` (pure: cells and moment samples)
  - `apps/web/src/server/overlay/gallery-snapshots.ts` (server-only: scripted folds per sport)
  - `apps/web/src/app/admin/overlay-gallery/page.tsx` (server; `requireStaff()`)
  - `apps/web/src/app/admin/overlay-gallery/overlay-gallery.tsx` (`"use client"`)
- Modify:
  - `apps/web/src/app/overlay/fixtures/[fixtureId]/page.tsx` (query `template`, the alias)
  - `apps/web/src/components/overlay/overlay-stage.tsx` (registry lookup, `slabPlacement`, two
    preview-only props)
  - `apps/web/src/components/overlay/overlay-slate.tsx`, `overlay-bar.tsx`, `overlay-bug.tsx` and
    the slab / end-of-over components (re-pin) only as the readability redesign requires
  - `apps/web/src/app/globals.css` (text floor, bar lift)
  - `apps/web/src/components/v2/fixture-stream-panel.tsx` and
    `apps/web/src/server/usecases/capture-phone.ts`: **import renames only**
  - `apps/web/src/app/admin/layout.tsx` (one nav link)
  - `apps/web/src/dictionaries/{en,es,fr,nl}/ui.json`, then `pnpm i18n:gen-keys` (generated; never
    hand-edit the keys file)
  - `scripts/smoke.ts` (`streamOverlaySuite`)
- Test:
  - `components/overlay/__tests__/template-registry.test.ts` (renamed, extended)
  - `components/overlay/__tests__/template-extensibility.test.ts`
  - `components/overlay/__tests__/overlay-readability-css.test.ts`
  - `components/overlay/__tests__/gallery-matrix.test.ts`
  - `components/overlay/__tests__/overlay-preview.test.tsx`
  - `components/overlay/__tests__/overlay-slab-placement.test.tsx` (extend; its geometry pins move with
    the bar lift)
  - `server/overlay/__tests__/gallery-snapshots.test.ts`
  - `app/overlay/fixtures/[fixtureId]/__tests__/page.test.ts` (extend)
  - e2e: create `apps/web/e2e/overlay-gallery.spec.ts`; extend `apps/web/e2e/stream-overlay.spec.ts`

## Interfaces

- **Consumes:** `OverlayThemeProps`, `OverlayLiveData` (`components/public-site/live-score-data.ts:62`),
  `OverlayMoment`, `RecentEvent`, `MOMENT_RULES`, `momentsFor`, `OVERLAY_SPORT_KEYS`, `foldFrom`,
  `projectOverlayLiveData`.
- **Produces** (R2004, R2005 and R2006 build on these exact names):
  ```ts
  // components/overlay/template-registry.ts
  export interface OverlayThemeProps { /* unchanged */ }
  export type TemplateId = "bar" | "bug";
  /** A box on the 1920×1080 canvas, in canvas px. */
  export interface CanvasBox { x: number; y: number; width: number; height: number }
  export interface OverlayTemplateDef<Id extends string = TemplateId> {
    /** Stable. Used in ?template=, stored choices and OBS URLs. Never changes once shipped. */
    id: Id;
    /** `ui` namespace; 4 locales. */
    labelKey: string;
    sports: "all" | readonly string[];
    component: ComponentType<OverlayThemeProps>;
    /** Where the moment slab attaches. Replaces `slabPlacementFor`. */
    slabPlacement: "bar" | "bug";
    /** The box scene and match cards must keep clear. The e2e proves the template renders inside it. */
    sceneCardSafeArea: CanvasBox;
    /** Kept so stored choices and OBS URLs resolve. Hidden from the picker. */
    retired?: { replacedBy: Id };
  }
  export type TemplateRegistry<Id extends string = string> = Readonly<Record<Id, OverlayTemplateDef<Id>>>;
  export const OVERLAY_TEMPLATES: Readonly<Record<TemplateId, OverlayTemplateDef>>;
  export function defaultTemplateFor(sportKey: string): TemplateId;
  /** Registry order; retired and sport-unsuitable entries hidden. */
  export function templatesForSportFrom<Id extends string>(registry: TemplateRegistry<Id>, sportKey: string): readonly OverlayTemplateDef<Id>[];
  export function templatesForSport(sportKey: string): readonly OverlayTemplateDef[];
  /** Unknown, unsuitable, retired-with-a-dead-chain → `fallback`. Retired → its replacement (≤ registry-size hops). Never throws. */
  export function resolveTemplateFrom<Id extends string>(registry: TemplateRegistry<Id>, requested: string | null | undefined, sportKey: string, fallback: Id): OverlayTemplateDef<Id>;
  export function resolveTemplate(requested: string | null | undefined, sportKey: string): OverlayTemplateDef;
  /** `template` when present and non-empty, else `style`. Array values take the first element. */
  export function templateParam(q: { template?: string | string[]; style?: string | string[] }): string | undefined;

  // components/overlay/overlay-scenes.ts
  export const OVERLAY_SCENES = [{ id: "live", labelKey: "stream.scene.live" }] as const; // R2005 appends starting, reconnecting, final
  export type SceneId = (typeof OVERLAY_SCENES)[number]["id"];

  // components/overlay/overlay-preview.tsx
  export interface OverlayPreviewProps {
    template: TemplateId; sportKey: string; scene: SceneId;
    snapshot: OverlayLiveData; sides: [OverlaySideInput, OverlaySideInput];
    startLabel: string | null; dict: Record<string, string>; decidedTemplates: DecidedOutcomeTemplates;
    /** A slab to show statically (gallery moment cells). */
    moment?: OverlayMoment;
    /** Rendered frame width in CSS px; the height is width × 1080 / 1920. */
    frameWidth: number;
  }
  export function OverlayPreview(props: OverlayPreviewProps): JSX.Element; // root data-testid="ovl-preview", data-template, data-scene

  // components/overlay/overlay-stage.tsx: two new optional props, preview-only
  snapshotOnly?: boolean;          // true ⇒ the fetcher returns `initial`; no request ever leaves the page
  pinnedMoment?: OverlayMoment;    // shown in the slab slot in the queue's fully-shown phase (re-pin its name in moment-queue.ts)

  // components/overlay/gallery-matrix.ts (pure)
  export type GallerySnapshotKind = "warming" | "live" | "ended";
  export const GALLERY_SNAPSHOT_KINDS: readonly GallerySnapshotKind[]; // ["warming", "live", "ended"]
  export interface MomentSample { id: string; sportKey: string; ruleKey: string; event: RecentEvent }
  export const GALLERY_MOMENT_SAMPLES: readonly MomentSample[];
  export interface GalleryCell { key: string; templateId: string; sportKey: string; scene: SceneId; snapshot: GallerySnapshotKind; momentSampleId: string | null }
  /** Plain cells: every non-retired template suiting the sport × sport × scene × snapshot kind.
   *  Moment cells: every non-retired template suiting the sample's sport × sample (scene "live", snapshot "live"). */
  export function galleryMatrix(registry: TemplateRegistry, sports: readonly string[], scenes: readonly SceneId[], samples: readonly MomentSample[]): GalleryCell[];

  // server/overlay/gallery-snapshots.ts (server-only)
  export const GALLERY_SIDES: readonly [OverlaySideInput, OverlaySideInput];
  export function gallerySnapshots(): Record<string /* OVERLAY_SPORT_KEYS */, Record<GallerySnapshotKind, OverlayLiveData>>;
  ```
  `GALLERY_SIDES` is `[{ id: "home", name: "Milton Keynes Rovers" }, { id: "away", name: "Northbridge Athletic Football & Social Club" }]`.
  The second name is 43 characters, the length that exposed the `truncate`/`min-w-0` defect before
  (AGENTS.md, phone composition).

## Steps (PR A)

- [ ] **Step 0: Read and re-pin.** Read spec §4.4–4.8 and `_THEMES.md`. Invoke `frontend-design`.
  Confirm the undo fix is on `main` (`git log origin/main --oneline | grep -a "end card"`), then
  re-pin every line in Context against the merged tree. Record any premise that no longer holds.

- [ ] **Step 1: Write the failing registry tests** (`template-registry.test.ts`, after the `git mv`).
  1. `defaultTemplateFor` over **every** `OVERLAY_SPORT_KEYS` entry: cricket → `bar`, every other →
     `bug` (spec §3's rule, typed from the spec). Print `sports checked: 11`; fail on 0.
  2. A table through `resolveTemplate`, each row with its reason:
     `"bar"`/football → bar; `"bug"`/cricket → bug; `undefined`/cricket → bar; `""`/football → bug;
     `"nope"`/football → bug; `"slate"`/cricket → bar; `"toString"`/football → bug (`Object.hasOwn`).
  3. Through `resolveTemplateFrom` with an injected registry (the shipped one has no retired or
     restricted entry, so these branches are otherwise unreachable):
     - `old: { retired: { replacedBy: "bug" } }` → `bug`
     - a chain `a → b → bug` → `bug`
     - a cycle `a ↔ b` → the fallback
     - `retired: { replacedBy: "missing" }` → the fallback
     - `only: { sports: ["badminton"] }` requested for cricket → the fallback; for badminton → `only`
  4. `templatesForSportFrom`: hides the retired entry; offers `only` to badminton and not to
     cricket; keeps registry order.
  5. **Every sport's default is pickable:** for each `OVERLAY_SPORT_KEYS` sport,
     `templatesForSport(sport)` contains `defaultTemplateFor(sport)`, and that entry is not retired.
     Print the count (11); fail on 0. A retired default would hand every unset fixture a template the
     picker can't show.
  6. `templateParam`: `{template:"bug", style:"bar"}` → `"bug"`; `{style:"bar"}` → `"bar"`;
     `{template:"", style:"bar"}` → `"bar"`; `{template:["bug","bar"]}` → `"bug"`; `{}` → `undefined`.

- [ ] **Step 2: Write the failing extensibility test** (`template-extensibility.test.ts`, spec §5).
  Define a throwaway registry **in the test only**:
  ```ts
  const PROBE: OverlayTemplateDef<string> = {
    id: "probe", labelKey: "stream.tab.bug", sports: ["badminton"], component: OverlayBug,
    slabPlacement: "bug", sceneCardSafeArea: OVERLAY_TEMPLATES.bug.sceneCardSafeArea,
  };
  const REG: TemplateRegistry = { ...OVERLAY_TEMPLATES, probe: PROBE };
  const RETIRED: TemplateRegistry = { ...REG, gone: { ...PROBE, id: "gone", retired: { replacedBy: "probe" } } };
  ```
  Assert, with no other edit anywhere:
  - `templatesForSportFrom(REG, "badminton")` ends with `probe`; for cricket it doesn't contain it
  - `galleryMatrix(REG, OVERLAY_SPORT_KEYS, OVERLAY_SCENES.map(s => s.id), GALLERY_MOMENT_SAMPLES)`
    has exactly `OVERLAY_SCENES.length × 3` plain cells for `probe`, all badminton, plus one moment
    cell per badminton sample, and **no** cricket cell for `probe`. Compute the expected counts from
    the declarations (`OVERLAY_SCENES`, `GALLERY_SNAPSHOT_KINDS`, the samples filtered by sport),
    never from `galleryMatrix`'s output.
  - the readability sweep's cell list **is** `galleryMatrix` (the e2e renders it 1:1; step 9), so
    the probe is in the sweep. Assert the sweep's exported cell source is that function by identity.
  - retired resolution: `resolveTemplateFrom(RETIRED, "gone", "badminton", "bug").id === "probe"`
    (a stored choice); the page's resolution of `?template=gone` takes the same function (page test,
    step 4). `templatesForSportFrom(RETIRED, "badminton")` does not contain `gone`.

- [ ] **Step 3: Write the failing readability CSS scan** (`overlay-readability-css.test.ts`).
  Use `parseCss` / `readGlobalsCss` / `declaredValue` from
  `components/v2/scorepad/v3/__tests__/_globals-css.ts`.
  ```ts
  const TEXT_FLOOR_PX = 30;      // spec 2026-10-15 §4.5 (D9): no overlay text below 30 px at 1920×1080
  const BAR_BOTTOM_MIN_PX = 150; // spec §4.5: the bar's bottom clear of YouTube's controls (~14 %)
  ```
  - Every rule whose selector names an `.ovl-` class and declares `font-size`: the value must be
    `<n>px` with `n >= TEXT_FLOOR_PX`. Any other form (`em`, `rem`, `%`, `calc()`, `var()`, a `font:`
    shorthand in an `.ovl-` rule) **fails the test by name** ("unparsed font-size form"): an
    assumption is a guard, so the scan can't skip what it can't read. Print
    `font-size declarations checked: N`; fail on 0.
  - `--ovl-bar-bottom` on `.ovl-canvas` parses to px `>= BAR_BOTTOM_MIN_PX`.
  - Expect red at pin: 26 rules below the floor, and `--ovl-bar-bottom` at 54.

- [ ] **Step 4: Write the failing page and stage tests.**
  1. `page.test.ts` (re-pin its harness: `vi.mock` of the data reads, real `resolveTemplate`): for a
     football fixture,
     - `?template=bug&style=bar` → `data-style="bug"`
     - `?style=bar` → `data-style="bar"` (the alias)
     - `?template=nope&style=bar` → `data-style="bug"` (decision 2)
     - no params on cricket → `data-style="bar"`

     Anchor every assertion on `="` (AGENTS.md: an omitted prop serialises as `"$undefined"`).
  2. `overlay-slab-placement.test.tsx`: for **each** `OVERLAY_TEMPLATES` entry, render the stage with
     a moment and assert the slot's class is `ovl-moment-slot--${def.slabPlacement}`. Print
     `templates checked: 2`; fail on 0. Pin the two shipped values against the spec's placement
     (bar → `bar`, bug → `bug`) so a registry edit that swaps them goes red too. Move its
     bar-geometry pins to the lifted bottom: the expected numbers come from the CSS custom properties
     (`--ovl-bar-bottom`, `--ovl-bar-main-h`, `--ovl-bar-band-h`), never from a rendered value.
  3. `overlay-preview.test.tsx` (`renderToStaticMarkup`, node): for each template,
     `<OverlayPreview frameWidth={390} …/>` renders `data-testid="ovl-preview"`,
     `data-template="<id>"` and `data-style="<id>"`, and a wrapper scale of `390 / 1920`. With
     `moment` set, the slab text appears. Without it, no slab element renders.

- [ ] **Step 5: Write the failing gallery-data tests.**
  1. `gallery-matrix.test.ts`:
     - **Exact sample coverage:** every `(sport, ruleKey)` in `MOMENT_RULES` has at least one
       `GALLERY_MOMENT_SAMPLES` entry, and cricket's `cricket.ball` has three (six, four, wicket).
       Print `rule entries covered: 17, samples: 19`; fail if either is 0 or any entry is uncovered.
     - **Every sample fires:** `momentsFor(sample.sportKey, [sample.event], sample.event.seq - 1, msg, names)`
       returns at least one moment, using the real `en/public.json` dictionary for `msg`. Cricket's
       three samples must yield the six, four and out headlines (`overlay.moment.six`, `.four`, `.out`).
     - Matrix size equals the sum, over sports, of `templatesForSport(sport).length × OVERLAY_SCENES.length × 3`,
       plus the sum over samples of `templatesForSport(sample.sportKey).length`. Keys are unique.
  2. `gallery-snapshots.test.ts` (server, node; no DB): `gallerySnapshots()` returns all 11 sports ×
     3 kinds. Print `snapshots: 33`. Run each through the real `overlayModel` (en dictionary) and
     `slateStateOf` (`overlay-slate.tsx:18`), the card layer's own authority, and assert the result
     equals the kind's name: `warming` → `"warming"`, `live` → `"live"`, `ended` → `"ended"`. Also:
     every `live` sample has at least one side with a non-zero score in the model, and every `ended`
     sample has a non-null `outcome`. Fail on 0. (`ENDED_STATUSES`, `lib/overlay-model.ts:341`, is not
     exported; don't export it for this, because `slateStateOf` is the reader that matters.)

- [ ] **Step 6: Run, expect a FAIL.**
  `cd apps/web && npx vitest run --reporter=json --outputFile=/tmp/r2007-red.json <the 8 test paths>`, then
  `jq '{total: .numTotalTests, passed: .numPassedTests, failed: .numFailedTests}' /tmp/r2007-red.json`.
  Confirm every file **collected** (`.testResults[].name`), and that the failures are the missing
  exports and the 26 low rules, not a harness error.

- [ ] **Step 7: Implement the registry migration** (decisions 1–4). `git mv` both files; rename the
  exports; add `slabPlacement: "bar"` / `"bug"` to the two entries. Give each a **provisional**
  `sceneCardSafeArea` from today's CSS: bar `{ x: 72, y: 1080 − bottom − main − band, width: 1776, height: main + band }`
  from the three custom properties; bug `{ x: 60, y: 54, width: 480, height: 972 }`, a deliberately
  loose bound that step 10 tightens from measurement. Delete `slabPlacementFor` and
  read `OVERLAY_TEMPLATES[props.style].slabPlacement` in the stage; the page reads
  `templateParam({ template, style })` and adds `template` to its query type. Update every caller's
  import (Context table). Leave the panel's and capture-phone's **behaviour** untouched: R2006 owns
  the panel, and `?style=` is the alias capture-phone keeps.

- [ ] **Step 8: Implement scenes, the preview, and the gallery data.**
  - `overlay-scenes.ts` as in Interfaces.
  - The stage's `snapshotOnly` passes `fetcher: async () => props.initial` to `useLiveFixture`.
    `pinnedMoment`, when set, renders in the slot instead of the queue's moment. **Don't touch
    `use-live-fixture.ts`**: it is shared with `MatchCentre`.
  - `OverlayPreview` lifts the panel's canvas-in-an-aspect-ratio-strip pattern
    (`fixture-stream-panel.tsx:566-610`) and renders `<OverlayStage realtime={false} snapshotOnly …/>`.
    It is the **only** preview path: the gallery uses it now, and R2006's picker must use it.
  - `gallery-matrix.ts` and `GALLERY_MOMENT_SAMPLES` (decision 7). Each sample's `event` is a
    `RecentEvent` carrying exactly the fields its rule reads (re-pin `server/overlay/recent.ts` for the
    shape, `derived.setWon` / `derived.pointState` included).
  - `gallery-snapshots.ts` (decision 6): per sport, `resolveModule` + the sport's default cfg (re-pin
    how `loadFoldInputs` builds `cfg` with `resolveFixtureCfg`), lineups where the sport needs them,
    and a scripted envelope list. `warming` folds nothing (`folded: null`). `live` folds the script's
    prefix. `ended` folds the whole script, which must decide the match by **play**, not by an award;
    pick the shortest legal cfg the sport declares, such as a short cricket innings. Project each
    through `projectOverlayLiveData` with `GALLERY_SIDES`.

  Run steps 1, 2, 4 and 5's files. Expect a PASS; paste the counts.

- [ ] **Step 9: The readability redesign** (D9 + D11; `frontend-design`; house theme only).
  - Lift every sub-30 px `.ovl-` font-size to ≥ 30 px. Numerals keep their larger sizes. Use only
    `overlay-tokens.ts`, `--sport-led`, `SPORT_PALETTES` (`components/v2/scorepad/v3/sport-theme.ts:165`),
    the stadium-night plate (`--mk-night`, `globals.css:535`) and the bundled fonts. The mockups in
    `.superpowers/brainstorm/` are layout references only.
  - Set `--ovl-bar-bottom: 162px` (decision 5). Every rule derived from it (the moment slot, the slate
    content padding) follows the variable; re-pin each.
  - Run the CSS scan (step 3): PASS. Paste `font-size declarations checked: N`.

- [ ] **Step 10: The gallery page and its e2e** (1280 only: `/admin` exemption, owner 2026-09-28).
  - `page.tsx` (server): `await requireStaff()`; `searchParams` `sport` (one of `OVERLAY_SPORT_KEYS`;
    absent or unknown → all) and `frame` (`full` → 960 px wide, `844` → 844 × 475, `390` → 390 × 219,
    spec §4.5; absent or unknown → `full`). Build `gallerySnapshots()` and
    `galleryMatrix(OVERLAY_TEMPLATES, …)`, and pass plain data to the client component.
  - `overlay-gallery.tsx`: one `OverlayPreview` per cell, over a checkerboard so transparency shows.
    Each cell root carries `data-gallery-cell=<key>`, `data-template`, `data-sport`, `data-scene`,
    `data-snapshot`, `data-moment`, `data-slab-placement` (the def's) and `data-safe-area` (the def's
    box as JSON). The page root carries `data-cell-count`.
  - Admin nav: `t(dict, "admin.overlayGallery.nav")` beside capture-records.
  - Copy (`ui.json`, all 4 locales, translated properly in the dictionary's tone; then
    `pnpm i18n:gen-keys`):

    | Key | English |
    |---|---|
    | `admin.overlayGallery.nav` | "Overlay gallery" |
    | `admin.overlayGallery.title` | "Overlay templates" |
    | `admin.overlayGallery.sport` | "Sport" |
    | `admin.overlayGallery.allSports` | "All sports" |
    | `admin.overlayGallery.frame` | "Frame" |
    | `admin.overlayGallery.frameFull` | "Full HD" |
    | `admin.overlayGallery.framePhoneLandscape` | "Phone, landscape (844 × 475)" |
    | `admin.overlayGallery.framePhonePortrait` | "Phone, portrait (390 × 219)" |
    | `admin.overlayGallery.snapshot.warming` | "Before the match" |
    | `admin.overlayGallery.snapshot.live` | "Live" |
    | `admin.overlayGallery.snapshot.ended` | "Result" |
    | `admin.overlayGallery.moments` | "Moments" |
    | `stream.scene.live` | "Live" |

  - **Write the failing e2e first** (`apps/web/e2e/overlay-gallery.spec.ts`; staff set-up copied from
    `capture-records-admin.spec.ts`; Desktop Chrome 1280). Every check prints its count, and 0 fails
    it:
    1. **Render fidelity:** the number of `[data-gallery-cell]` equals `data-cell-count`, which is
       > 0. Every cell contains one `[data-testid="ovl-root"]`. (The matrix size itself is pinned by
       step 5's unit test from the declarations.)
    2. **Text floor, computed:** in each cell, every element with its own non-whitespace text node and
       `checkVisibility()` true has `parseFloat(getComputedStyle(el).fontSize) >= 30`. A CSS
       `transform` doesn't change computed font-size, so this reads canvas px. Report
       `cells: M, text elements checked: N` and list each violator by cell key, class and size.
    3. **Bar lift:** in every `bar` cell, `(canvas.bottom − bar.bottom) / scale >= 150`, where
       `scale = canvasRect.width / 1920`.
    4. **Safe area:** in every cell, the template element's box (canvas px) lies inside
       `data-safe-area`. In every `warming` and `ended` cell, the match card's box does not intersect
       it. This is where the Context hypothesis about the card and the bug is settled. Record the
       answer in the PR body.
    5. **Slab placement:** in every moment cell, the slot's class is
       `ovl-moment-slot--<data-slab-placement>`, and the slot holds a slab.
    6. **No transport:** over 16 s after load (longer than `POLL_MS`), zero requests match
       `/api/v1/public/fixtures/` and zero websocket opens to the realtime host. `snapshotOnly` is
       what makes this true.
    7. `?frame=844` and `?frame=390`: each preview's box width equals the frame (±1 px).
  - Run, expect a FAIL; implement; run, expect a PASS. Run it alone from `apps/web` with
    `BASE=http://localhost:<port>` against a prod build (the `seazn-local-env` skill).
  - **Tighten each `sceneCardSafeArea`:** check 4 logs the union box of the template element over
    every sport × snapshot. Round it outward to whole px and write it into the registry, replacing
    the provisional box from step 7. Re-run: check 4 now pins the declared box.

- [ ] **Step 11: The existing overlay e2e, by route.** `grep -arln "/overlay/fixtures" apps/web/e2e`.
  Extend `stream-overlay.spec.ts`: `?template=bug` on a cricket fixture shows the bug, and
  `?style=bar` still works. Run the **whole** file, never a `-g` slice (AGENTS.md class 21). All
  green.

- [ ] **Step 12: Smoke.** In `scripts/smoke.ts`'s `streamOverlaySuite`, add
  `GET /overlay/fixtures/<id>?template=bug` → 200 with `data-style="bug"` (anchored on `="`), and
  `?style=bar` → `data-style="bar"`. `streamOverlay` is not in `SELECTABLE_SUITES` (`scripts/smoke.ts:21789`)
  at pin, so register `streamOverlay: () => streamOverlaySuite()` there, then run it alone:
  `SMOKE_ONLY=streamOverlay pnpm test:smoke` against the local prod build (never the full smoke).

- [ ] **Step 13: Mutate once each.** Restore each from a `cp` backup, never `git checkout <file>`.

  | # | Mutant | Expected red |
  |---|---|---|
  | 1 | `.ovl-bug-status` font-size 29px (the spec's own mutant) | CSS scan (step 3) and e2e check 2 |
  | 2 | `--ovl-bar-bottom: 149px` | CSS scan and e2e check 3 |
  | 3 | the stage reads `"bar"` instead of `def.slabPlacement` (spec mutant) | step 4.2 and e2e check 5 |
  | 4 | `templateParam` ignores `style` | page test `?style=bar` |
  | 5 | `templateParam` prefers `style` | page test `?template=bug&style=bar` |
  | 6 | `templatesForSportFrom` stops hiding retired | step 1.4 and the extensibility test |
  | 7 | `galleryMatrix` iterates `OVERLAY_TEMPLATES` instead of its argument | extensibility test |
  | 8 | `OverlayPreview` drops `snapshotOnly` | e2e check 6 |

  Record all 8 in the commit body.

- [ ] **Step 14: Owner visual review.** Screenshots at 1280 of the gallery, at `frame=full`, `844`
  and `390`, cropped per template × sport. Post them in the PR comment, **never committed**
  (owner 10-09). Get a per-screen verdict on each (AGENTS.md class 11). The checks pass only if the
  images exist and differ between frames (class 10).

- [ ] **Step 15: Scoped gate, commit, PR.** Touched unit files (JSON reporter, counts pasted), the two
  e2e files, `rtk proxy pnpm lint` on touched paths (read `✖ N problems`), and the typecheck.
  `feat(overlay): template registry, readability floor and the staff overlay gallery`.

## PR B: the owed R2002 contract follow-up (spec §6 last row)

The compositor reports `programmePhase = final` during the final hold (C06), and the page reports
which phase channel is live (`phaseChannel`, spec §4.3, R2005). `RelayHeartbeat` is **strict**, with
`programmePhase: z.enum(["waiting", "live", "frozen", "reconnecting"]).nullish()`
(`server/api-v1/schemas.ts:1687`). A beat carrying `final` is a **400 today**, and that includes the
final exit beat (owner ruling 2026-10-15), so the exit reason would be lost. This PR merges, and C07
re-vendors, before any compositor build emits either value.

Pins: contract `docs/contracts/relay-job.v1.json:294`; generator `scripts/gen-relay-contracts.ts:84`;
`server/relay/sanitise.ts:27` (`ALLOWED_KEYS`); fixtures `docs/contracts/fixtures/relay-job.v1/`
(`heartbeat-full.json` uses `frozen`); `relay-contract.test.ts` (sha table);
`relay-internal-routes.test.ts`.

**Files:**
- Create: `apps/web/src/lib/relay-phase.ts`. **Import-free**, because `schemas.ts` may import it only
  by relative `.ts` path (the OpenAPI generator bans `@/`).
  ```ts
  /** The compositor's programme phases, as the heartbeat and the loopback endpoint send them
   *  (compositor C03 + C06; overlay templates spec 2026-10-15 §4.2). */
  export const PROGRAMME_PHASES = ["waiting", "live", "frozen", "reconnecting", "final"] as const;
  export type ProgrammePhaseWire = (typeof PROGRAMME_PHASES)[number];
  /** Which channel the overlay page reads the phase from (spec §4.3). */
  export const PHASE_CHANNELS = ["loopback", "web"] as const;
  /** Spec §4.3: the compositor's loopback phase endpoint. Unprivileged (> 1023) and below Linux's
   *  default ephemeral range (32768–60999), so no outbound socket is ever handed this port first. */
  export const RELAY_PHASE_PORT = 7419;
  export const RELAY_PHASE_PATH = "/phase";
  /** Spec §4.3: a long-poll is held at most this long. */
  export const RELAY_PHASE_LONG_POLL_MAX_MS = 10_000;
  ```
- Modify: `server/api-v1/schemas.ts`. `programmePhase: z.enum(PROGRAMME_PHASES).nullish()`;
  `phaseChannel: z.enum(PHASE_CHANNELS).nullish()`; new
  `RelayPhaseReply = z.object({ phase: z.enum(PROGRAMME_PHASES), seq: z.number().int().min(0) }).strict()`
  and `RelayPhaseEndpoint = z.object({ port: z.literal(RELAY_PHASE_PORT), path: z.literal(RELAY_PHASE_PATH), longPollMaxMs: z.literal(RELAY_PHASE_LONG_POLL_MAX_MS) }).strict()`.
- Modify: `scripts/gen-relay-contracts.ts`: `$defs.phaseReply` and `$defs.phaseEndpoint`; the
  `programmePhase` description names `final` ("the final hold, C06"). Regenerate
  `docs/contracts/relay-job.v1.json` and OpenAPI (the same places `exitReason` went).
- Modify: `server/relay/sanitise.ts` `ALLOWED_KEYS` + `phaseChannel`, and its "no allowed key names a
  credential" pin.
- Create fixtures: `heartbeat-final-hold.json` (`state: "playing"`, `programmePhase: "final"`,
  `phaseChannel: "loopback"`), `heartbeat-invalid-phase-channel.json` (`phaseChannel: "wifi"`),
  `phase-reply-final.json` (`{ "phase": "final", "seq": 5 }`) and
  `phase-reply-invalid-unknown-field.json`.

**Tests (failing first):**
1. `relay-contract.test.ts`: the sha matches the regenerated file; the new fixtures parse or reject by
   name; **every** `PROGRAMME_PHASES` value parses inside `RelayHeartbeat` (print
   `phases checked: 5`; fail on 0); `"paused"` is rejected; the committed contract's
   `$defs.phaseEndpoint.properties.port.const` equals `RELAY_PHASE_PORT`.
2. `relay-internal-routes.test.ts`: POST `heartbeat-final-hold.json` through the real route with a
   job token. `programmePhase: "final"` and `phaseChannel: "loopback"` round-trip in **both** stores
   (`last_heartbeat` and the newest `fixture_stream_samples.raw`). `heartbeat-minimal.json` still
   leaves `phaseChannel` an **absent key** in both.
3. **Mutants:** drop `"final"` from `PROGRAMME_PHASES` (expected red: test 2's 200 becomes a 400 and
   the sweep fails); remove `phaseChannel` from `ALLOWED_KEYS` (expected red: the samples half of test 2).

**Commit, PR:** `feat(relay): the programme phase gains final, and the beat carries its phase channel`.
The PR body gives the contract sha256 and the web commit sha that C07 pins.

## Acceptance

- PR A: the registry migrated, with `?template=` and `?style=` alias resolution, retired → replacement,
  and unknown → default, all unit-pinned; the extensibility probe appears in the picker list and the
  gallery matrix with no other edit; the CSS scan and the computed sweep pass with non-zero counts;
  `/admin/overlay-gallery` renders every cell at 1280; the existing overlay e2e is green and
  unchanged apart from the new cases; smoke extended; 8 mutants red; owner verdicts per screen.
- PR B: merged before C07 vendors; the contract sha is in the PR body; 2 mutants red.
- The false premises and the resolved ambiguities (decisions 2–4, the card/bug answer) are in the PR
  bodies, and in compositor `docs/_INDEX.md` through the orchestrator.

## Do not touch

The stream panel's behaviour (R2006: the OBS URL stays `?style=` here), the scene layer and the phase
channel (R2005), relay usecases (R2004), `use-live-fixture.ts`, and C04/C05.

## Output cap

Under 15 lines: both PR URLs, test counts, the sweep counts, mutants, the contract sha, deviations.
