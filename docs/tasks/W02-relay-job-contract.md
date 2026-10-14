# W02: The `relay-job.v1` contract and the heartbeat extension

**Repo:** `onryde/seazn.club`. Worktree:
`git worktree add ../seazn.club-wt/w02-relay-contract -b feat/relay-job-contract origin/main`.
Use the `seazn-local-env` skill for setup.
**Depends on:** W01 merged.

## Context

Pinned 2026-10-14. **Re-pin before editing.**

- The internal GET `app/api/internal/relay/sessions/[sid]/route.ts` → `sessionFactsForJob`
  (`server/usecases/stream-sessions.ts:2180`) returns an **inline literal with no zod schema**:
  `{sessionId, fixtureId, mode, themeId, overlayDelayMs, maxDurationMinutes, target{url,streamKey}, pageToken}`
  (`:2185`).
- The heartbeat body is `RelayHeartbeat` (`server/api-v1/schemas.ts:1666`, strict). The reply is
  `RelayHeartbeatReply` (`:1686`) `{desiredState}`. The handler is `heartbeat`
  (`stream-sessions.ts:2123`).
- Contracts precedent:
  - `docs/contracts/` holds six `capture-*.json` files plus `fixtures/`.
  - They are generated from zod by `scripts/gen-capture-contracts.ts`.
  - The drift gate is a sha256 check in `server/api-v1/__tests__/capture-contract.test.ts:49`.
  - The capture app vendors the files byte for byte.
  - There is no relay contract yet.
- Tests to extend: `server/usecases/__tests__/relay-internal-routes.test.ts` and
  `server/api-v1/__tests__/stream-contract.test.ts`.

## Goal

The compositor (C07) needs a machine-checkable contract to vendor. This task delivers four things:

1. zod schemas for both directions
2. a generated `docs/contracts/relay-job.v1.json` plus fixtures
3. a drift test
4. heartbeat ingestion of the new optional fields, persisted through the real route

The composed **GET fields** (`input`, a composed `target`, `output`, `heartbeatIntervalMs`,
`overlayUrl`) are defined in the schema here but **served** by W04. W04 owns the data they come from.
Until W04 ships, a composed GET still returns today's shape. That's acceptable, because W01 keeps
composed mode unreachable.

## Schema

These go in `server/api-v1/schemas.ts`, next to `RelayHeartbeat`. Match the house zod style there.

```ts
export const RelayJobInput = z.object({
  protocol: z.literal("srt"),
  url: z.string(),            // srt://host:port
  streamId: z.string(),
  passphrase: z.string(),
}).strict();

export const RelayJobOutput = z.object({
  width: z.number().int().positive(),
  height: z.number().int().positive(),
  fps: z.number().int().positive(),
  videoKbps: z.number().int().positive(),
}).strict();

export const RelayJobSession = z.object({
  sessionId: z.string().uuid(),
  fixtureId: z.string().uuid(),
  mode: StreamMode,
  themeId: z.string().nullable(),          // keep today's nullability; re-pin
  overlayDelayMs: z.number().int().min(0),
  maxDurationMinutes: z.number().int().positive(),
  target: z.object({ url: z.string(), streamKey: z.string() }).strict(),
  pageToken: z.string(),
  // composed only (W04 serves them; absent for passthrough)
  input: RelayJobInput.optional(),
  output: RelayJobOutput.optional(),
  heartbeatIntervalMs: z.number().int().positive().optional(),
  overlayUrl: z.string().url().optional(),
}).strict();
```

Extend `RelayHeartbeat` with these **optional, nullable** fields. "Absent is not zero" stays the rule.

```ts
programmePhase: z.enum(["waiting", "live", "frozen", "reconnecting"]).nullish(),
input: z.enum(["connecting", "connected", "backoff"]).nullish(),
output: z.enum(["connecting", "connected", "backoff"]).nullish(),
graphics: z.enum(["starting", "ready", "restarting", "failed"]).nullish(),
inputKicks: z.number().int().min(0).nullish(),
inputKickAlert: z.boolean().nullish(),
pipelineRebuilds: z.number().int().min(0).nullish(),
sinkSwaps: z.number().int().min(0).nullish(),
rung: z.enum(["full", "overlay_half_rate", "fast_preset"]).nullish(),
scoreStale: z.boolean().nullish(),
```

The existing `state`, `fps`, `bitrateKbps`, `egressBytes`, `measuredLatencyMs`, `droppedFrames`,
`encoderSpeed`, `cpuPct` and `memMb` fields are unchanged.

## Constant

Add `RELAY_HEARTBEAT_INTERVAL_MS = 5_000` to `apps/web/src/server/relay/config.ts`, with a doc line:
"compositor spec §5.3; served as `heartbeatIntervalMs` (W04); W03's composed stale rule is 3× this".
The heartbeat schema doesn't use it, but W03 and W04 both import it, and **this task is the one place
it's defined**.

## Files

- Modify: `apps/web/src/server/relay/config.ts` (the one constant)
- Modify: `apps/web/src/server/api-v1/schemas.ts`
- Modify: `apps/web/src/server/usecases/stream-sessions.ts`. `sessionFactsForJob` returns
  `RelayJobSession`-typed data and validates with `.parse` in tests. `heartbeat` persists the new
  fields.
- Create: `scripts/gen-relay-contracts.ts`, modelled on `gen-capture-contracts.ts`. Add a matching
  `package.json` script **only if** the capture one has one. It doesn't, per the pin, so mirror that.
- Create: `docs/contracts/relay-job.v1.json` (both schemas, under `$defs`: `session`, `heartbeat`,
  `heartbeatReply`)
- Create: these fixtures in `docs/contracts/fixtures/relay-job.v1/`:
  - `session-passthrough.json`
  - `session-composed-720p.json`
  - `session-composed-1080p.json`
  - `heartbeat-minimal.json` (state only)
  - `heartbeat-full.json`
  - `heartbeat-zero-is-a-measurement.json`: a beat with `fps: 0`. It is valid under the schema, and
    it documents that a present 0 means "measured zero", never "unmeasured". The fixtures dir
    `README.md` states the convention.
  - `heartbeat-invalid-unknown-field.json` (must fail, because the schema is strict)
  - `reply-live.json`
  - `reply-ending.json`
- Create: `apps/web/src/server/api-v1/__tests__/relay-contract.test.ts`, modelled on
  `capture-contract.test.ts`. It checks:
  - the generated JSON equals the committed file (sha256)
  - every `valid-*`/named fixture parses
  - every `invalid-*` fixture is rejected
- Modify: `relay-internal-routes.test.ts`

## Persistence of the new heartbeat fields

Re-pin how `heartbeat` (`:2123`) persists today: `fixture_stream_samples` columns, raw jsonb, or
something else. **Follow the existing pattern.**

- If samples keep a raw jsonb of the body, the new fields ride in it, and you only need to assert
  they land there.
- If samples are column-per-field, add nullable columns in **one** migration delta. Take the next free
  `V` number at build time. Never pin it in a doc.

**Named consumer** (an inert-seam guard): C17's post-match health review query reads these fields. Put
that query in `docs/runbooks/relay-health-review.sql` in this repo, written against the real storage.
C17 runs it.

## Steps

- [ ] **Step 1: Failing contract test.** Write `relay-contract.test.ts` against fixtures that don't
  exist yet. Run it scoped with `--reporter=json`, and expect a fail.
- [ ] **Step 2: Add the schemas, then the generator.** Run
  `pnpm tsx scripts/gen-relay-contracts.ts` (mirror how the capture generator is invoked) to write
  the JSON. Hand-write the fixtures. Never generate fixtures from the code under test.
- [ ] **Step 3: Run the contract test and get a pass.** Paste the counts.
- [ ] **Step 4: Failing route test.** In `relay-internal-routes.test.ts`:
  - POST a `heartbeat-full.json` body through the **real route handler** with a valid job token, then
    read the stored row and assert that each new field round-trips.
  - POST `heartbeat-minimal.json` and assert the new fields are stored as null, **not 0**.
  - POST the unknown-field fixture and assert a 400.
  - Positive pair: the existing minimal beat still returns 200 `{desiredState}`.
- [ ] **Step 5: Implement the persistence.** Run the tests and get a pass.
- [ ] **Step 6: Strict session test.** In the same file, GET the session for a passthrough fixture,
  and assert `RelayJobSession.parse(body)` succeeds and that no composed-only key is present.
- [ ] **Step 7: Mutate once.** Drop `.strict()` from `RelayHeartbeat`. The unknown-field case must go
  red. Revert, and note it in the commit body.
- [ ] **Step 8: Scoped gate.** Run the files above plus `stream-contract.test.ts` with the JSON
  reporter. Lint with `rtk proxy` and read `✖ N`. Run the typecheck.
- [ ] **Step 9: Commit, push and open the PR.**
  `feat(relay): relay-job.v1 contract, strict session schema and the extended heartbeat`.
  The PR body gives the web commit sha that C07 will pin.

## Acceptance

- The contract JSON plus fixtures are committed, and the drift test is green.
- The new heartbeat fields round-trip through the real route. Absent fields are stored as null.
- The mutant went red.
- The PR is open, with the sha noted for C07.

## Do not touch

The composed provisioning (W04), the runner (W03), and the UI.

## Output cap

Under 15 lines: the PR URL, the contract sha256, test counts, and deviations.
