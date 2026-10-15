# R2007: Overlay template system, readability floor, and the staff gallery

**Repo:** `onryde/seazn.club`. Two PRs from one brief:

- **PR A (the template system):** worktree
  `git worktree add ../seazn.club-wt/r2007-overlay-templates -b feat/overlay-template-system origin/main`.
  **Depends on:** the end-card-undo fix (branch `fix/overlay-end-card-undo`) **merged to `main`**.
  (Corrected 2026-10-15 review: the fix does not touch `overlay-stage.tsx`.) At `e9356a944` it changes
  `use-live-fixture.ts`, `e2e/stream-overlay.spec.ts` and `e2e/overlay-kit.ts`, plus tests. This task
  extends `stream-overlay.spec.ts` (step 11), and `snapshotOnly` relies on the hook's fetcher
  semantics, which the fix changes. So start PR A only after the fix lands, and re-pin from the merged
  `main`.
- **PR B (the owed R2002 contract follow-up, §"PR B" below):** worktree
  `git worktree add ../seazn.club-wt/r2007-relay-final-phase -b feat/relay-contract-final-phase origin/main`.
  **Does not wait for the undo fix**: it touches no `components/overlay/**` file. It must merge
  **before C07 vendors the contract**. **(Corrected 2026-10-15, briefs review) Branch it after R2003
  (#967) merges.** #967 also edits `server/api-v1/schemas.ts`, `openapi/v1.json` and
  `relay-internal-routes.test.ts` (+340 lines), all three of which PR B edits. Rebase onto the merged
  `main` and regenerate OpenAPI and the contract there.

**Use the `frontend-design` skill** (house rule for every UI task; put the invocation in the dispatch).
**Spec (authority):** `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
(owner-approved 2026-10-15; branch `docs/overlay-templates-and-scenes` @ `29fa9f59c`). This task covers
§4.4–4.7 and the gallery and render-function part of §4.8. Scenes themselves (§4.1–4.3, the rest of
§4.8) are R2005. The picker is R2006. Also read `docs/superpowers/specs/2026-09-05-stream-overlay-prompts/_THEMES.md`.

## Context

Pinned 2026-10-15 at `seazn.club` `origin/main` `b01d07cd5`. **Re-pin everything after the undo fix
merges.** It changes `use-live-fixture.ts` and `stream-overlay.spec.ts`; the stage pins below should
hold, but confirm them.

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

**The card overlaps the bug today (review 2026-10-15, from the CSS).** `.ovl-slate-content`
(`globals.css:1702-1706`) is `inset: 0` with padding `72px 96px <bar stack + 24px>`. It centres a card
up to 1100 px wide (x 410–1510) and clears only the **bar**. The bug sits at x 60–540 from y 54, and it
paints over the card (z-index 3 vs 1), so nobody noticed. The 30 px floor makes the bug wider. That
geometry is bar-only, so a new template's `sceneCardSafeArea` could never move a card, which breaks
spec §4.7's "nothing else changes". **Step 9b replaces it:** cards are laid out from the registry
entry's box (coordinator decision G2).

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
    /** The box scene and match cards must keep clear. DECLARED from the template's CSS layout rule (step 7,
     *  re-declared in step 9), never copied from a measurement. The stage lays every card out from it
     *  (`cardRegionFor`, step 9b), and the e2e proves the rendered template stays inside it. */
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
  /** Step 9b (coordinator decision G2). The canvas region where scene and match cards are laid out: the
   *  largest, by area, of the four inset-canvas strips beside `box` (right, above, left, below; ties in that
   *  order), kept `CARD_CLEARANCE_PX` clear of it. `null` when none is at least CARD_MIN_W × CARD_MIN_H,
   *  in which case no card renders. */
  export const CARD_INSET = { top: 72, right: 96, bottom: 72, left: 96 } as const; // today's .ovl-slate-content padding, globals.css:1702-1706
  export const CARD_CLEARANCE_PX = 24;   // today's gap above the bar stack, globals.css:1705
  export const CARD_MIN_W = 640;         // design rule: a third of the canvas width
  export const CARD_MIN_H = 360;         // design rule: a third of the canvas height
  export function cardRegionFor(box: CanvasBox): CanvasBox | null;

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
  /** A Result-card variant other than "decided by play" (void, walkover, abandoned, …; review m11). */
  export interface ResultVariant { status: string; sportKey: string }
  /** The two sports result variants are drawn for: cricket (the bar default, the longest card text) and
   *  badminton (the bug default). The card layout is sport-agnostic apart from the score line, so two
   *  sports cover both templates' defaults. */
  export const GALLERY_RESULT_VARIANT_SPORTS = ["cricket", "badminton"] as const;
  export interface GalleryCell { key: string; templateId: string; sportKey: string; scene: SceneId; snapshot: GallerySnapshotKind; momentSampleId: string | null; resultStatus: string | null }
  /** Plain cells: every non-retired template suiting the sport × sport × scene × snapshot kind.
   *  Moment cells: every non-retired template suiting the sample's sport × sample (scene "live", snapshot "live").
   *  Variant cells: every non-retired template suiting the variant's sport × variant (scene "live", snapshot "ended"). */
  export function galleryMatrix(registry: TemplateRegistry, sports: readonly string[], scenes: readonly SceneId[], samples: readonly MomentSample[], variants: readonly ResultVariant[]): GalleryCell[];
  /** The gallery's ONE cell source (review I3): `galleryMatrix` over the shipped sports, scenes and samples.
   *  The page calls it with `OVERLAY_TEMPLATES`, and the extensibility test with a probe registry. */
  export function galleryCells(registry: TemplateRegistry, variants: readonly ResultVariant[]): GalleryCell[];

  // server/overlay/gallery-snapshots.ts (server-only)
  export const GALLERY_SIDES: readonly [OverlaySideInput, OverlaySideInput];
  export function gallerySnapshots(): Record<string /* OVERLAY_SPORT_KEYS */, Record<GallerySnapshotKind, OverlayLiveData>>;
  /** Every fixture status whose Result card is not "decided by play", for each GALLERY_RESULT_VARIANT_SPORTS sport. */
  export function galleryResultVariants(): { variant: ResultVariant; snapshot: OverlayLiveData }[];
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
  7. **`cardRegionFor`** (step 9b; coordinator decision G2). Expected values come from the rule and
     the constants, never from a call:
     - a top-left box `{ x: 60, y: 54, width: 480, height: 600 }` → the right strip:
       `x = 60 + 480 + CARD_CLEARANCE_PX`, `y = CARD_INSET.top`, `width = 1920 − CARD_INSET.right − x`,
       `height = 1080 − CARD_INSET.top − CARD_INSET.bottom`
     - a bottom strip `{ x: 72, y: 741, width: 1776, height: 177 }` → the strip above:
       `x = CARD_INSET.left`, `y = CARD_INSET.top`, `width = 1920 − CARD_INSET.left − CARD_INSET.right`,
       `height = 741 − CARD_CLEARANCE_PX − CARD_INSET.top`
     - the full canvas `{ x: 0, y: 0, width: 1920, height: 1080 }` → `null` (the guard; the stage then
       renders no card)
     - for **every** `OVERLAY_TEMPLATES` entry, the region is non-null and does not intersect the
       entry's box. Print `templates checked: 2`; fail on 0.

- [ ] **Step 2: Write the failing extensibility test** (`template-extensibility.test.ts`, spec §5).
  Define a throwaway registry **in the test only**:
  ```ts
  const PROBE: OverlayTemplateDef<string> = {
    id: "probe", labelKey: "stream.tab.bug", sports: ["badminton"], component: OverlayBug,
    // Its own box (top-right), so the test can prove a new template moves the cards with no other edit.
    slabPlacement: "bug", sceneCardSafeArea: { x: 1380, y: 54, width: 480, height: 600 },
  };
  const REG: TemplateRegistry = { ...OVERLAY_TEMPLATES, probe: PROBE };
  const RETIRED: TemplateRegistry = { ...REG, gone: { ...PROBE, id: "gone", retired: { replacedBy: "probe" } } };
  ```
  Assert, with no other edit anywhere:
  - `templatesForSportFrom(REG, "badminton")` ends with `probe`; for cricket it doesn't contain it
  - `galleryCells(REG, VARIANTS)`, with `VARIANTS = [{ status: "walkover", sportKey: "badminton" }]`
    typed in the test, has exactly `OVERLAY_SCENES.length × GALLERY_SNAPSHOT_KINDS.length` plain cells
    for `probe`, all badminton, plus one moment cell per badminton sample, plus one variant cell, and
    **no** cricket cell for `probe`. Compute the expected counts from the declarations
    (`OVERLAY_SCENES`, `GALLERY_SNAPSHOT_KINDS`, the samples filtered by sport, `VARIANTS`), never from
    `galleryCells`'s output.
  - **The sweep's cell source is `galleryCells` (review I3).** Call the gallery page's default export
    directly. It is an async server component, so `await Page({ searchParams: Promise.resolve({}) })`
    returns a React element without rendering. Do this with `vi.mock` of the template-registry module
    (importOriginal, `OVERLAY_TEMPLATES` replaced by `REG`), of `requireStaff` (re-pin its module;
    resolves) and of `galleryResultVariants` (returns `[]`). Find the `<OverlayGallery>` element in the
    returned tree and assert its `cells` prop includes the probe's badminton cells and equals
    `galleryCells(REG, [])`. The e2e renders those cells 1:1 (step 10), so the probe is in every sweep.
  - **A new box moves the cards:** `cardRegionFor(PROBE.sceneCardSafeArea)` does not intersect the
    probe's box and differs from `cardRegionFor(OVERLAY_TEMPLATES.bug.sceneCardSafeArea)`.
  - retired resolution: `resolveTemplateFrom(RETIRED, "gone", "badminton", "bug").id === "probe"`
    (a stored choice). The page's resolution of `?template=gone` takes the same function (page test
    4.1, retired row). `templatesForSportFrom(RETIRED, "badminton")` does not contain `gone`.

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
  - **The card layout consumes the registry (G2):** `.ovl-slate-content` declares `left`, `top`,
    `width` and `height` as `var(--ovl-card-x)`, `var(--ovl-card-y)`, `var(--ovl-card-w)` and
    `var(--ovl-card-h)`, and declares neither `inset: 0` nor a padding that names `--ovl-bar-`. That
    is a guard: a card centred on the whole canvas is the defect.
  - Expect red at pin: 26 rules below the floor, `--ovl-bar-bottom` at 54, and the card rule at
    `inset: 0`.

- [ ] **Step 4: Write the failing page and stage tests.**
  1. `page.test.ts` (re-pin its harness: `vi.mock` of the data reads, real `resolveTemplate`): for a
     football fixture,
     - `?template=bug&style=bar` → `data-style="bug"`
     - `?style=bar` → `data-style="bar"` (the alias)
     - `?template=nope&style=bar` → `data-style="bug"` (decision 2)
     - no params on cricket → `data-style="bar"`
     - **retired row (review m5b):** `vi.mock` the template-registry module (importOriginal) so that
       `OVERLAY_TEMPLATES` gains `gone: { …bug, id: "gone", retired: { replacedBy: "bug" } }` and
       `resolveTemplate` is the real `resolveTemplateFrom` over that registry. Then
       `?template=gone` → `data-style="bug"`, so an OBS URL with a retired id keeps working.

     Anchor every assertion on `="` (AGENTS.md: an omitted prop serialises as `"$undefined"`).
  2. `overlay-slab-placement.test.tsx`: for **each** `OVERLAY_TEMPLATES` entry, render the stage with
     a moment and assert the slot's class is `ovl-moment-slot--${def.slabPlacement}`. Print
     `templates checked: 2`; fail on 0. Pin the two shipped values against the spec's placement
     (bar → `bar`, bug → `bug`) so a registry edit that swaps them goes red too. Move its
     bar-geometry pins to the lifted bottom: the expected numbers come from the CSS custom properties
     (`--ovl-bar-bottom`, `--ovl-bar-main-h`, `--ovl-bar-band-h`), never from a rendered value.
  2b. **Card variables (G2):** for each `OVERLAY_TEMPLATES` entry, the stage's `.ovl-canvas` element
     carries the inline custom properties `--ovl-card-x/y/w/h` equal to `cardRegionFor(def.sceneCardSafeArea)`,
     and `--ovl-safe-x/y/w/h` equal to the def's box, in px. The expected values are the def's own box,
     taken from the registry declaration. Print `templates checked: 2`.
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
       plus the sum over samples of `templatesForSport(sample.sportKey).length`, plus the sum over the
       variants passed of `templatesForSport(variant.sportKey).length`. Keys are unique.
  2. `gallery-snapshots.test.ts` (server, node; no DB): `gallerySnapshots()` returns all 11 sports ×
     3 kinds. Print `snapshots: 33`. Run each through the real `overlayModel` (en dictionary) and
     `slateStateOf` (`overlay-slate.tsx:18`), the card layer's own authority, and assert the result
     equals the kind's name: `warming` → `"warming"`, `live` → `"live"`, `ended` → `"ended"`. Also:
     - every `ended` sample has a non-null `outcome`
     - **(corrected by the briefs review)** every `live` sample of a sport with at least one `MOMENT_RULES`
       entry has at least one side with a non-zero score in the model. Those sports score as they go,
       which is why they have moment rules. Boardgame, carrom and generic (empty `MOMENT_RULES` at pin)
       can have no running score before their result, so for **every** sport the `live` model carries
       both `GALLERY_SIDES` names. Derive the score-checked set from `MOMENT_RULES`, never a typed list.
       Print `live samples score-checked: N, name-checked: 11`; fail if either is 0.

     Fail on 0. (`ENDED_STATUSES`, `lib/overlay-model.ts:341`, is not exported; don't export it for
     this, because `slateStateOf` is the reader that matters.)
  3. **Result variants (review m11)**, in `gallery-snapshots.test.ts`. `galleryResultVariants()`:
     - Re-pin the fixture-status union (the `fixtures.status` type and its check constraint). Keep the
       statuses whose projection `slateStateOf` reads `"ended"`, other than the one `ended` already
       covers (decided by play).
     - Produce each through the real producers. Re-pin how each status gets its outcome: for example,
       a walkover is an award folded from the engine's forfeit event through `foldFrom`, while void,
       abandoned and `needs_decision` come from the row's status in `projectOverlayLiveData`.
     - For each variant, assert `slateStateOf(overlayModel(..)) === "ended"`.
     - Print `result variants: N (statuses: …)`; fail on 0. At pin, expect void, walkover, abandoned
       and `needs_decision` (review m11); a status the re-pin finds is added, not skipped.

- [ ] **Step 6: Run, expect a FAIL.**
  `cd apps/web && npx vitest run --reporter=json --outputFile=/tmp/r2007-red.json <the 8 test paths>`, then
  `jq '{total: .numTotalTests, passed: .numPassedTests, failed: .numFailedTests}' /tmp/r2007-red.json`.
  Confirm every file **collected** (`.testResults[].name`), and that the failures are the missing
  exports and the 26 low rules, not a harness error.

- [ ] **Step 7: Implement the registry migration** (decisions 1–4). `git mv` both files; rename the
  exports; add `slabPlacement: "bar"` / `"bug"` to the two entries. **Declare** each
  `sceneCardSafeArea` from the template's CSS layout rule, with the derivation as a comment beside the
  numbers (review m1: a box copied from a measurement passes check 4 by construction, so it's a
  ratchet, not an oracle):
  - bar: `{ x: 72, y: 1080 − bottom − main − band, width: 1776, height: main + band }`, from the three
    custom properties and `.ovl-bar`'s left/right 72
  - bug: `{ x: 60, y: 54, width: <.ovl-bug width>, height: <header + the most data rows any sport's bug
    shows × row height + footer> }`. Re-pin each term from the bug's CSS and the sport with the most
    rows (sets or innings).

  Step 9 re-declares both from the redesigned CSS. Delete `slabPlacementFor` and
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
  - `gallery-matrix.ts`: `galleryMatrix`, and `galleryCells(registry, variants)`, which is
    `galleryMatrix(registry, OVERLAY_SPORT_KEYS, OVERLAY_SCENES.map(s => s.id), GALLERY_MOMENT_SAMPLES, variants)`.
    `GALLERY_MOMENT_SAMPLES` (decision 7): each sample's `event` is a
    `RecentEvent` carrying exactly the fields its rule reads (re-pin `server/overlay/recent.ts` for the
    shape, `derived.setWon` / `derived.pointState` included).
  - `gallery-snapshots.ts` (decision 6): per sport, `resolveModule` + the sport's default cfg (re-pin
    how `loadFoldInputs` builds `cfg` with `resolveFixtureCfg`), lineups where the sport needs them,
    and a scripted envelope list. `warming` folds nothing (`folded: null`). `live` folds the script's
    prefix. `ended` folds the whole script, which must decide the match by **play**, not by an award;
    pick the shortest legal cfg the sport declares, such as a short cricket innings. Project each
    through `projectOverlayLiveData` with `GALLERY_SIDES`. `galleryResultVariants()` as step 5.3.

  Run steps 1, 2, 4 and 5's files. Expect a PASS, except step 1.7, step 2's card leg, step 4.2b and
  the CSS card guard, which go green in step 9b. Paste the counts.

- [ ] **Step 9: The readability redesign** (D9 + D11; `frontend-design`; house theme only).
  - Lift every sub-30 px `.ovl-` font-size to ≥ 30 px. Numerals keep their larger sizes. Use only
    `overlay-tokens.ts`, `--sport-led`, `SPORT_PALETTES` (`components/v2/scorepad/v3/sport-theme.ts:165`),
    the stadium-night plate (`--mk-night`, `globals.css:535`) and the bundled fonts. The mockups in
    `.superpowers/brainstorm/` are layout references only.
  - Set `--ovl-bar-bottom: 162px` (decision 5). Every rule derived from it (the moment slot, the slate
    content padding) follows the variable; re-pin each.
  - **Re-declare both `sceneCardSafeArea` boxes** from the redesigned CSS (the bug grows with the
    floor), with the derivation comment updated. Never copy a measured box in.
  - Run the CSS scan (step 3): the font-size and bar legs PASS. Paste `font-size declarations checked: N`.

- [ ] **Step 9b: Cards are laid out from the registry (coordinator decision G2; `frontend-design`).**
  - `cardRegionFor` in `template-registry.ts`, as in Interfaces.
  - The stage writes, on `.ovl-canvas`, the inline custom properties `--ovl-safe-x/y/w/h` (the def's
    box) and `--ovl-card-x/y/w/h` (`cardRegionFor(def.sceneCardSafeArea)`), in px. A `null` region
    renders no match card (the guard; step 1.7 reaches it with an injected box).
  - `.ovl-slate-content` becomes `position: absolute; left: var(--ovl-card-x); top: var(--ovl-card-y);
    width: var(--ovl-card-w); height: var(--ovl-card-h)`, centring the card inside that region. Drop
    `inset: 0` and the bar-stack padding. `.ovl-slate-card` keeps its `max-width: 1100px` and
    `max-height: 100%`, so a narrow region shrinks it.
  - R2005's scene cards render inside the same `.ovl-slate-content` wrapper, so they inherit this
    layout and check 4 with no gallery edit.
  - Run step 1.7, the extensibility test, step 4.2b and the CSS scan: PASS. Owner screen: the card
    beside the bug, and the card above the bar (step 14).

- [ ] **Step 10: The gallery page and its e2e** (1280 only: `/admin` exemption, owner 2026-09-28).
  - `page.tsx` (server): `await requireStaff()`; `searchParams` `sport` (one of `OVERLAY_SPORT_KEYS`;
    absent or unknown → all) and `frame` (`full` → 960 px wide, `844` → 844 × 475, `390` → 390 × 219,
    spec §4.5; absent or unknown → `full`). Build `gallerySnapshots()`, `galleryResultVariants()` and
    `galleryCells(OVERLAY_TEMPLATES, variants.map(v => v.variant))` (the one cell source; step 2), and
    pass plain data to the client component `OverlayGallery`: `cells`, the snapshots, and the variant
    snapshots keyed `${status}:${sportKey}`. A variant cell (`resultStatus` non-null) renders its
    variant's snapshot, and a cell whose key finds no snapshot throws (a guard, so a cell is never
    rendered from the wrong data).
  - `overlay-gallery.tsx`: one `OverlayPreview` per cell, over a checkerboard so transparency shows.
    Each cell root carries `data-gallery-cell=<key>`, `data-template`, `data-sport`, `data-scene`,
    `data-snapshot`, `data-moment`, `data-result-status`, `data-slab-placement` (the def's) and
    `data-safe-area` (the def's box as JSON). The page root carries `data-cell-count`.
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
    4. **Safe area (rewritten; coordinator decision G2 / review I2):**
       - In every cell, the template element's box (canvas px) lies inside `data-safe-area`.
       - The card leg is keyed on **DOM presence, not on the snapshot kind**: in every cell containing
         `[data-testid="ovl-slate-card"]`, the card's box does not intersect `data-safe-area`. That
         includes R2005's scene cards (Reconnecting, Stream ended), which use the same element.
       - The sweep covers every scene × every template × the `live` and `ended` snapshots (and
         `warming`, and the result variants), because the cells are `galleryCells`. When R2005 appends
         its scenes, this check covers them with no edit.
       - Anti-vacuity: every `warming`, `ended` and variant cell must contain a card. Print
         `cells: M, cards checked: K, cards expected ≥ W` (W from the cell list); fail if K is 0 or
         K < W.
       - A failure is fixed in the template's CSS or by re-declaring the box with its design reason,
         **never** by writing a measured box in.
    5. **Slab placement:** in every moment cell, the slot's class is
       `ovl-moment-slot--<data-slab-placement>`, and the slot holds a slab.
    6. **No transport:** over 16 s after load (longer than `POLL_MS`), zero requests match
       `/api/v1/public/fixtures/` and zero websocket opens to the realtime host. `snapshotOnly` is
       what makes this true.
    7. `?frame=844` and `?frame=390`: each preview's box width equals the frame (±1 px).
  - Run, expect a FAIL; implement; run, expect a PASS. Run it alone from `apps/web` with
    `BASE=http://localhost:<port>` against a prod build (the `seazn-local-env` skill).
  - (Superseded 2026-10-15, review m1: the box is no longer tightened from the measured union box.
    Check 4 now tests the rendered template against the declared box; see step 7.) Check 4 still
    **logs** the measured union box per template. If that box is much smaller than the declared one,
    note it in the PR body for the owner; it is information only.

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
  | 9 | `.ovl-slate-content` back to `inset: 0` + bar padding (ignores the card variables) | CSS card guard and e2e check 4 (bug cells) |
  | 10 | `cardRegionFor` ignores `box` (always the strip above a bar) | step 1.7, the extensibility card leg, e2e check 4 |

  Record all 10 in the commit body.

- [ ] **Step 14: Owner visual review.** Screenshots at 1280 of the gallery, at `frame=full`, `844`
  and `390`, cropped per template × sport. Post them in the PR comment, **never committed**
  (owner 10-09). Get a per-screen verdict on each (AGENTS.md class 11). The checks pass only if the
  images exist and differ between frames (class 10).

- [ ] **Step 14b: OBS compatibility (coordinator decision G1; spec §5 "Compatibility"; manual, every
  release).** OBS's browser source runs its own CEF, which isn't the compositor's, so the page is
  checked in it by hand, once per release that touches `components/overlay/**` or `globals.css`'s
  `.ovl-` rules, and once for this PR:
  - In OBS (record its version and the CEF version from its About box), add a 1920×1080 browser source
    with the panel's OBS URL for a stg fixture: `?template=bar` and then `?template=bug`. Score a goal
    and check that the slab shows, at the template's placement.
  - Add a source with a **scene URL**: the same fixture with `&host=compositor`, and run
    `harness/phase-probe-server.py --variant pna --script waiting:5000,reconnecting:5000,final:5000`
    from the compositor repo (C11 Step 0's file) on the same machine. The scenes cycle only if OBS's
    CEF allows the loopback call; if it doesn't, record that too, since OBS users never use
    `host=compositor`.
  - Check visually: text is legible at the 1920 canvas, the bar sits clear of the bottom, no card
    covers the template, and the background is transparent.
  - Record the result in the compositor `_FINDINGS.md` as `F-0xx: OBS browser source compatibility,
    <release>` (versions, URLs, verdict per check), or in the web release checklist if one exists
    (re-pin). Post the screenshots in the PR comment, never in the repo. C17's acceptance lists this
    record.

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
  `/admin/overlay-gallery` renders every cell at 1280; cards are laid out from `sceneCardSafeArea`
  and check 4 covers every card in every cell; the existing overlay e2e is green and unchanged apart
  from the new cases; smoke extended; 10 mutants red; owner verdicts per screen; the OBS
  compatibility record (step 14b) exists.
- PR B: merged before C07 vendors; the contract sha is in the PR body; 2 mutants red.
- The false premises and the resolved ambiguities (decisions 2–4, the card/bug answer) are in the PR
  bodies, and in compositor `docs/_INDEX.md` through the orchestrator.

## Do not touch

The stream panel's behaviour (R2006: the OBS URL stays `?style=` here), the scene layer and the phase
channel (R2005), relay usecases (R2004), `use-live-fixture.ts`, and C04/C05.

## Output cap

Under 15 lines: both PR URLs, test counts, the sweep counts, mutants, the contract sha, deviations.
